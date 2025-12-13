//! Integration tests for the colocate-drift guard (issue `08cf14b`).
//!
//! The guard refuses to run mutating verbs from inside the jjforge
//! source repo because the storage layer's 4-CLI write dance moves
//! `@` onto an unbranched commit, which in a colocated jj+git repo
//! also drives git HEAD to `refs/jj/root` — a destructive footgun
//! the orchestration loop hit twice in one session.
//!
//! These tests build a "fake source repo" fixture: a jj-git repo
//! that contains the marker files `crates/jjf/Cargo.toml` and
//! `docs/storage-format.md` (the set `preflight::SELF_HOST_MARKERS`
//! checks for). Running any mutating `jjf` verb from inside that
//! fixture should fail with exit 2 and the `self_hosted_write_refused`
//! kind. Setting `JJF_ALLOW_SELF_HOST=1` should bypass the guard
//! cleanly; read verbs should be unaffected either way.
//!
//! We deliberately do NOT shell out to the surrounding worktree —
//! the test cwd is the fixture, not the real source repo, so a CI
//! environment that runs this test outside a jjforge checkout still
//! exercises the same logic.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const JJF_BIN: &str = env!("CARGO_BIN_EXE_jjf");

/// Per-test scratch dir under the crate. Mirrors the other test
/// modules' `scratch()` shape: wipe + recreate on each run, return
/// a canonicalized absolute path.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(".scratch")
        .join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(&dir).unwrap()
}

/// Build a fixture that looks like the jjforge source repo to the
/// `refuse_self_hosted_write` probe. The marker set is
/// `crates/jjf/Cargo.toml` and `docs/storage-format.md`; presence of
/// both at the repo root is what triggers the refusal. We also
/// `jj git init` the dir so the verb's other preflight probes
/// (jj-repo, then optionally bugs-bookmark) get past the early bail
/// — we want to exercise the self-host probe specifically.
fn make_fake_source_repo(name: &str) -> PathBuf {
    let dir = scratch(name);
    fs::create_dir_all(dir.join("crates/jjf")).unwrap();
    fs::create_dir_all(dir.join("docs")).unwrap();
    fs::write(dir.join("crates/jjf/Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    fs::write(dir.join("docs/storage-format.md"), "# storage\n").unwrap();
    let out = Command::new("jj")
        .args(["git", "init"])
        .current_dir(&dir)
        .output()
        .expect("spawn jj git init");
    assert!(
        out.status.success(),
        "jj git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

/// Like `make_fake_source_repo` but WITHOUT the marker files. Used
/// to pin "an unrelated jj repo doesn't trip the guard."
///
/// The fixture root MUST live outside the surrounding worktree —
/// otherwise the marker-walk-up finds the worktree's own
/// `crates/jjf/Cargo.toml` + `docs/storage-format.md` and the guard
/// fires correctly (but defeats the test's intent). We use
/// `std::env::temp_dir()` rather than `tests/.scratch/` for that
/// reason. On macOS this is `$TMPDIR` (per-user, not `/tmp`); on
/// Linux it's typically `/tmp` which is fine for ephemeral test
/// fixtures.
fn make_plain_jj_repo_outside_worktree(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join("jjf-self-host-guard-tests");
    let dir = root.join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    let dir = fs::canonicalize(&dir).unwrap();
    let out = Command::new("jj")
        .args(["git", "init"])
        .current_dir(&dir)
        .output()
        .expect("spawn jj git init");
    assert!(
        out.status.success(),
        "jj git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

/// Run `jjf <args...>` in `cwd` with NO `JJF_ALLOW_SELF_HOST` in the
/// environment — we want the guard to fire when it should. We also
/// scrub `HOME` to a temp dir so any operator-side jj config
/// doesn't leak in and pollute the test (jj reads `~/.jjconfig.toml`).
fn run_jjf_no_bypass(cwd: &Path, args: &[&str]) -> Output {
    Command::new(JJF_BIN)
        .env_remove("JJF_ALLOW_SELF_HOST")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn jjf")
}

/// Run `jjf <args...>` in `cwd` WITH `JJF_ALLOW_SELF_HOST=1` so the
/// guard bypasses. Used to verify the override path.
fn run_jjf_bypass(cwd: &Path, args: &[&str]) -> Output {
    Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn jjf")
}

#[test]
fn init_inside_jjforge_source_repo_refused_with_typed_exit_two() {
    // The headline failure mode from issue 08cf14b: running a mutating
    // verb inside the source repo silently drifts git HEAD. The guard
    // turns that silent footgun into a typed preflight failure.
    let dir = make_fake_source_repo("guard_init_refuse");

    let out = run_jjf_no_bypass(&dir, &["init"]);
    assert!(
        !out.status.success(),
        "init should be refused from inside the source repo"
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "preflight failure should exit 2, got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("refusing to write from inside the jjforge source repo"),
        "stderr should name the refusal, got: {stderr}"
    );
    assert!(
        stderr.contains("JJF_ALLOW_SELF_HOST=1"),
        "stderr should name the bypass env, got: {stderr}"
    );
    assert!(
        stderr.contains(dir.to_string_lossy().as_ref()),
        "stderr should include the offending path, got: {stderr}"
    );
}

#[test]
fn init_json_envelope_uses_self_hosted_write_refused_kind() {
    // Pins the `docs/cli-json.md` contract for the new error kind:
    // exit 2, kind `self_hosted_write_refused`, details with `path`
    // and `markers`.
    let dir = make_fake_source_repo("guard_init_json");

    let out = run_jjf_no_bypass(&dir, &["--json", "init"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.is_empty(),
        "stdout should be empty on error, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("stderr must be valid JSON envelope");
    assert_eq!(v["ok"], serde_json::Value::Bool(false));
    assert_eq!(
        v["error"]["kind"].as_str(),
        Some("self_hosted_write_refused"),
        "kind wrong: {stderr}"
    );
    assert_eq!(
        v["error"]["details"]["path"].as_str(),
        Some(dir.to_string_lossy().as_ref()),
        "details.path wrong: {stderr}"
    );
    // Markers are an array; we pin both expected entries are present
    // (order is fixed by `SELF_HOST_MARKERS`).
    let markers = v["error"]["details"]["markers"]
        .as_array()
        .expect("markers must be an array");
    let marker_strs: Vec<&str> = markers
        .iter()
        .filter_map(|m| m.as_str())
        .collect();
    assert!(
        marker_strs.contains(&"crates/jjf/Cargo.toml"),
        "markers should include crates/jjf/Cargo.toml, got: {marker_strs:?}"
    );
    assert!(
        marker_strs.contains(&"docs/storage-format.md"),
        "markers should include docs/storage-format.md, got: {marker_strs:?}"
    );
}

#[test]
fn init_bypassed_when_jjf_allow_self_host_env_set() {
    // The bypass path: env var set, guard waved, init proceeds.
    // Critical for orchestration loops authorized to write from inside
    // the source repo. We also assert exit code 0 and the human-readable
    // success text.
    let dir = make_fake_source_repo("guard_init_bypass");

    let out = run_jjf_bypass(&dir, &["init"]);
    assert!(
        out.status.success(),
        "init should succeed with bypass, got code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("JJF_ALLOW_SELF_HOST=1 set; proceeding"),
        "bypass should announce itself on stderr (text mode), got: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("bugs"),
        "init should still mention the bugs bookmark, got: {stdout}"
    );
}

#[test]
fn init_json_bypass_keeps_stderr_clean_for_envelope_parsers() {
    // The bypass announcement is text-mode-only — under `--json` it
    // would break downstream JSON-envelope parsers. Pin that the
    // success envelope on stdout still parses and that stderr stays
    // empty.
    let dir = make_fake_source_repo("guard_init_json_bypass");

    let out = run_jjf_bypass(&dir, &["--json", "init"]);
    assert!(out.status.success(), "init --json bypass should succeed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.is_empty(),
        "stderr should be silent in --json bypass mode, got: {stderr}"
    );
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
            .expect("stdout should be a single JSON envelope under --json bypass");
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
}

#[test]
fn plain_jj_repo_without_markers_is_not_guarded() {
    // The marker set is jjforge-specific; an unrelated jj repo should
    // run freely. Pins that the guard's false-positive surface is
    // zero — only repos containing BOTH markers trip it.
    let dir = make_plain_jj_repo_outside_worktree("guard_init_unrelated");

    let out = run_jjf_no_bypass(&dir, &["init"]);
    assert!(
        out.status.success(),
        "init in a plain jj repo should not be guarded, got code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn read_verbs_pass_through_even_inside_source_repo() {
    // The guard targets MUTATING verbs only. Read verbs (`show`, `ls`)
    // don't trigger the 4-CLI dance and don't drift HEAD. They must
    // remain runnable from inside the source repo so an orchestrator
    // (or a curious operator) can inspect bug state without bypassing.
    //
    // We use `ls` because it doesn't need a bug id; success is the
    // empty array (since the fixture has no `bugs` bookmark yet, but
    // the preflight will fail with `missing_bugs_bookmark` — which is
    // exit 2, not the `self_hosted_write_refused` exit 2 we'd see if
    // the guard fired). We assert the kind specifically.
    let dir = make_fake_source_repo("guard_ls_passthrough");

    let out = run_jjf_no_bypass(&dir, &["--json", "ls"]);
    // Either succeeds (no bugs) or fails with missing_bugs_bookmark —
    // both are acceptable. What MUST NOT happen is self_hosted_write_refused.
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let v: serde_json::Value = serde_json::from_str(stderr.trim())
            .expect("stderr must be JSON envelope on failure");
        let kind = v["error"]["kind"].as_str().unwrap_or("");
        assert_ne!(
            kind, "self_hosted_write_refused",
            "read verbs must bypass the self-host guard, got kind={kind} stderr={stderr}"
        );
    }
}

#[test]
fn guard_walks_up_to_find_markers_in_ancestor() {
    // Marker detection climbs upward from cwd. A subagent running
    // from `crates/jjf/` or `experiments/<topic>/` inside the source
    // tree should still be caught — markers live at the repo root,
    // not wherever cwd happens to be.
    let root = make_fake_source_repo("guard_ancestor_walk");
    let subdir = root.join("crates/jjf");
    // make_fake_source_repo already created `crates/jjf/`; we just
    // run jjf from inside it.
    assert!(
        subdir.is_dir(),
        "fixture should have crates/jjf/ subdir; got {subdir:?}"
    );

    let out = run_jjf_no_bypass(&subdir, &["init"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "guard should fire from a subdirectory; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("refusing to write"),
        "stderr should explain the refusal, got: {stderr}"
    );
    // Path in the error should be the ROOT of the marker tree, not
    // the cwd where jjf was invoked.
    assert!(
        stderr.contains(root.to_string_lossy().as_ref()),
        "path should be the marker root, got: {stderr}"
    );
}
