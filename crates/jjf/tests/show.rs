//! Integration tests for `jjf show <id>` — drive the compiled binary
//! against per-test scratch repos and assert exit code, stderr, stdout
//! (plain + `--json`), and the error matrix:
//!
//! - happy path (plain + `--json`),
//! - bug not found at the bookmark tip → exit 1,
//! - bad id parse → exit 2,
//! - non-jj repo → exit 2,
//! - jj repo without `issues` bookmark → exit 2 (run `jjf init` first).
//!
//! End-to-end: each happy-path test chains `jjf new` → `jjf show`, so
//! we exercise the full write-then-read cycle through the binary
//! (rather than peeking via `Storage::read` directly the way
//! `tests/new.rs` does for write-side assertions). Same hermetic
//! scratch / no-`assert_cmd` discipline as the other test files.

use std::path::Path;
use std::process::Command;

mod common;
use common::*;

/// Create a bug with the given title/body via `jjf new` and return its
/// freshly-minted id. Centralized so each `show` test reads one line
/// for the setup step and the test body focuses on the assertion.
fn create_issue(
    repo: &Path,
    title: &str,
    body: &[u8],
    extra_args: &[&str],
) -> String {
    let mut args: Vec<&str> = vec!["new", "-t", title, "-F", "-"];
    args.extend_from_slice(extra_args);
    let out = run_jjf_with_stdin(repo, &args, body);
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
fn show_plain_text_includes_every_scalar_field() {
    let repo = make_initialized_repo("show_plain");
    let body = "Steps to reproduce:\n1. open thing\n2. break thing\n";
    let id = create_issue(
        &repo,
        "kernel panic on boot",
        body.as_bytes(),
        &["-l", "bug", "-l", "p1", "-a", "alice"],
    );

    let out = run_jjf(&repo, &["show", &id]);
    assert!(
        out.status.success(),
        "jjf show failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Spot-check every field is present and labelled. The plain-text
    // shape is v1 (not a contract per the ticket), so we look for the
    // values rather than column positions or exact framing.
    assert!(stdout.contains(&id), "id missing from output: {stdout}");
    assert!(stdout.contains("[open]"), "status missing: {stdout}");
    assert!(
        stdout.contains("kernel panic on boot"),
        "title missing: {stdout}"
    );
    assert!(stdout.contains("bug"), "label `bug` missing: {stdout}");
    assert!(stdout.contains("p1"), "label `p1` missing: {stdout}");
    assert!(
        stdout.contains("alice"),
        "assignee missing: {stdout}"
    );
    assert!(
        stdout.contains("Steps to reproduce"),
        "body missing: {stdout}"
    );
    assert!(
        stdout.contains("break thing"),
        "body line 2 missing: {stdout}"
    );
    // The comments header should be present even when there are zero
    // comments — readers can tell "no comments yet" from a v1 bug.
    assert!(
        stdout.contains("comments (0)"),
        "comments section missing: {stdout}"
    );
}

#[test]
fn show_plain_text_renders_none_for_unset_optionals() {
    // No labels, no assignee, empty body. The plain-text renderer
    // should print `(none)` for the optional fields rather than going
    // blank or eliding the line entirely.
    let repo = make_initialized_repo("show_none");
    let id = create_issue(&repo, "bare bug", b"", &[]);

    let out = run_jjf(&repo, &["show", &id]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("labels: (none)"),
        "labels line missing/wrong: {stdout}"
    );
    assert!(
        stdout.contains("assignee: (none)"),
        "assignee line missing/wrong: {stdout}"
    );
    assert!(
        stdout.contains("dependencies: (none)"),
        "dependencies line missing/wrong: {stdout}"
    );
}

#[test]
fn show_json_emits_bug_record_verbatim() {
    let repo = make_initialized_repo("show_json");
    let body = "json body\nwith\nnewlines\n";
    let id = create_issue(
        &repo,
        "json shape",
        body.as_bytes(),
        &["-l", "needs-json", "-a", "bob"],
    );

    let out = run_jjf(&repo, &["show", "--json", &id]);
    assert!(
        out.status.success(),
        "jjf show --json failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("show --json output must be valid JSON");

    // The structured payload is the `Bug` record verbatim — no `ok`
    // envelope. Assert each expected field surfaces.
    assert_eq!(
        v["id"].as_str(),
        Some(id.as_str()),
        "id field wrong: {stdout}"
    );
    assert_eq!(v["title"].as_str(), Some("json shape"));
    assert_eq!(v["status"].as_str(), Some("open"));
    assert_eq!(v["body"].as_str(), Some(body));
    assert_eq!(v["assignee"].as_str(), Some("bob"));
    let labels = v["labels"]
        .as_array()
        .expect("labels must be an array");
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].as_str(), Some("needs-json"));
    // Comments array is present and empty.
    let comments = v["comments"]
        .as_array()
        .expect("comments must be an array");
    assert!(comments.is_empty());
    // Timestamps are present strings (don't assert on the value — they
    // come from the system clock — but assert on shape).
    assert!(
        v["created_at"].as_str().is_some(),
        "created_at missing: {stdout}"
    );
    assert!(
        v["updated_at"].as_str().is_some(),
        "updated_at missing: {stdout}"
    );
}

#[test]
fn show_json_error_envelope_on_nonexistent_id() {
    // `--json` and a well-formed-but-missing id: error envelope on
    // stderr, plain `Bug` JSON nowhere. This is the primary read-verb
    // error contract — pattern-matchable on `kind` rather than message.
    let repo = make_initialized_repo("show_json_err_missing");
    let nonexistent = "deadbee";

    let out = run_jjf(&repo, &["--json", "show", nonexistent]);
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
fn show_with_nonexistent_id_exits_one() {
    // A well-formed but never-created bug id is a runtime failure, not
    // a preflight: the user gave us syntactically-valid input that
    // happens to point at nothing. Exit 1, useful message that echoes
    // the id so the operator can copy-paste-correct.
    let repo = make_initialized_repo("show_missing");
    let nonexistent = "deadbee"; // 7 hex chars, valid shape, unlikely to collide.

    let out = run_jjf(&repo, &["show", nonexistent]);
    assert!(!out.status.success(), "show on missing id should fail");
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
fn show_with_bad_id_exits_two() {
    // Bad id parse — too short, uppercase, non-hex — is a preflight
    // failure (exit 2). We assert each shape individually so a regression
    // that loosens the id parser (or tightens it differently) is caught.
    let repo = make_initialized_repo("show_bad_id");

    for bad in ["short", "GGGGGGG", "12345", "not-a-real-bug-id"] {
        let out = run_jjf(&repo, &["show", bad]);
        assert!(!out.status.success(), "show on {bad:?} should fail");
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
fn show_in_non_jj_directory_exits_two() {
    let dir = scratch("show_non_jj");

    // Use a well-formed id so we get past the parse step and exercise
    // the preflight, not the BadIssueId branch.
    let out = run_jjf(&dir, &["show", "abcdef0"]);
    assert!(!out.status.success(), "show in non-jj dir should fail");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a jj repo"),
        "stderr should mention `not a jj repo`, got: {stderr}"
    );
}

#[test]
fn show_in_jj_repo_without_bugs_bookmark_exits_two_with_init_hint() {
    // Fresh jj repo, no `jjf init` yet — the missing-bookmark probe
    // should fire with the typed `run jjf init first` message rather
    // than the raw jj stderr from a downstream read attempt.
    let repo = make_jj_repo("show_no_bookmark");

    let out = run_jjf(&repo, &["show", "abcdef0"]);
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
        stderr.contains("`issues` bookmark") && stderr.contains("jjf init"),
        "stderr should tell the user to run `jjf init` first, got: {stderr}"
    );
}

#[test]
fn show_help_documents_positional_and_json_flag() {
    // `--help` should mention the id positional and the --json flag.
    // Keeps the public surface stable against accidental renames.
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
        .args(["show", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn jjf show --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("<ID>") || help.contains("<id>"),
        "show --help should document the positional ID, got: {help}"
    );
    assert!(
        help.contains("--json"),
        "show --help should document --json, got: {help}"
    );
}
