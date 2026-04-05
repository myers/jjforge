//! Integration tests for `jjf init` — drive the compiled binary
//! against per-test scratch repos and assert exit code, stderr,
//! stdout (including `--json` shape), and observable repo state.
//!
//! Mirrors the hermetic-scratch style of `jjf-storage`'s
//! `tests/integration.rs`: per-test directory under `tests/.scratch/`,
//! wiped on each run, gitignored via `crates/**/tests/.scratch/`.
//!
//! We deliberately do NOT take a dep on `assert_cmd` here — locating
//! the binary via `CARGO_BIN_EXE_jjf` (cargo sets it for any test
//! target in the same package as the `[[bin]]`) plus `std::process`
//! is enough for what we need, and matches the rest of the
//! workspace's "narrow dep list" discipline.

use std::path::Path;
use std::process::Command;

mod common;
use common::*;

/// Convenience: check whether the v2 `issues` git branch exists.
/// Used to assert that v3 `jjf init` does NOT create the old v2
/// bookmark. Implemented via pure git (no jj — J7).
fn bugs_bookmark_present(repo: &Path) -> bool {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "refs/heads/issues"])
        .current_dir(repo)
        .output()
        .expect("spawn git rev-parse");
    out.status.success()
}

/// Convenience: does `refs/jjf/meta/format-version` resolve? Used by
/// the v3-init tests to assert the sentinel ref was planted.
fn v3_sentinel_present(repo: &Path) -> bool {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "refs/jjf/meta/format-version"])
        .current_dir(repo)
        .output()
        .expect("spawn git rev-parse");
    out.status.success()
}

#[test]
fn init_on_fresh_jj_repo_succeeds_and_plants_v3_sentinel() {
    let repo = make_jj_repo("init_fresh");
    assert!(
        !bugs_bookmark_present(&repo),
        "precondition: issues bookmark must not exist before init"
    );
    assert!(
        !v3_sentinel_present(&repo),
        "precondition: v3 sentinel must not exist before init"
    );

    let out = run_jjf(&repo, &["init"]);
    assert!(
        out.status.success(),
        "jjf init failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("initialized"),
        "expected human-readable output to mention `initialized`, got: {stdout}"
    );

    assert!(
        v3_sentinel_present(&repo),
        "init should plant the v3 sentinel ref"
    );
    assert!(
        !bugs_bookmark_present(&repo),
        "v3 init must NOT create the v2 issues bookmark"
    );
}

#[test]
fn init_in_non_jj_directory_fails_with_exit_two_and_useful_stderr() {
    let dir = scratch_non_git("init_non_jj");

    let out = run_jjf(&dir, &["init"]);
    assert!(!out.status.success(), "init should fail outside a jj repo");
    assert_eq!(
        out.status.code(),
        Some(2),
        "preflight failure should exit 2, got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The exact phrasing comes from `StorageError::NotAJjRepo`'s
    // Display impl ("not a jj repo: <path>"). We assert on both the
    // tag and the path so a future error-message rewording can't
    // silently strip context.
    assert!(
        stderr.contains("not a jj repo"),
        "stderr should explain the failure, got: {stderr}"
    );
    assert!(
        stderr.contains(dir.to_string_lossy().as_ref()),
        "stderr should include the offending path, got: {stderr}"
    );
}

#[test]
fn init_is_idempotent_when_run_twice() {
    let repo = make_jj_repo("init_idempotent");

    let first = run_jjf(&repo, &["init"]);
    assert!(
        first.status.success(),
        "first init failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(v3_sentinel_present(&repo), "first init should plant the v3 sentinel");

    let second = run_jjf(&repo, &["init"]);
    assert!(
        second.status.success(),
        "second init failed (should be no-op success): {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        v3_sentinel_present(&repo),
        "sentinel should still be present after second init"
    );
    assert!(
        !bugs_bookmark_present(&repo),
        "v3 init must not create a bookmark, even on the second run"
    );
}

#[test]
fn init_json_emits_expected_object() {
    let repo = make_jj_repo("init_json");

    let out = run_jjf(&repo, &["init", "--json"]);
    assert!(
        out.status.success(),
        "init --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Parse and assert structurally, not on byte equality — the
    // ticket pins the object shape, not the serializer's whitespace.
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("init --json output should be valid JSON");
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
    assert_eq!(v["bookmark"], serde_json::Value::String("issues".into()));
}

#[test]
fn init_json_error_envelope_on_non_jj_directory() {
    // `--json` plus a failing path: error must surface on stderr as the
    // documented error envelope, not the plain `jjf: <text>` line. Pins
    // the contract in `docs/cli-json.md` for the `not_a_jj_repo` kind.
    let dir = scratch_non_git("init_json_err_non_jj");

    let out = run_jjf(&dir, &["--json", "init"]);
    assert!(!out.status.success(), "init should fail outside a jj repo");
    assert_eq!(
        out.status.code(),
        Some(2),
        "preflight failure should exit 2, got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    // Stdout should be empty — errors render to stderr, not stdout.
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
        Some("not_a_jj_repo"),
        "kind wrong: {stderr}"
    );
    assert!(
        v["error"]["message"].as_str().is_some_and(|m| !m.is_empty()),
        "message missing/empty: {stderr}"
    );
    assert_eq!(
        v["error"]["details"]["path"].as_str(),
        Some(dir.to_string_lossy().as_ref()),
        "details.path wrong: {stderr}"
    );
}

#[test]
fn global_json_flag_works_before_subcommand_too() {
    // clap's `global = true` lets the flag sit on either side of the
    // subcommand. We assert both shapes so the surface stays stable
    // regardless of how a caller writes the invocation.
    let repo = make_jj_repo("init_json_before");
    let out = run_jjf(&repo, &["--json", "init"]);
    assert!(
        out.status.success(),
        "--json init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
            .expect("output should be valid JSON");
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
}

#[test]
fn verb_on_uninitialized_repo_rejects_without_jj() {
    // Plain git repo — NO `jj git init`, NO `jjf init`, no
    // refs/jjf/meta/format-version sentinel. After J3 the preflight
    // detects the missing sentinel purely via git-plumbing and refuses
    // with MissingIssuesBookmark WITHOUT spawning jj at all. Using a
    // plain git repo (not jj-colocated) ensures this test could NOT
    // have passed before J3 via the old `jj bookmark list` fallback
    // (jj would have refused the non-jj dir), so it actually gates the
    // v3-sentinel-only refusal path introduced by J3.
    let repo = scratch("verb_uninitialized");
    Command::new("git")
        .arg("init")
        .arg(&repo)
        .output()
        .expect("git init");

    // ls must fail — no sentinel, no init. This is the J3 gate: the
    // refusal must come purely from the git-plumbing sentinel check
    // (no jj subprocess). Other init tests cover the init-then-success
    // path on jj-colocated repos; this test pins the refusal on a
    // substrate where the pre-J3 `jj bookmark list` fallback would
    // also have failed, proving J3's sentinel-only path is what fires.
    let out = run_jjf(&repo, &["ls"]);
    assert!(
        !out.status.success(),
        "ls should refuse on an uninitialized plain-git repo; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("jjf init"),
        "error message should mention `jjf init`; got: {stderr}"
    );
}

#[test]
fn help_lists_every_epic_verb() {
    // Run from the crate's manifest dir; `--help` doesn't touch the
    // filesystem so the cwd doesn't matter, but using a stable path
    // keeps the test reproducible.
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
        .arg("--help")
        .current_dir(cwd)
        .output()
        .expect("spawn jjf --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);

    // The verbs the epic body (`c4f7fcb`) lists, plus `init` (the
    // one this ticket actually implements). If a verb is ever
    // renamed or dropped, this test catches it.
    for verb in ["init", "new", "show", "ls", "update", "comment", "close", "open", "label"] {
        assert!(
            help.contains(verb),
            "--help missing verb `{verb}`. Full help:\n{help}"
        );
    }
}
