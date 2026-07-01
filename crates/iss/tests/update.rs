//! Integration tests for `iss update <id> [--title T] [--status S]
//! [--body-file PATH|-] [--assignee NAME] [--unset-assignee] [--json]`
//! — drive the compiled binary against per-test scratch repos and
//! assert the full ticket matrix:
//!
//! - single-field happy paths (`--title` reads back via `show`),
//! - three-field-at-once happy path with the **load-bearing**
//!   one-commit-three-trailers assertion (verified via
//!   `Storage::read_history`),
//! - `--assignee` set then `--unset-assignee` clear round trip,
//! - plain-text + `--json` output shapes (field list matches what
//!   actually changed, in field-declaration order),
//! - no field flags → exit 2 with the at-least-one hint,
//! - `--assignee` + `--unset-assignee` → exit 2 (clap conflicts_with),
//! - nonexistent bug id → exit 1 (runtime, via `IssueNotFound`),
//! - bad id parse → exit 2,
//! - non-jj cwd → exit 2 with `not a jj repo`,
//! - jj repo without `issues` bookmark → exit 2 with init hint,
//! - unreadable `--body-file` path → exit 2,
//! - `--help` documents every field flag + `--json`.
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as the other
//! test files in this crate. The one-commit-three-trailers assertion
//! peeks at `Storage::read_history` directly — same trick `label.rs`
//! uses for its non-idempotency assertions — because the CLI doesn't
//! expose history yet (`cli-history` territory).

use std::path::Path;
use std::process::Command;

mod common;
use common::*;

/// Create a bug via `iss new`, return its id.
fn create_issue(repo: &Path, title: &str) -> String {
    let out = run_jjf_with_stdin(repo, &["new", "-t", title, "-F", "-"], b"");
    assert!(
        out.status.success(),
        "jjf new failed during setup: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

// --- tests ---------------------------------------------------------

#[test]
fn update_title_only_show_reports_new_title() {
    let repo = make_initialized_repo("update_title_only");
    let id = create_issue(&repo, "before");

    let out = run_jjf(&repo, &["update", &id, "--title", "after"]);
    assert!(
        out.status.success(),
        "jjf update --title failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), format!("updated {id}: title"));

    // show now reports the new title.
    let out = run_jjf(&repo, &["show", &id]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("after"),
        "show should report the new title, got: {stdout}"
    );
    // And the old title is gone.
    assert!(
        !stdout.lines().any(|l| l == "before"),
        "old title must not survive: {stdout}"
    );
}

#[test]
fn update_three_fields_lands_one_commit_with_three_trailers() {
    // The load-bearing acceptance criterion: changing title + status +
    // body in a single CLI call lands ONE new commit on the bookmark
    // carrying THREE `Jjf-Op:` trailers. We verify via
    // `Storage::read_history`: three new entries appear, all sharing
    // the same `commit` id.
    use iss_storage::{IssueId, Op, Storage};

    let repo = make_initialized_repo("update_three_fields_one_commit");
    let id = create_issue(&repo, "initial title");

    let storage = Storage::open(&repo).expect("Storage::open");
    let issue_id = IssueId::parse(&id).expect("parse id");
    let baseline_count = storage
        .read_history(&issue_id)
        .expect("read_history baseline")
        .len();

    // Pipe the body on stdin (`--body-file -`) so the stdin pathway
    // gets exercised too.
    let out = run_jjf_with_stdin(
        &repo,
        &[
            "update",
            &id,
            "--title",
            "new title",
            "--status",
            "closed",
            "--body-file",
            "-",
        ],
        b"new body from stdin",
    );
    assert!(
        out.status.success(),
        "jjf update three-fields failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Plain-text field list is in field-declaration order.
    assert_eq!(
        stdout.trim(),
        format!("updated {id}: title, status, body")
    );

    // Read history. New entries = three (set-title, set-status,
    // set-body), all sharing one `commit` id.
    let history = storage.read_history(&issue_id).expect("read_history after");
    let new = &history[baseline_count..];
    assert_eq!(
        new.len(),
        3,
        "expected three new ops, got {}: {:#?}",
        new.len(),
        new,
    );
    let commit = &new[0].commit;
    assert!(
        new.iter().all(|e| &e.commit == commit),
        "all three new ops must share ONE commit (multi-op-per-commit), got: {:#?}",
        new,
    );
    // Op-type sanity: title, status, body in field-declaration order.
    assert!(
        matches!(new[0].op, Op::SetTitle { .. }),
        "new[0] expected SetTitle, got {:?}",
        new[0].op
    );
    assert!(
        matches!(new[1].op, Op::SetStatus { .. }),
        "new[1] expected SetStatus, got {:?}",
        new[1].op
    );
    assert!(
        matches!(new[2].op, Op::SetBody { .. }),
        "new[2] expected SetBody, got {:?}",
        new[2].op
    );

    // And the record reflects every change.
    let bug = storage.read(&issue_id).expect("read");
    assert_eq!(bug.title, "new title");
    assert_eq!(bug.body, "new body from stdin");
    assert!(matches!(bug.status, iss_storage::Status::Closed));
}

#[test]
fn update_assignee_set_then_unset_round_trip() {
    let repo = make_initialized_repo("update_assignee_round_trip");
    let id = create_issue(&repo, "assign me");

    // Set.
    let out = run_jjf(&repo, &["update", &id, "--assignee", "alice"]);
    assert!(
        out.status.success(),
        "jjf update --assignee failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let out = run_jjf(&repo, &["show", &id]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("assignee: alice"),
        "show after set should report `assignee: alice`, got: {stdout}"
    );

    // Unset.
    let out = run_jjf(&repo, &["update", &id, "--unset-assignee"]);
    assert!(
        out.status.success(),
        "jjf update --unset-assignee failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let out = run_jjf(&repo, &["show", &id]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("assignee: (none)"),
        "show after unset should report `assignee: (none)`, got: {stdout}"
    );

    // And the storage-side read returns `None` (not `Some("")`).
    use iss_storage::{IssueId, Storage};
    let storage = Storage::open(&repo).expect("Storage::open");
    let issue_id = IssueId::parse(&id).expect("parse id");
    let bug = storage.read(&issue_id).expect("read");
    assert_eq!(bug.assignee, None);
}

#[test]
fn update_json_envelope_shape() {
    let repo = make_initialized_repo("update_json");
    let id = create_issue(&repo, "json me");

    let out = run_jjf(
        &repo,
        &[
            "update",
            "--json",
            &id,
            "--title",
            "json title",
            "--status",
            "closed",
        ],
    );
    assert!(
        out.status.success(),
        "jjf update --json failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("update --json must be valid JSON");
    assert_eq!(v["ok"].as_bool(), Some(true), "ok wrong: {stdout}");
    assert_eq!(v["id"].as_str(), Some(id.as_str()), "id wrong: {stdout}");
    let fields = v["fields"]
        .as_array()
        .expect("fields must be an array");
    let names: Vec<&str> = fields.iter().filter_map(|x| x.as_str()).collect();
    assert_eq!(
        names,
        vec!["title", "status"],
        "fields array should list the changed fields in declaration order, got {names:?}"
    );
}

#[test]
fn update_no_field_flags_exits_two_with_hint() {
    let repo = make_initialized_repo("update_no_flags");
    let id = create_issue(&repo, "needs flags");

    let out = run_jjf(&repo, &["update", &id]);
    assert!(!out.status.success(), "no-flag update must fail");
    assert_eq!(
        out.status.code(),
        Some(2),
        "no-flag update should exit 2, got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("nothing to update"),
        "stderr should mention `nothing to update`, got: {stderr}"
    );
    // And the hint enumerates the available flags.
    assert!(
        stderr.contains("--title")
            && stderr.contains("--status")
            && stderr.contains("--body-file")
            && stderr.contains("--assignee")
            && stderr.contains("--unset-assignee"),
        "stderr should enumerate the available flags, got: {stderr}"
    );
}

#[test]
fn update_assignee_and_unset_assignee_are_mutually_exclusive() {
    let repo = make_initialized_repo("update_assignee_conflict");
    let id = create_issue(&repo, "conflict");

    let out = run_jjf(
        &repo,
        &[
            "update",
            &id,
            "--assignee",
            "alice",
            "--unset-assignee",
        ],
    );
    assert!(!out.status.success(), "conflicting flags must fail");
    assert_eq!(
        out.status.code(),
        Some(2),
        "conflicting flags should exit 2 (clap), got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn update_json_error_envelope_on_nonexistent_id() {
    // `--json` plus a missing id: the documented `issue_not_found` envelope
    // shows up on stderr. Mirrors the contract pinned for the other
    // bug-id-taking mutators (`close`, `open`, `comment`, `label add/rm`).
    let repo = make_initialized_repo("update_json_err_missing");
    let nonexistent = "deadbee";

    let out = run_jjf(
        &repo,
        &["--json", "update", nonexistent, "--title", "x"],
    );
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(1));
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
        Some("issue_not_found"),
        "kind wrong: {stderr}"
    );
    assert_eq!(
        v["error"]["details"]["id"].as_str(),
        Some(nonexistent),
        "details.id wrong: {stderr}"
    );
}

#[test]
fn update_nonexistent_id_exits_one() {
    let repo = make_initialized_repo("update_missing");
    let nonexistent = "deadbee"; // well-formed but unlikely to collide.

    let out = run_jjf(&repo, &["update", nonexistent, "--title", "x"]);
    assert!(
        !out.status.success(),
        "update on missing id should fail"
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "missing-bug should exit 1 (runtime), got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(nonexistent),
        "stderr should echo the missing id, got: {stderr}"
    );
}

#[test]
fn update_bad_id_exits_two() {
    let repo = make_initialized_repo("update_bad_id");

    for bad in ["short", "GGGGGGG", "12345", "not-a-real-bug-id"] {
        let out = run_jjf(&repo, &["update", bad, "--title", "x"]);
        assert!(!out.status.success(), "update on {bad:?} should fail");
        assert_eq!(
            out.status.code(),
            Some(2),
            "bad id {bad:?} should exit 2, got {:?}; stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr),
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(bad),
            "stderr should echo the bad value {bad:?}, got: {stderr}"
        );
    }
}

#[test]
fn update_in_non_jj_directory_exits_two() {
    let dir = scratch_non_git("update_non_jj");
    let out = run_jjf(&dir, &["update", "abcdef0", "--title", "x"]);
    assert!(!out.status.success(), "update in non-jj dir should fail");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a jj repo"),
        "stderr should mention `not a jj repo`, got: {stderr}"
    );
}

#[test]
fn update_in_jj_repo_without_bugs_bookmark_exits_two_with_init_hint() {
    let repo = make_jj_repo("update_no_bookmark");
    let out = run_jjf(&repo, &["update", "abcdef0", "--title", "x"]);
    assert!(!out.status.success());
    assert_eq!(
        out.status.code(),
        Some(2),
        "missing-bookmark preflight should exit 2, got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("`issues` bookmark") && stderr.contains("iss init"),
        "stderr should tell the user to run `iss init` first, got: {stderr}"
    );
}

#[test]
fn update_unreadable_body_file_exits_two() {
    let repo = make_initialized_repo("update_unreadable_body_file");
    let id = create_issue(&repo, "file-not-found");

    let bogus = repo.join("does-not-exist.md");
    let out = run_jjf(
        &repo,
        &["update", &id, "--body-file", bogus.to_str().unwrap()],
    );
    assert!(!out.status.success(), "unreadable --body-file should fail");
    assert_eq!(
        out.status.code(),
        Some(2),
        "unreadable body file should exit 2, got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn update_help_documents_every_field_flag_and_json() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(ISS_BIN)
        .args(["update", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn jjf update --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("<ID>"),
        "update --help should document <ID>: {help}"
    );
    for flag in [
        "--title",
        "--status",
        "--body-file",
        "--assignee",
        "--unset-assignee",
        "--json",
    ] {
        assert!(
            help.contains(flag),
            "update --help should document `{flag}`, got: {help}"
        );
    }
}
