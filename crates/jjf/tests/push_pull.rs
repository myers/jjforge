//! Integration tests for `jjf push <remote>` and `jjf pull <remote>` —
//! drive the compiled binary against per-test scratch jj-clones-of-a-
//! bare-git-remote and assert exit code, stdout shape (plain +
//! `--json`), single-clone round-trip, two-clone divergence (same-
//! field LWW and different-fields union), unknown remote / empty
//! remote / non-jj / missing-bookmark preflight outcomes.
//!
//! All tests are hermetic: per-test scratch root under
//! `tests/.scratch/`, gitignored. Each spins up its own bare git repo
//! as the "remote" and one or two jj clones; the `jjf` binary lives
//! at `env!("CARGO_BIN_EXE_jjf")` (no `assert_cmd` dep).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const JJF_BIN: &str = env!("CARGO_BIN_EXE_jjf");

/// Per-test scratch root. Gitignored via the workspace-level rule.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(".scratch")
        .join(format!("push_pull_{name}"));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(&dir).unwrap()
}

fn run_jjf(cwd: &Path, args: &[&str]) -> Output {
    Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn jjf")
}

fn run_jj(cwd: &Path, args: &[&str]) -> Output {
    Command::new("jj")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn jj")
}

fn must_succeed(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed: code={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Stand up a bare git repo at `<root>/remote.git` and (optionally)
/// one jj clone per name in `clones`. Returns the root.
///
/// jj 0.40's `jj git clone` requires the source URL be reachable; we
/// pass an absolute filesystem path so this works fully offline.
fn setup(name: &str, clones: &[&str]) -> PathBuf {
    let root = scratch(name);
    let remote = root.join("remote.git");
    let init = Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(&remote)
        .output()
        .expect("git init");
    must_succeed(&init, "git init --bare");
    for clone in clones {
        let dest = root.join(clone);
        let clone_out = Command::new("jj")
            .arg("git")
            .arg("clone")
            .arg(remote.to_str().unwrap())
            .arg(&dest)
            .output()
            .expect("jj git clone");
        must_succeed(&clone_out, &format!("jj git clone {clone}"));
        // Configure a per-clone identity so commits don't all collide
        // on the default `Tester <t@t.com>` author.
        let cfg_name = Command::new("jj")
            .args(["config", "set", "--repo", "user.name", clone])
            .current_dir(&dest)
            .output()
            .expect("jj config user.name");
        must_succeed(&cfg_name, "jj config user.name");
        let cfg_mail = Command::new("jj")
            .args(["config", "set", "--repo", "user.email", &format!("{clone}@example.com")])
            .current_dir(&dest)
            .output()
            .expect("jj config user.email");
        must_succeed(&cfg_mail, "jj config user.email");
    }
    root
}

// ---------------------------------------------------------------
// Happy-path round-trip
// ---------------------------------------------------------------

#[test]
fn push_pull_single_clone_round_trip() {
    let root = setup("single_round_trip", &["alice", "bob"]);
    let alice = root.join("alice");
    let bob = root.join("bob");

    // Alice inits + creates + pushes.
    must_succeed(&run_jjf(&alice, &["init"]), "jjf init (alice)");
    let new_out = run_jjf(&alice, &["new", "-t", "shared title"]);
    must_succeed(&new_out, "jjf new (alice)");
    let id = String::from_utf8_lossy(&new_out.stdout).trim().to_owned();
    assert_eq!(id.len(), 7, "id must be 7 hex chars: {id:?}");
    let push = run_jjf(&alice, &["push", "origin"]);
    must_succeed(&push, "jjf push origin (alice)");
    let push_stdout = String::from_utf8_lossy(&push.stdout);
    assert_eq!(push_stdout.trim(), "pushed issues -> origin");

    // Bob pulls (fresh clone with no local issues bookmark).
    let pull = run_jjf(&bob, &["pull", "origin"]);
    must_succeed(&pull, "jjf pull origin (bob)");
    let pull_stdout = String::from_utf8_lossy(&pull.stdout);
    assert_eq!(
        pull_stdout.trim(),
        "pulled issues <- origin",
        "first pull should report clean fetch (no merge driver)",
    );

    // Bob can show alice's bug.
    let show = run_jjf(&bob, &["show", &id]);
    must_succeed(&show, "jjf show (bob)");
    let show_stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        show_stdout.contains("shared title"),
        "bob should see alice's title; got:\n{show_stdout}"
    );
}

#[test]
fn push_json_envelope_shape() {
    let root = setup("push_json", &["alice"]);
    let alice = root.join("alice");
    must_succeed(&run_jjf(&alice, &["init"]), "init");
    must_succeed(&run_jjf(&alice, &["new", "-t", "x"]), "new");
    let out = run_jjf(&alice, &["--json", "push", "origin"]);
    must_succeed(&out, "push --json");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("push --json must be valid JSON");
    assert_eq!(v["ok"].as_bool(), Some(true));
    assert_eq!(v["remote"].as_str(), Some("origin"));
    assert_eq!(v["bookmark"].as_str(), Some("issues"));
}

#[test]
fn pull_json_envelope_with_no_remote_bookmark_yet() {
    // Alice clones a fresh bare remote and pulls before pushing —
    // remote has no `issues` bookmark yet, exit 0, `remote_present:
    // false`.
    let root = setup("pull_empty", &["alice"]);
    let alice = root.join("alice");
    let out = run_jjf(&alice, &["--json", "pull", "origin"]);
    must_succeed(&out, "pull --json (empty remote)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("pull --json must be valid JSON");
    assert_eq!(v["ok"].as_bool(), Some(true));
    assert_eq!(v["remote"].as_str(), Some("origin"));
    assert_eq!(v["bookmark"].as_str(), Some("issues"));
    assert_eq!(
        v["remote_present"].as_bool(),
        Some(false),
        "remote_present must be false when remote has no issues bookmark; got: {stdout}"
    );
    assert_eq!(v["resolved_issues"].as_i64(), Some(0));
    assert_eq!(v["merge_strategy"].as_str(), Some("op_space"));

    // Plain-text variant.
    let out = run_jjf(&alice, &["pull", "origin"]);
    must_succeed(&out, "pull (empty remote)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no issues bookmark on remote yet"),
        "plain stdout should mention empty remote; got: {stdout}"
    );
}

// ---------------------------------------------------------------
// Two-clone divergence
// ---------------------------------------------------------------

/// Same-field divergence (both edit `title`). The op-space resolver
/// applies LWW by the spec §6 ordering tuple — `(jjf_at, commit,
/// trailer_index)` — to pick a single winner. The two clones' edits
/// land at different `now_rfc3339_nanos()` instants (the writer's
/// op-time stamp), so the later wall-clock instant wins. We don't pin
/// which clone wins (clock skew + test scheduling makes that flaky to
/// assert on); we pin that exactly one of the two values wins and the
/// bookmark converges.
#[test]
fn pull_two_clones_same_field_lww_converges() {
    let root = setup("two_same_field", &["alice", "bob"]);
    let alice = root.join("alice");
    let bob = root.join("bob");

    must_succeed(&run_jjf(&alice, &["init"]), "init alice");
    let new_out = run_jjf(&alice, &["new", "-t", "shared"]);
    must_succeed(&new_out, "new (alice)");
    let id = String::from_utf8_lossy(&new_out.stdout).trim().to_owned();
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "push (alice)");
    must_succeed(&run_jjf(&bob, &["pull", "origin"]), "first pull (bob)");

    // Concurrent edits: both retitle.
    must_succeed(
        &run_jjf(&alice, &["update", &id, "--title", "alice title"]),
        "alice update",
    );
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "alice push");
    must_succeed(
        &run_jjf(&bob, &["update", &id, "--title", "bob title"]),
        "bob update",
    );

    // Pull triggers the op-space resolver pass.
    let pull = run_jjf(&bob, &["pull", "origin"]);
    must_succeed(&pull, "bob pull (divergent)");
    let pull_stdout = String::from_utf8_lossy(&pull.stdout);
    assert!(
        pull_stdout.contains("resolved 1 issue"),
        "pull stdout should mention resolved issue count; got: {pull_stdout}"
    );

    // Verify bob's view post-merge: one of the two titles wins; never
    // the original "shared" value.
    let show = run_jjf(&bob, &["show", &id]);
    must_succeed(&show, "bob show");
    let s = String::from_utf8_lossy(&show.stdout);
    let won_alice = s.contains("alice title");
    let won_bob = s.contains("bob title");
    assert!(
        won_alice ^ won_bob,
        "exactly one title should win post-merge; got both/neither in:\n{s}"
    );
    assert!(
        !s.contains("shared\n") && !s.contains("\nshared"),
        "the pre-divergence title should not survive; got:\n{s}"
    );
}

/// Different-fields divergence: alice adds a label, bob adds a
/// different label. Both should survive the merge — under the
/// op-space resolver, each `label-add` op is applied in causal order
/// across the merged op stream (spec §6); two disjoint adds compose
/// to a final state with both labels present. Same outcome shape as
/// the v1 file-bytes driver's set-union approximation, but reached
/// through the principled "replay-by-op" path rather than a
/// JSON-set-union policy hack.
#[test]
fn pull_two_clones_different_fields_both_survive() {
    let root = setup("two_diff_fields", &["alice", "bob"]);
    let alice = root.join("alice");
    let bob = root.join("bob");

    must_succeed(&run_jjf(&alice, &["init"]), "init alice");
    let new_out = run_jjf(&alice, &["new", "-t", "shared"]);
    must_succeed(&new_out, "new (alice)");
    let id = String::from_utf8_lossy(&new_out.stdout).trim().to_owned();
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "alice push");
    must_succeed(&run_jjf(&bob, &["pull", "origin"]), "bob first pull");

    must_succeed(
        &run_jjf(&alice, &["label", "add", &id, "alice-label"]),
        "alice add label",
    );
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "alice push 2");
    must_succeed(
        &run_jjf(&bob, &["label", "add", &id, "bob-label"]),
        "bob add label",
    );
    must_succeed(&run_jjf(&bob, &["pull", "origin"]), "bob pull");

    let show = run_jjf(&bob, &["show", &id]);
    must_succeed(&show, "bob show");
    let s = String::from_utf8_lossy(&show.stdout);
    assert!(
        s.contains("alice-label"),
        "alice's label should survive (set-union); got:\n{s}"
    );
    assert!(
        s.contains("bob-label"),
        "bob's label should survive (set-union); got:\n{s}"
    );
}

// ---------------------------------------------------------------
// Preflight / error matrix
// ---------------------------------------------------------------

#[test]
fn push_unknown_remote_exits_two_with_remote_not_found_kind() {
    let root = setup("push_unknown", &["alice"]);
    let alice = root.join("alice");
    must_succeed(&run_jjf(&alice, &["init"]), "init");
    must_succeed(&run_jjf(&alice, &["new", "-t", "x"]), "new");
    let out = run_jjf(&alice, &["--json", "push", "nope"]);
    assert!(!out.status.success(), "push to unknown remote should fail");
    assert_eq!(
        out.status.code(),
        Some(2),
        "unknown remote must exit 2 (preflight); stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("stderr should be JSON envelope");
    assert_eq!(v["ok"], serde_json::Value::Bool(false));
    assert_eq!(v["error"]["kind"].as_str(), Some("remote_not_found"));
    assert_eq!(v["error"]["details"]["name"].as_str(), Some("nope"));
}

#[test]
fn pull_unknown_remote_exits_two() {
    let root = setup("pull_unknown", &["alice"]);
    let alice = root.join("alice");
    let out = run_jjf(&alice, &["pull", "nope"]);
    assert!(!out.status.success(), "pull from unknown remote should fail");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn push_outside_jj_repo_exits_two_not_a_jj_repo() {
    let dir = scratch("push_non_jj");
    let out = run_jjf(&dir, &["push", "origin"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a jj repo"),
        "stderr should mention not a jj repo; got: {stderr}"
    );
}

#[test]
fn pull_outside_jj_repo_exits_two_not_a_jj_repo() {
    let dir = scratch("pull_non_jj");
    let out = run_jjf(&dir, &["pull", "origin"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a jj repo"),
        "stderr should mention not a jj repo; got: {stderr}"
    );
}

#[test]
fn push_without_bugs_bookmark_exits_two_missing_bookmark() {
    // jj repo exists but `jjf init` was never run — local `issues` is
    // absent. `push` requires the bookmark.
    let root = setup("push_no_bookmark", &["alice"]);
    let alice = root.join("alice");
    let out = run_jjf(&alice, &["push", "origin"]);
    assert!(!out.status.success(), "push without bookmark should fail");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("issues"),
        "stderr should mention issues bookmark; got: {stderr}"
    );

    // JSON envelope variant.
    let out = run_jjf(&alice, &["--json", "push", "origin"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("stderr must be JSON envelope");
    assert_eq!(v["error"]["kind"].as_str(), Some("missing_issues_bookmark"));
}

#[test]
fn pull_help_lists_remote_positional() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
        .args(["pull", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn pull --help");
    must_succeed(&out, "pull --help");
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("<REMOTE>"),
        "help should document <REMOTE>: {help}"
    );
}

#[test]
fn push_help_lists_remote_positional() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
        .args(["push", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn push --help");
    must_succeed(&out, "push --help");
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("<REMOTE>"),
        "help should document <REMOTE>: {help}"
    );
}

/// `--json` push to a remote without the issues bookmark locally: error
/// envelope with `missing_issues_bookmark`. Sister test of
/// `push_without_bugs_bookmark_exits_two_missing_bookmark` but exposing
/// the json shape contract.
#[test]
fn pull_first_time_tracks_and_materializes_local_bugs() {
    // Verifies the `track` step in `pull`'s flow on a fresh second
    // clone: bob's `issues@origin` exists post-fetch but local `issues`
    // is untracked until we explicitly track it.
    //
    // Test sequence:
    //   1. Alice inits + pushes (creates `bugs` on the bare remote).
    //   2. Bob was cloned BEFORE alice pushed, so bob has no
    //      `issues@origin` yet.
    //   3. Bob pulls — under the hood: fetch (now sees issues@origin),
    //      track (materializes local issues), no divergence (clean).
    //   4. Local `bugs` should be present in bob's bookmark list.
    let root = setup("round_trip_track", &["alice", "bob"]);
    let alice = root.join("alice");
    let bob = root.join("bob");
    must_succeed(&run_jjf(&alice, &["init"]), "init alice");
    must_succeed(&run_jjf(&alice, &["new", "-t", "from alice"]), "new alice");
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "push alice");

    // Pull — fetches, then tracks issues@origin so local `issues` appears.
    must_succeed(&run_jjf(&bob, &["pull", "origin"]), "bob pull");

    let bm = run_jj(&bob, &["bookmark", "list", "--all-remotes"]);
    let bm_stdout = String::from_utf8_lossy(&bm.stdout);
    // After tracking, local `issues` should appear (the local-only line
    // doesn't have an `@` segment — that's how we distinguish it from
    // `issues@origin`).
    let has_local_issues = bm_stdout
        .lines()
        .any(|line| line.trim_start().starts_with("issues:") || line.trim_start().starts_with("issues "));
    assert!(
        has_local_issues,
        "after pull, local `issues` should exist; got:\n{bm_stdout}"
    );
}
