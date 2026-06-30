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
//! at `env!("CARGO_BIN_EXE_iss")` (no `assert_cmd` dep).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;
use common::{run_jjf, scratch_non_git, JJF_BIN};

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
/// one plain git clone per name in `clones`. Returns the root.
///
/// Uses `git clone` (no jj — J7). The clones are plain git repos
/// with `origin` already pointing at the bare remote, which is all
/// `jjf push`/`jjf pull` needs.
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
        let clone_out = Command::new("git")
            .arg("clone")
            .arg(remote.to_str().unwrap())
            .arg(&dest)
            .output()
            .expect("git clone");
        must_succeed(&clone_out, &format!("git clone {clone}"));
        // Configure a per-clone identity so the actor chain has an identity
        // and commits don't all collide on a shared author.
        let cfg_name = Command::new("git")
            .args(["config", "user.name", clone])
            .current_dir(&dest)
            .output()
            .expect("git config user.name");
        must_succeed(&cfg_name, "git config user.name");
        let cfg_mail = Command::new("git")
            .args(["config", "user.email", &format!("{clone}@example.com")])
            .current_dir(&dest)
            .output()
            .expect("git config user.email");
        must_succeed(&cfg_mail, "git config user.email");
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
    // The one issue ref counts toward `refs_pushed`; the
    // `meta/format-version` sentinel ref is force-pushed alongside but
    // is not counted as a "data ref" (its content isn't validated;
    // it's a presence flag — see ticket `95fb2d6`).
    assert!(
        v["refs_pushed"].as_u64().unwrap() >= 1,
        "expected >= 1 refs pushed, got {v}"
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
    let dir = scratch_non_git("push_non_jj");
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
    let dir = scratch_non_git("pull_non_jj");
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

// ---------------------------------------------------------------
// meta/* sentinel divergence cluster (tickets eaf0674 / 0c0e7d8 /
// 8034dc1). Each peer plants its own sentinel at `jjf init` time;
// pull must not error on the divergence and push must not exit 1 on
// the resulting non-fast-forward against the remote sentinel.
// ---------------------------------------------------------------

/// Ticket `0c0e7d8` (F-002): `jjf pull` from a fresh clone that ran
/// `jjf init` (which plants a fresh local sentinel) must return ok
/// even when the remote's `refs/jjf/meta/format-version` resolves to
/// a different commit than the local one. The pre-fix classifier
/// routed `meta/*` divergence to the "refusing to merge" error path;
/// after the fix it's a no-op (keep local).
#[test]
fn pull_meta_sentinel_divergence_is_noop_not_error() {
    let root = setup("meta_divergence_pull", &["alice", "bob"]);
    let alice = root.join("alice");
    let bob = root.join("bob");

    // Alice inits, creates an issue, pushes — remote now has a
    // sentinel + one issue ref.
    must_succeed(&run_jjf(&alice, &["init"]), "init (alice)");
    let new_out = run_jjf(&alice, &["new", "-t", "alice's issue"]);
    must_succeed(&new_out, "new (alice)");
    let id = String::from_utf8_lossy(&new_out.stdout).trim().to_owned();
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "alice push");

    // Bob runs `jjf init` on his fresh clone — this plants a LOCAL
    // sentinel. To guarantee divergence with alice's sentinel in
    // this test (otherwise wall-clock-coincident `git commit-tree`
    // calls can produce identical OIDs), we then overwrite bob's
    // sentinel with a hand-crafted commit that differs by message.
    must_succeed(&run_jjf(&bob, &["init"]), "init (bob)");

    // Build a divergent sentinel commit deterministically.
    let blob_oid = {
        let out = Command::new("git")
            .arg("-C")
            .arg(&bob)
            .args(["hash-object", "-w", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn hash-object");
        use std::io::Write;
        out.stdin
            .as_ref()
            .expect("stdin")
            .write_all(b"version: 3\n")
            .expect("write");
        let o = out.wait_with_output().expect("hash-object wait");
        must_succeed(&o, "hash-object");
        String::from_utf8_lossy(&o.stdout).trim().to_owned()
    };
    let tree_oid = {
        let out = Command::new("git")
            .arg("-C")
            .arg(&bob)
            .args(["mktree"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn mktree");
        use std::io::Write;
        out.stdin
            .as_ref()
            .expect("stdin")
            .write_all(format!("100644 blob {blob_oid}\tversion\n").as_bytes())
            .expect("write");
        let o = out.wait_with_output().expect("mktree wait");
        must_succeed(&o, "mktree");
        String::from_utf8_lossy(&o.stdout).trim().to_owned()
    };
    let commit_oid = {
        let out = Command::new("git")
            .arg("-C")
            .arg(&bob)
            .args(["commit-tree", &tree_oid, "-m", "bob's divergent sentinel"])
            .env("GIT_AUTHOR_NAME", "bob")
            .env("GIT_AUTHOR_EMAIL", "bob@example.com")
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_NAME", "bob")
            .env("GIT_COMMITTER_EMAIL", "bob@example.com")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
            .output()
            .expect("commit-tree");
        must_succeed(&out, "commit-tree");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    let update = Command::new("git")
        .arg("-C")
        .arg(&bob)
        .args(["update-ref", "refs/jjf/meta/format-version", &commit_oid])
        .output()
        .expect("update-ref");
    must_succeed(&update, "update-ref bob sentinel");
    let bob_local_oid = commit_oid;

    // Now the pull. Pre-fix this exited 1 with "diverged ref ... not
    // in a known v3 namespace; refusing to merge". Post-fix: exit 0.
    let pull = run_jjf(&bob, &["pull", "origin"]);
    must_succeed(&pull, "bob pull (diverged sentinel)");

    // The data ref still landed: bob can show alice's issue.
    let show = run_jjf(&bob, &["show", &id]);
    must_succeed(&show, "bob show alice's issue");

    // Local sentinel is preserved (we kept local on meta divergence).
    let bob_local_after = Command::new("git")
        .arg("-C")
        .arg(&bob)
        .args([
            "rev-parse",
            "--verify",
            "refs/jjf/meta/format-version",
        ])
        .output()
        .expect("git rev-parse bob local post-pull");
    must_succeed(&bob_local_after, "git rev-parse bob local post-pull");
    let bob_local_after_oid = String::from_utf8_lossy(&bob_local_after.stdout)
        .trim()
        .to_owned();
    assert_eq!(
        bob_local_oid, bob_local_after_oid,
        "meta/* divergence should keep local sentinel unchanged"
    );
}

/// Ticket `8034dc1` (F-003): `jjf push` from a fresh clone that ran
/// `jjf init` must return ok even when the local sentinel is
/// non-fast-forward against the remote sentinel. Pre-fix the push
/// refspec was `refs/jjf/*:refs/jjf/*` (no force on meta/*) and the
/// push exited 1 forever. Post-fix the meta refspec is `+`-prefixed
/// (force) and data refs are non-force.
#[test]
fn push_meta_sentinel_non_fast_forward_succeeds_with_data() {
    let root = setup("meta_divergence_push", &["alice", "bob"]);
    let alice = root.join("alice");
    let bob = root.join("bob");

    must_succeed(&run_jjf(&alice, &["init"]), "init (alice)");
    must_succeed(&run_jjf(&alice, &["new", "-t", "alice"]), "new (alice)");
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "alice push");

    // Bob inits (fresh sentinel diverges from remote) and creates his
    // own issue + memory locally.
    must_succeed(&run_jjf(&bob, &["init"]), "init (bob)");
    let new_out = run_jjf(&bob, &["new", "-t", "bob's issue"]);
    must_succeed(&new_out, "new (bob)");
    let bob_id = String::from_utf8_lossy(&new_out.stdout).trim().to_owned();
    must_succeed(
        &run_jjf(&bob, &["remember", "bob's note", "--key", "bob-note"]),
        "bob remember",
    );

    // Push. Pre-fix this exited 1 with "non-fast-forward on
    // refs/jjf/meta/format-version". Post-fix: exit 0.
    let push = run_jjf(&bob, &["push", "origin"]);
    must_succeed(&push, "bob push (diverged sentinel)");

    // Alice pulls bob's push and can see both the issue and the
    // memory.
    must_succeed(&run_jjf(&alice, &["pull", "origin"]), "alice pull");
    let show = run_jjf(&alice, &["show", &bob_id]);
    must_succeed(&show, "alice show bob's issue");
    let recall = run_jjf(&alice, &["recall", "bob-note"]);
    must_succeed(&recall, "alice recall bob-note");
    assert!(
        String::from_utf8_lossy(&recall.stdout).contains("bob's note"),
        "alice should see bob's memory after pull"
    );

    // A second push is also clean (no growing-state in the
    // sentinel refspec — meta is force, idempotent).
    let push2 = run_jjf(&bob, &["push", "origin"]);
    must_succeed(&push2, "bob push #2");
}

/// Ticket `eaf0674` (F-001): after running `jjf init` on a freshly-
/// cloned repo whose remote is already configured, the v3 fetch
/// refspec gets written into `.git/config` so a plain `git fetch
/// origin` carries `refs/jjf/*` into the local namespace. Without
/// this, the operator must hand-write `git fetch origin
/// 'refs/jjf/*:refs/jjf/*'` before `jjf ls` will work — the original
/// papercut the ticket reports.
#[test]
fn init_writes_jjf_fetch_refspec_for_existing_remotes() {
    let root = setup("init_writes_refspec", &["alice", "bob"]);
    let alice = root.join("alice");
    let bob = root.join("bob");

    // Alice publishes some data so `git fetch origin` on bob actually
    // has something to materialize.
    must_succeed(&run_jjf(&alice, &["init"]), "init (alice)");
    let new_out = run_jjf(&alice, &["new", "-t", "from alice"]);
    must_succeed(&new_out, "new (alice)");
    let id = String::from_utf8_lossy(&new_out.stdout).trim().to_owned();
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "alice push");

    // Sanity: bob's fresh clone has the standard heads-only refspec
    // and no jjf refspec.
    let pre = Command::new("git")
        .arg("-C")
        .arg(&bob)
        .args(["config", "--get-all", "remote.origin.fetch"])
        .output()
        .expect("git config probe");
    let pre_text = String::from_utf8_lossy(&pre.stdout);
    assert!(
        !pre_text.contains("refs/jjf/*"),
        "precondition: bob's clone should not have a jjforge fetch refspec yet; got:\n{pre_text}"
    );

    // `jjf init` should plant the refspec.
    must_succeed(&run_jjf(&bob, &["init"]), "init (bob)");
    let post = Command::new("git")
        .arg("-C")
        .arg(&bob)
        .args(["config", "--get-all", "remote.origin.fetch"])
        .output()
        .expect("git config probe post-init");
    let post_text = String::from_utf8_lossy(&post.stdout);
    assert!(
        post_text
            .lines()
            .any(|l| l.trim() == "+refs/jjf/*:refs/remotes/origin/jjf/*"),
        "init should have planted the jjforge fetch refspec; got:\n{post_text}"
    );

    // Re-running `jjf init` is idempotent: the refspec is not
    // duplicated.
    must_succeed(&run_jjf(&bob, &["init"]), "init (bob) #2");
    let after = Command::new("git")
        .arg("-C")
        .arg(&bob)
        .args(["config", "--get-all", "remote.origin.fetch"])
        .output()
        .expect("git config probe after second init");
    let after_text = String::from_utf8_lossy(&after.stdout);
    let count = after_text
        .lines()
        .filter(|l| l.trim() == "+refs/jjf/*:refs/remotes/origin/jjf/*")
        .count();
    assert_eq!(
        count, 1,
        "second init must not duplicate the refspec; got:\n{after_text}"
    );

    // End-to-end: a plain `git fetch origin` on bob now materializes
    // `refs/jjf/*` into the local namespace, which is the actual
    // user-facing behavior the ticket cares about.
    let fetch = Command::new("git")
        .arg("-C")
        .arg(&bob)
        .args(["fetch", "origin"])
        .output()
        .expect("git fetch origin");
    must_succeed(&fetch, "plain git fetch origin");
    let issues_refs = Command::new("git")
        .arg("-C")
        .arg(&bob)
        .args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/remotes/origin/jjf/issues/",
        ])
        .output()
        .expect("git for-each-ref");
    let s = String::from_utf8_lossy(&issues_refs.stdout);
    assert!(
        !s.trim().is_empty(),
        "plain git fetch should have brought alice's issue ref under refs/remotes/origin/jjf/issues/; got:\n{s}"
    );
    // And `jjf show` finds alice's issue without any extra fetch.
    let pull = run_jjf(&bob, &["pull", "origin"]);
    must_succeed(&pull, "bob pull");
    let show = run_jjf(&bob, &["show", &id]);
    must_succeed(&show, "bob show alice's issue");
}

// ---------------------------------------------------------------
// Metadata LWW via divergent remote tips (spec finding #11)
// ---------------------------------------------------------------

/// Metadata LWW via divergent remote tips — exercises `merge_ops::reduce_to_merged`.
///
/// The existing `metadata_lww_under_concurrent_writes` test in
/// `crates/jjf-storage/tests/integration.rs` pins the write-path
/// (sequential chain, CAS retry). This test exercises the READ path:
/// two clones write the same metadata key offline, then the pull-merge
/// driver at scenario 5 (diverged) calls `reduce_to_merged` to resolve
/// the conflict. The post-merge read must see exactly one value for the
/// key — never both, never neither.
///
/// Structure mirrors `pull_two_clones_same_field_lww_converges`.
#[test]
fn metadata_lww_under_divergent_remote_tips() {
    let root = setup("meta_lww_divergent", &["alice", "bob"]);
    let alice = root.join("alice");
    let bob = root.join("bob");

    must_succeed(&run_jjf(&alice, &["init"]), "init (alice)");
    let new_out = run_jjf(&alice, &["new", "-t", "shared"]);
    must_succeed(&new_out, "new (alice)");
    let id = String::from_utf8_lossy(&new_out.stdout).trim().to_owned();
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "alice push");
    must_succeed(&run_jjf(&bob, &["pull", "origin"]), "bob first pull");

    // Concurrent offline edits: each clone sets the same metadata key
    // to a different value without syncing — this creates a true fork
    // in the refs/jjf/issues/<id> DAG. The pull-merge driver will hit
    // scenario 5 (diverged) and call reduce_to_merged.
    must_succeed(
        &run_jjf(&alice, &["metadata", "set", &id, "gc.kind", "alice-value"]),
        "alice set metadata",
    );
    must_succeed(&run_jjf(&alice, &["push", "origin"]), "alice push 2");
    // Bob writes WITHOUT pulling first — his ref is still at the pre-push
    // alice tip, so bob's commit diverges from alice's.
    must_succeed(
        &run_jjf(&bob, &["metadata", "set", &id, "gc.kind", "bob-value"]),
        "bob set metadata",
    );

    // Pull triggers scenario 5: refs/jjf/issues/<id> on bob diverges
    // from the remote tip. reduce_to_merged runs to resolve.
    let pull = run_jjf(&bob, &["pull", "origin"]);
    must_succeed(&pull, "bob pull (divergent metadata)");
    let pull_stdout = String::from_utf8_lossy(&pull.stdout);
    assert!(
        pull_stdout.contains("merged 1 ref(s)"),
        "pull stdout should mention merged ref count; got: {pull_stdout}"
    );

    // Post-merge: exactly one value for "gc.kind", never both.
    let show = run_jjf(&bob, &["show", "--json", &id]);
    must_succeed(&show, "bob show --json");
    let v: serde_json::Value =
        serde_json::from_slice(&show.stdout).expect("jjf show --json must emit valid JSON");
    let metadata = v.get("metadata").expect("show --json must have 'metadata' field");
    let gc_kind = metadata
        .get("gc.kind")
        .and_then(|v| v.as_str())
        .expect("gc.kind must be present post-merge");
    assert!(
        gc_kind == "alice-value" || gc_kind == "bob-value",
        "LWW winner must be one of the two written values; got {gc_kind:?}"
    );
    // The loser must not appear anywhere in the metadata map.
    let metadata_str = metadata.to_string();
    let (winner, loser) = if gc_kind == "alice-value" {
        ("alice-value", "bob-value")
    } else {
        ("bob-value", "alice-value")
    };
    assert!(
        metadata_str.contains(winner),
        "winner {winner:?} must appear in metadata; got: {metadata_str}"
    );
    assert!(
        !metadata_str.contains(loser),
        "loser {loser:?} must not appear in metadata post-merge; got: {metadata_str}"
    );
}
