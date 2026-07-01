//! Integration tests for `iss block <id> [--reason <text>]` and
//! `iss unblock <id>` (v2.5 — `agent-await-gates-impl`). Drives the
//! compiled binary against per-test scratch repos and asserts:
//!
//! - happy path: block + show reports `[blocked]` and the
//!   `block-reason:` line.
//! - `--json` envelope shape (`ok`, `id`, `status: "blocked"`,
//!   `reason`, `blocked: true`).
//! - unblock round-trip (status back to open, reason cleared).
//! - `iss ready` excludes blocked issues by default.
//! - `iss ready --include-blocked` re-includes them.
//! - `iss ls --status blocked` filters correctly.
//! - multi-line reason → exit 1 (`invalid_input` from storage).
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as the other
//! test files in this crate.

use std::path::Path;

mod common;
use common::*;

/// Create an issue via `iss new`; return its id.
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
fn block_happy_path_show_reports_blocked_and_reason() {
    let repo = make_initialized_repo("block_happy");
    let id = create_issue(&repo, "park me");

    let out = run_jjf(&repo, &["block", &id, "--reason", "waiting on PR-42"]);
    assert!(
        out.status.success(),
        "jjf block failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), format!("blocked {id}: waiting on PR-42"));

    // `show` reports `[blocked]` and the `block-reason:` line.
    let out = run_jjf(&repo, &["show", &id]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[blocked]"),
        "show should report [blocked]: {stdout}"
    );
    assert!(
        stdout.contains("block-reason: waiting on PR-42"),
        "show should render block-reason line: {stdout}"
    );
}

#[test]
fn block_json_envelope_shape() {
    let repo = make_initialized_repo("block_json");
    let id = create_issue(&repo, "json park");

    let out = run_jjf(
        &repo,
        &["block", "--json", &id, "--reason", "waiting on signal"],
    );
    assert!(
        out.status.success(),
        "jjf block --json failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("block --json output must be valid JSON");
    assert_eq!(v["ok"].as_bool(), Some(true), "ok field wrong: {stdout}");
    assert_eq!(
        v["id"].as_str(),
        Some(id.as_str()),
        "id field wrong: {stdout}"
    );
    assert_eq!(
        v["status"].as_str(),
        Some("blocked"),
        "status field wrong: {stdout}"
    );
    assert_eq!(
        v["reason"].as_str(),
        Some("waiting on signal"),
        "reason field wrong: {stdout}"
    );
    assert_eq!(v["blocked"].as_bool(), Some(true), "blocked flag wrong");
}

#[test]
fn block_then_unblock_round_trip() {
    let repo = make_initialized_repo("block_unblock_roundtrip");
    let id = create_issue(&repo, "flip-flop");

    let out = run_jjf(&repo, &["block", &id, "--reason", "wait"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = run_jjf(&repo, &["show", &id]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[blocked]"), "{stdout}");
    assert!(stdout.contains("block-reason: wait"), "{stdout}");

    let out = run_jjf(&repo, &["unblock", &id]);
    assert!(
        out.status.success(),
        "jjf unblock failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), format!("unblocked {id}"));

    let out = run_jjf(&repo, &["show", &id]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[open]"),
        "show should report [open]: {stdout}"
    );
    assert!(
        !stdout.contains("block-reason:"),
        "block-reason line should be gone after unblock: {stdout}"
    );
}

#[test]
fn ready_excludes_blocked_by_default() {
    let repo = make_initialized_repo("ready_excludes_blocked_cli");
    let id_a = create_issue(&repo, "A");
    let _id_b = create_issue(&repo, "B");

    let out = run_jjf(&repo, &["block", &id_a, "--reason", "park"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["ready"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains(&id_a),
        "blocked id_a must not appear in `iss ready`: stdout={stdout}"
    );
}

#[test]
fn ready_include_blocked_re_includes_blocked() {
    let repo = make_initialized_repo("ready_include_blocked_cli");
    let id_a = create_issue(&repo, "A");
    let _id_b = create_issue(&repo, "B");
    let out = run_jjf(&repo, &["block", &id_a, "--reason", "park"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["ready", "--include-blocked"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&id_a),
        "id_a should be included with --include-blocked: stdout={stdout}"
    );
}

#[test]
fn ls_status_blocked_filters_correctly() {
    let repo = make_initialized_repo("ls_blocked_filter");
    let id_a = create_issue(&repo, "A");
    let id_b = create_issue(&repo, "B");
    let out = run_jjf(&repo, &["block", &id_a, "--reason", "park"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["ls", "--status", "blocked"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&id_a), "id_a should be in blocked list: {stdout}");
    assert!(!stdout.contains(&id_b), "id_b should NOT be in blocked list: {stdout}");
}

#[test]
fn block_multiline_reason_errors_invalid_input() {
    let repo = make_initialized_repo("block_multiline_rejected");
    let id = create_issue(&repo, "x");

    let out = run_jjf(&repo, &["block", &id, "--reason", "line one\nline two"]);
    assert!(
        !out.status.success(),
        "multi-line reason should fail; got success: stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    // Storage::block surfaces `Error::Invalid`; CLI maps to exit 1
    // (`invalid_input`).
    assert_eq!(out.status.code(), Some(1), "expected exit 1");
}

#[test]
fn ready_json_envelope_with_blocked_visible_carries_reason() {
    // When --include-blocked surfaces a blocked issue under --json,
    // the `block_reason` field is present on the Issue payload so a
    // script can read it without `iss show`.
    let repo = make_initialized_repo("ready_json_blocked_reason");
    let id_a = create_issue(&repo, "A");
    let out = run_jjf(&repo, &["block", &id_a, "--reason", "waiting on x"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["ready", "--json", "--include-blocked"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: serde_json::Value =
        serde_json::from_str(&stdout).expect("ready --json must be valid JSON");
    let arr = arr.as_array().expect("ready --json returns array");
    let entry = arr
        .iter()
        .find(|v| v["id"].as_str() == Some(id_a.as_str()))
        .expect("blocked id_a should appear in --include-blocked --json output");
    assert_eq!(entry["status"].as_str(), Some("blocked"));
    assert_eq!(entry["block_reason"].as_str(), Some("waiting on x"));
}
