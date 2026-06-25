//! Integration tests for `jjf push <remote>` and `jjf pull <remote>` —
//! drive the compiled binary against per-test scratch jj-clones-of-a-
//! bare-git-remote and assert the v3 transport: refspec
//! `refs/jjf/*:refs/jjf/*` on push, refspec
//! `refs/jjf/*:refs/remotes/<remote>/jjf/*` on fetch, then per-ref
//! five-scenario merge on pull.
//!
//! As of ticket 5 of the v3 storage epic (`07c1dc9`), these tests
//! exercise the v3 push/pull verbs end-to-end. The migrator on
//! `Storage::open` runs unconditionally (no env-var opt-out here); but
//! since v3 `jjf init` lays down the format-version sentinel directly,
//! no v2-to-v3 migration ever needs to actually run on the test
//! fixtures.
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
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn jjf")
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

    // Alice inits + creates + pushes. `jjf init` lays down the v3
    // sentinel ref; `jjf new` lands a `refs/jjf/issues/<id>` ref.
    must_succeed(&run_jjf(&alice, &["init"]), "jjf init (alice)");
    let new_out = run_jjf(&alice, &["new", "-t", "shared title"]);
    must_succeed(&new_out, "jjf new (alice)");
    let id = String::from_utf8_lossy(&new_out.stdout).trim().to_owned();
    assert_eq!(id.len(), 7, "id must be 7 hex chars: {id:?}");
    let push = run_jjf(&alice, &["push", "origin"]);
    must_succeed(&push, "jjf push origin (alice)");
    let push_stdout = String::from_utf8_lossy(&push.stdout);
    assert!(
        push_stdout.contains("refs/jjf/* ref(s) -> origin"),
        "v3 push should mention the wildcard refspec; got: {push_stdout}"
    );

    // Bob pulls (fresh clone with no local v3 sentinel — the fetch
    // refspec materializes the sentinel + every issue ref).
    let pull = run_jjf(&bob, &["pull", "origin"]);
    must_succeed(&pull, "jjf pull origin (bob)");
    let pull_stdout = String::from_utf8_lossy(&pull.stdout);
    assert!(
        pull_stdout.contains("refs/jjf/* ref(s) <- origin"),
        "first pull should mention refs pulled; got: {pull_stdout}"
    );
    assert!(
        !pull_stdout.contains("merged"),
        "clean fetch should not mention merges; got: {pull_stdout}"
    );

    // Bob can show alice's issue.
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
    assert!(
        v["refs_pushed"].as_u64().is_some(),
        "push --json must carry refs_pushed; got: {stdout}"
    );
    // At minimum, the sentinel + the one issue ref = 2 refs pushed.
    assert!(
        v["refs_pushed"].as_u64().unwrap() >= 2,
        "expected >= 2 refs pushed, got {v}"
    );
}

#[test]
fn pull_json_envelope_with_no_remote_data_yet() {
    // Alice clones a fresh bare remote and pulls before pushing —
    // remote has no `refs/jjf/*` refs yet, exit 0, `remote_present:
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
    assert_eq!(
        v["remote_present"].as_bool(),
        Some(false),
        "remote_present must be false when remote has no jjf refs; got: {stdout}"
    );
    assert_eq!(v["merged"].as_i64(), Some(0));
    assert_eq!(v["merge_strategy"].as_str(), Some("per_ref_lww"));

    // Plain-text variant.
    let out = run_jjf(&alice, &["pull", "origin"]);
    must_succeed(&out, "pull (empty remote)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no jjf refs on remote yet"),
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
/// post-pull state reflects a single value, not both.
#[test]
fn pull_two_clones_same_field_lww_converges() {
    let root = setup("two_same_field", &["alice", "bob"]);
    let alice = root.join("alice");
    let bob = root.join("bob");

    must_succeed(&run_jjf(&alice, &["init"]), "init (alice)");
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

    // Pull triggers the per-ref merge for the diverged issue ref.
    let pull = run_jjf(&bob, &["pull", "origin"]);
    must_succeed(&pull, "bob pull (divergent)");
    let pull_stdout = String::from_utf8_lossy(&pull.stdout);
    assert!(
        pull_stdout.contains("merged 1 ref(s)"),
        "pull stdout should mention merged ref count; got: {pull_stdout}"
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
/// to a final state with both labels present.
#[test]
fn pull_two_clones_different_fields_both_survive() {
    let root = setup("two_diff_fields", &["alice", "bob"]);
    let alice = root.join("alice");
    let bob = root.join("bob");

    must_succeed(&run_jjf(&alice, &["init"]), "init (alice)");
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

/// Different issues edited offline by two clones — pull should
/// fast-forward each per-issue ref independently and emit no merge
/// commits. Confirms that the per-ref refspec keeps unrelated issues
/// from triggering merge work on each other.
#[test]
fn pull_two_clones_different_issues_no_merge() {
    let root = setup("two_diff_issues", &["alice", "bob"]);
    let alice = root.join("alice");
    let bob = root.join("bob");

    must_succeed(&run_jjf(&alice, &["init"]), "init (alice)");
    let a_out = run_jjf(&alice, &["new", "-t", "alice issue"]);
    must_succeed(&a_out, "new (alice)");
    let a_id = String::from_utf8_lossy(&a_out.stdout).trim().to_owned();
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "alice push 1");
    must_succeed(&run_jjf(&bob, &["pull", "origin"]), "bob first pull");

    // Bob creates a totally separate issue while Alice creates one of
    // her own. Different per-issue refs → no per-ref divergence.
    let b_out = run_jjf(&bob, &["new", "-t", "bob issue"]);
    must_succeed(&b_out, "new (bob)");
    let b_id = String::from_utf8_lossy(&b_out.stdout).trim().to_owned();
    must_succeed(&run_jjf(&alice, &["new", "-t", "alice second"]), "alice 2nd");
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "alice push 2");

    // Bob pulls — should fast-forward alice's new issue ref without
    // touching bob's own ref, and report zero merges.
    let pull = run_jjf(&bob, &["--json", "pull", "origin"]);
    must_succeed(&pull, "bob pull");
    let v: serde_json::Value = serde_json::from_str(
        String::from_utf8_lossy(&pull.stdout).trim(),
    )
    .expect("pull --json");
    assert_eq!(v["merged"].as_i64(), Some(0));
    // Bob should see both issues.
    let show_a = run_jjf(&bob, &["show", &a_id]);
    must_succeed(&show_a, "bob show alice issue");
    let show_b = run_jjf(&bob, &["show", &b_id]);
    must_succeed(&show_b, "bob show bob issue");
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
fn push_without_init_exits_two_missing_marker() {
    // jj repo exists but `jjf init` was never run — no v3 sentinel and
    // no v2 bookmark. `push` requires the marker (either kind).
    let root = setup("push_no_init", &["alice"]);
    let alice = root.join("alice");
    let out = run_jjf(&alice, &["push", "origin"]);
    assert!(!out.status.success(), "push without init should fail");
    assert_eq!(out.status.code(), Some(2));

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

/// First-time pull on a fresh clone: bob's local repo has no v3
/// sentinel until the fetch lands it. Verifies that the v3 transport
/// correctly bootstraps a brand-new clone by copying the remote
/// sentinel into the local namespace via the wildcard refspec.
#[test]
fn pull_first_time_bootstraps_v3_sentinel() {
    let root = setup("round_trip_bootstrap", &["alice", "bob"]);
    let alice = root.join("alice");
    let bob = root.join("bob");
    must_succeed(&run_jjf(&alice, &["init"]), "init (alice)");
    must_succeed(&run_jjf(&alice, &["new", "-t", "from alice"]), "new alice");
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "push alice");

    // Pre-pull: bob has no v3 sentinel locally.
    let pre_sentinel = Command::new("git")
        .arg("-C")
        .arg(&bob)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            "refs/jjf/meta/format-version",
        ])
        .status()
        .expect("git rev-parse");
    assert!(
        !pre_sentinel.success(),
        "bob should not have v3 sentinel before pulling"
    );

    // Pull bootstraps the sentinel + every issue ref.
    must_succeed(&run_jjf(&bob, &["pull", "origin"]), "bob pull");

    // Post-pull: bob has the v3 sentinel.
    let post_sentinel = Command::new("git")
        .arg("-C")
        .arg(&bob)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            "refs/jjf/meta/format-version",
        ])
        .status()
        .expect("git rev-parse");
    assert!(
        post_sentinel.success(),
        "bob should have v3 sentinel after pulling"
    );

    // Bob has at least one local `refs/jjf/issues/*` ref.
    let issues_refs = Command::new("git")
        .arg("-C")
        .arg(&bob)
        .args(["for-each-ref", "--format=%(refname)", "refs/jjf/issues/"])
        .output()
        .expect("git for-each-ref");
    let s = String::from_utf8_lossy(&issues_refs.stdout);
    assert!(
        !s.trim().is_empty(),
        "bob should have at least one refs/jjf/issues/* ref after pull; got: {s}"
    );
}

/// Round-trip pull on a clean fetch (no diverged refs) lands as a
/// sequence of cheap fast-forwards — no merge commits in any ref's
/// chain. Pins the acceptance criterion from the ticket body.
#[test]
fn pull_clean_round_trip_is_fast_forwards_only() {
    let root = setup("ff_only", &["alice", "bob"]);
    let alice = root.join("alice");
    let bob = root.join("bob");

    must_succeed(&run_jjf(&alice, &["init"]), "init (alice)");
    let id_out = run_jjf(&alice, &["new", "-t", "thing"]);
    must_succeed(&id_out, "new (alice)");
    let id = String::from_utf8_lossy(&id_out.stdout).trim().to_owned();
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "push 1");
    must_succeed(&run_jjf(&bob, &["pull", "origin"]), "first pull (bob)");

    // Alice mutates; nobody else does.
    must_succeed(
        &run_jjf(&alice, &["update", &id, "--title", "renamed"]),
        "alice rename",
    );
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "push 2");

    let pull = run_jjf(&bob, &["--json", "pull", "origin"]);
    must_succeed(&pull, "bob pull");
    let v: serde_json::Value = serde_json::from_str(
        String::from_utf8_lossy(&pull.stdout).trim(),
    )
    .expect("pull --json");
    assert_eq!(v["merged"].as_i64(), Some(0), "no merges on clean pull");
    let ff = v["fast_forwards"].as_u64().unwrap_or(0);
    assert!(
        ff >= 1,
        "expected at least one fast-forward for alice's update; got {v}"
    );
}
