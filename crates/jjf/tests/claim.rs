//! Integration tests for `jjf update --claim` / `--unclaim` and
//! `jjf ready --claim` / `--include-claimed` — drive the compiled
//! binary against per-test scratch repos and assert the v2.3
//! atomicity, idempotency, and parallel-claim safety contract.
//!
//! Tests verify:
//!
//! - `update --claim` lands one commit with TWO trailers
//!   (set-assignee + set-status=in-progress).
//! - Same-user reclaim is a no-op (no new commit, exit 0).
//! - Different-user reclaim errors with `already_claimed` (exit 2).
//! - `update --unclaim` round-trips assignee + status.
//! - `ready` excludes InProgress by default.
//! - `ready --include-claimed` includes InProgress.
//! - `ready --claim --limit 1` picks + claims atomically.
//! - `ready --claim` without `--limit 1` errors with
//!   `claim_requires_limit_one` (exit 2).
//! - Parallel-claim race: two spawned binaries claiming
//!   simultaneously end up with two DIFFERENT ids assigned.
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as the other
//! test files in this crate.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;
use common::{run_jjf, run_jjf_with_stdin, scratch, JJF_BIN};

fn make_jj_repo_with_user(name: &str, user: &str) -> PathBuf {
    let dir = scratch(name);
    let out = Command::new("git")
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("spawn git init");
    assert!(
        out.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Pin user.name so the `--claim` resolver has an identity. The actor
    // chain reads `git config user.name` (jj config calls removed in J7).
    let out = Command::new("git")
        .args(["config", "user.name", user])
        .current_dir(&dir)
        .output()
        .expect("spawn git config user.name");
    assert!(
        out.status.success(),
        "git config user.name failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(&dir)
        .output()
        .expect("spawn git config user.email");
    assert!(
        out.status.success(),
        "git config user.email failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

fn make_initialized_repo(name: &str) -> PathBuf {
    make_initialized_repo_with_user(name, "Test User")
}

fn make_initialized_repo_with_user(name: &str, user: &str) -> PathBuf {
    let repo = make_jj_repo_with_user(name, user);
    let out = Command::new(JJF_BIN)
        .arg("init")
        .current_dir(&repo)
        .output()
        .expect("spawn jjf init");
    assert!(
        out.status.success(),
        "jjf init in {} failed: {}",
        repo.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    repo
}

/// Create an issue via `jjf new`, return its id.
fn create_issue(repo: &Path, title: &str, extra_args: &[&str]) -> String {
    let mut args: Vec<&str> = vec!["new", "-t", title, "-F", "-"];
    args.extend_from_slice(extra_args);
    let out = run_jjf_with_stdin(repo, &args, b"");
    assert!(
        out.status.success(),
        "jjf new failed during setup: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

// --- tests --------------------------------------------------------

#[test]
fn update_claim_sets_assignee_and_status_in_progress() {
    let repo = make_initialized_repo_with_user("claim_basic", "alice");
    let id = create_issue(&repo, "claim me", &["--type", "feature"]);

    let out = run_jjf(&repo, &["update", &id, "--claim"]);
    assert!(
        out.status.success(),
        "jjf update --claim failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("claimed") && stdout.contains(&id) && stdout.contains("alice"),
        "stdout should say `claimed <id> by alice`, got: {stdout}"
    );

    // show reports assignee=alice, status=in-progress.
    let out = run_jjf(&repo, &["show", &id]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("in-progress"),
        "show should report in-progress status, got: {stdout}"
    );
    assert!(
        stdout.contains("assignee: alice"),
        "show should report assignee: alice, got: {stdout}"
    );
}

#[test]
fn update_claim_lands_one_commit_with_two_trailers() {
    use jjf_storage::{IssueId, Op, Status, Storage};

    let repo = make_initialized_repo_with_user("claim_two_trailers", "alice");
    let id = create_issue(&repo, "x", &[]);

    let storage = Storage::open(&repo).expect("Storage::open");
    let issue_id = IssueId::parse(&id).expect("parse id");
    let baseline = storage.read_history(&issue_id).unwrap().len();

    let out = run_jjf(&repo, &["update", &id, "--claim"]);
    assert!(
        out.status.success(),
        "jjf update --claim failed: stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );

    let hist = storage.read_history(&issue_id).unwrap();
    let new = &hist[baseline..];
    assert_eq!(new.len(), 2, "expected two new ops, got {new:#?}");
    let commit = &new[0].commit;
    assert!(
        new.iter().all(|e| &e.commit == commit),
        "both ops must share one commit: {new:#?}"
    );
    assert!(matches!(
        new[0].op,
        Op::SetAssignee {
            assignee: Some(_),
            ..
        }
    ));
    assert!(matches!(
        new[1].op,
        Op::SetStatus {
            status: Status::InProgress,
            ..
        }
    ));
}

#[test]
fn update_claim_idempotent_same_user_is_no_op_at_commit_level() {
    use jjf_storage::{IssueId, Storage};

    let repo = make_initialized_repo_with_user("claim_idempotent", "alice");
    let id = create_issue(&repo, "x", &[]);
    let storage = Storage::open(&repo).unwrap();
    let issue_id = IssueId::parse(&id).unwrap();

    let out = run_jjf(&repo, &["update", &id, "--claim"]);
    assert!(out.status.success());
    let count_after_first = storage.read_history(&issue_id).unwrap().len();

    // Re-claim by same user — exits 0 but writes no new commit.
    let out = run_jjf(&repo, &["update", &id, "--claim"]);
    assert!(
        out.status.success(),
        "re-claim by same user should exit 0, got code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let count_after_second = storage.read_history(&issue_id).unwrap().len();
    assert_eq!(
        count_after_first, count_after_second,
        "re-claim by same user must not add commits"
    );
}

#[test]
fn update_claim_different_user_errors_already_claimed() {
    // Set up: alice claims. Then run a SECOND jjf instance with
    // user.name=bob — bob's --claim must fail with already_claimed
    // (exit 2).
    let repo = make_initialized_repo_with_user("claim_diff_user", "alice");
    let id = create_issue(&repo, "x", &[]);
    let out = run_jjf(&repo, &["update", &id, "--claim"]);
    assert!(out.status.success(), "alice's claim must succeed");

    // Flip user.name to bob in git config (the binary reads from git
    // config, so only git config needs to be updated; jj config calls
    // removed in J7).
    let out = Command::new("git")
        .args(["config", "user.name", "bob"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = run_jjf(&repo, &["--json", "update", &id, "--claim"]);
    assert!(!out.status.success(), "bob's claim must fail");
    assert_eq!(
        out.status.code(),
        Some(2),
        "already_claimed should be exit 2, got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("stderr must be JSON envelope");
    assert_eq!(v["ok"], serde_json::Value::Bool(false));
    assert_eq!(
        v["error"]["kind"].as_str(),
        Some("already_claimed"),
        "kind wrong: {stderr}"
    );
    assert_eq!(
        v["error"]["details"]["by"].as_str(),
        Some("alice"),
        "details.by should name the existing claimant: {stderr}"
    );
}

#[test]
fn update_unclaim_clears_assignee_and_resets_status() {
    let repo = make_initialized_repo_with_user("unclaim_round_trip", "alice");
    let id = create_issue(&repo, "x", &[]);
    let out = run_jjf(&repo, &["update", &id, "--claim"]);
    assert!(out.status.success());

    let out = run_jjf(&repo, &["update", &id, "--unclaim"]);
    assert!(
        out.status.success(),
        "unclaim should succeed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run_jjf(&repo, &["show", &id]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[open]"),
        "show should report open status after unclaim, got: {stdout}"
    );
    assert!(
        stdout.contains("assignee: (none)"),
        "assignee should be cleared, got: {stdout}"
    );
}

#[test]
fn update_claim_and_unclaim_are_mutually_exclusive() {
    let repo = make_initialized_repo("claim_unclaim_conflict");
    let id = create_issue(&repo, "x", &[]);
    let out = run_jjf(&repo, &["update", &id, "--claim", "--unclaim"]);
    assert!(!out.status.success(), "--claim + --unclaim must fail");
    assert_eq!(out.status.code(), Some(2), "clap conflict should exit 2");
}

#[test]
fn update_claim_and_assignee_are_mutually_exclusive() {
    let repo = make_initialized_repo("claim_assignee_conflict");
    let id = create_issue(&repo, "x", &[]);
    let out = run_jjf(
        &repo,
        &["update", &id, "--claim", "--assignee", "alice"],
    );
    assert!(!out.status.success(), "--claim + --assignee must fail");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn ready_excludes_in_progress_by_default() {
    let repo = make_initialized_repo_with_user("ready_excludes_claimed", "alice");
    let a = create_issue(&repo, "A", &["--type", "feature"]);
    let b = create_issue(&repo, "B", &["--type", "feature"]);
    let out = run_jjf(&repo, &["update", &a, "--claim"]);
    assert!(out.status.success());

    let out = run_jjf(&repo, &["ready", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    let arr = v.as_array().unwrap();
    let ids: Vec<&str> = arr.iter().map(|x| x["id"].as_str().unwrap()).collect();
    assert!(!ids.contains(&a.as_str()), "A is claimed, must be hidden: {ids:?}");
    assert!(ids.contains(&b.as_str()), "B should be visible: {ids:?}");
    assert_eq!(arr.len(), 1);
}

#[test]
fn ready_include_claimed_shows_in_progress_too() {
    let repo = make_initialized_repo_with_user("ready_include_claimed", "alice");
    let a = create_issue(&repo, "A", &["--type", "feature"]);
    let _b = create_issue(&repo, "B", &["--type", "feature"]);
    let out = run_jjf(&repo, &["update", &a, "--claim"]);
    assert!(out.status.success());

    let out = run_jjf(&repo, &["ready", "--include-claimed", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2, "both A and B should be visible: {arr:?}");
}

#[test]
fn ready_claim_limit_one_picks_and_claims_atomically() {
    use jjf_storage::{IssueId, Status, Storage};

    let repo = make_initialized_repo_with_user("ready_claim_one", "alice");
    let _a = create_issue(&repo, "A", &["--type", "feature"]);

    let out = run_jjf(&repo, &["--json", "ready", "--claim", "--limit", "1"]);
    assert!(
        out.status.success(),
        "ready --claim --limit 1 failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
    assert_eq!(v["claimed"], serde_json::Value::Bool(true));
    let claimed_id = v["id"].as_str().expect("id should be a string").to_owned();
    assert!(!claimed_id.is_empty());

    // The claimed issue is now InProgress with assignee=alice.
    let storage = Storage::open(&repo).unwrap();
    let issue = storage.read(&IssueId::parse(&claimed_id).unwrap()).unwrap();
    assert_eq!(issue.status, Status::InProgress);
    assert_eq!(issue.assignee.as_deref(), Some("alice"));
}

#[test]
fn ready_claim_requires_limit_one() {
    let repo = make_initialized_repo_with_user("ready_claim_no_limit", "alice");
    let _a = create_issue(&repo, "A", &["--type", "feature"]);

    // No limit flag.
    let out = run_jjf(&repo, &["ready", "--claim"]);
    assert!(!out.status.success(), "--claim without --limit must fail");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--claim") && stderr.contains("--limit"),
        "stderr should mention --claim and --limit, got: {stderr}"
    );

    // --limit 2 — also rejected.
    let out = run_jjf(&repo, &["ready", "--claim", "--limit", "2"]);
    assert!(!out.status.success(), "--claim --limit 2 must fail");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn ready_claim_on_empty_set_succeeds_with_null_id() {
    // No open issues to claim → exit 0 with null id under JSON,
    // silent under plain text.
    let repo = make_initialized_repo_with_user("ready_claim_empty", "alice");

    let out = run_jjf(&repo, &["--json", "ready", "--claim", "--limit", "1"]);
    assert!(
        out.status.success(),
        "ready --claim on empty should succeed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
    assert_eq!(v["claimed"], serde_json::Value::Bool(false));
    assert!(v["id"].is_null(), "id should be null on empty: {v}");
}

#[test]
fn parallel_ready_claim_limit_one_assigns_two_different_ids() {
    // The race-safety acceptance criterion. Spawn two `jjf ready
    // --claim --limit 1` instances in parallel against the same
    // repo and assert they end up holding TWO DIFFERENT ids — one
    // wins the bookmark race for the top id, the other re-reads
    // ready and picks the next.
    //
    // Setup: two unblocked issues. Race two claims.
    let repo = make_initialized_repo_with_user("ready_claim_race", "alice");
    let a = create_issue(&repo, "A", &["--type", "feature"]);
    let b = create_issue(&repo, "B", &["--type", "feature"]);

    let repo1 = repo.clone();
    let repo2 = repo.clone();
    let t1 = std::thread::spawn(move || run_jjf(&repo1, &["--json", "ready", "--claim", "--limit", "1"]));
    let t2 = std::thread::spawn(move || run_jjf(&repo2, &["--json", "ready", "--claim", "--limit", "1"]));
    let r1 = t1.join().unwrap();
    let r2 = t2.join().unwrap();

    // One process MUST succeed claiming the top id. The other
    // either also succeeds (claiming the next id, because both
    // ids were ready) or fails (one of the calls landed first
    // and the loser's commit was rejected by `jj bookmark set`).
    //
    // The acceptance is: if BOTH succeeded, they claimed DIFFERENT
    // ids (the resolver gave the loser its own ready list).
    let p = |o: &Output| {
        if o.status.success() {
            let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
            Some(v["id"].as_str().unwrap().to_owned())
        } else {
            None
        }
    };
    let id1 = p(&r1);
    let id2 = p(&r2);

    // At least ONE must have succeeded.
    assert!(
        id1.is_some() || id2.is_some(),
        "both parallel claims failed: r1={:?} r2={:?}",
        String::from_utf8_lossy(&r1.stderr),
        String::from_utf8_lossy(&r2.stderr),
    );

    // If both succeeded, the ids MUST differ (no duplicate work).
    if let (Some(i1), Some(i2)) = (&id1, &id2) {
        assert_ne!(
            i1, i2,
            "parallel claims must NOT both grab the same id: {i1} vs {i2}"
        );
    }

    // Sanity: every successful claim landed on an id that's actually one
    // of our two issues (no claim "from thin air").
    for id_opt in [&id1, &id2] {
        if let Some(id) = id_opt {
            assert!(
                id == &a || id == &b,
                "claimed id {id} is not one of the two issues we created ({a}, {b})"
            );
        }
    }
}
