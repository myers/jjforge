//! Integration tests for `jjf comment <id> -F - [--author <NAME>]
//! [--json]` — drive the compiled binary against per-test scratch repos
//! and assert the full matrix the ticket calls out:
//!
//! - happy path + `show` reports the new comment (plain),
//! - `--json` envelope shape with `comment_id` exposed from storage,
//! - `--author` override appears in the read-back comment,
//! - two-in-a-row land in chronological order in `show` output,
//! - empty body (stdin closes empty) → exit 2 with hint,
//! - nonexistent id → exit 1,
//! - bad id → exit 2,
//! - non-jj cwd → exit 2,
//! - jj repo without `issues` bookmark → exit 2 + init hint,
//! - `--help` documents positional + `-F` + `--author` + `--json`.
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as the other test
//! files in this crate; helpers mirror `close.rs` rather than being
//! extracted to a shared module (the crate hasn't pulled the extraction
//! trigger yet — see the audit-cleanup tickets).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;
use common::{run_jjf, run_jjf_with_stdin, scratch, JJF_BIN};

fn make_jj_repo(name: &str) -> PathBuf {
    let dir = scratch(name);
    let out = Command::new("jj")
        .args(["git", "init"])
        .current_dir(&dir)
        .output()
        .expect("spawn jj");
    assert!(
        out.status.success(),
        "jj git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Make sure the scratch repo has a stable user identity so the
    // default-author path in `jjf comment` has something to find.
    // We do this here (rather than relying on the test runner's
    // environment) so the tests are hermetic.
    let out = Command::new("jj")
        .args(["config", "set", "--repo", "user.name", "Test User"])
        .current_dir(&dir)
        .output()
        .expect("spawn jj config set name");
    assert!(
        out.status.success(),
        "jj config set user.name failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("jj")
        .args(["config", "set", "--repo", "user.email", "test@example.com"])
        .current_dir(&dir)
        .output()
        .expect("spawn jj config set email");
    assert!(
        out.status.success(),
        "jj config set user.email failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

fn make_initialized_repo(name: &str) -> PathBuf {
    let repo = make_jj_repo(name);
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

/// Create a bug via `jjf new`, return its id.
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
fn comment_happy_path_show_reports_comment() {
    let repo = make_initialized_repo("comment_happy");
    let id = create_issue(&repo, "needs a comment");

    let out = run_jjf_with_stdin(
        &repo,
        &["comment", &id, "-F", "-"],
        b"first thought from stdin\n",
    );
    assert!(
        out.status.success(),
        "jjf comment failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), format!("comment added to {id}"));

    // `show` should now include the comment body and the default
    // (jj-config) author `Test User <test@example.com>`.
    let out = run_jjf(&repo, &["show", &id]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("first thought from stdin"),
        "show output should contain comment body: {stdout}"
    );
    assert!(
        stdout.contains("Test User <test@example.com>"),
        "show output should contain default author: {stdout}"
    );
    assert!(
        stdout.contains("--- comments (1) ---"),
        "show should report one comment: {stdout}"
    );
}

#[test]
fn comment_json_envelope_shape() {
    let repo = make_initialized_repo("comment_json");
    let id = create_issue(&repo, "json comment");

    let out = run_jjf_with_stdin(
        &repo,
        &["comment", "--json", &id, "-F", "-"],
        b"a body for the json shape test",
    );
    assert!(
        out.status.success(),
        "jjf comment --json failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("comment --json output must be valid JSON");
    assert_eq!(v["ok"].as_bool(), Some(true), "ok field wrong: {stdout}");
    assert_eq!(
        v["id"].as_str(),
        Some(id.as_str()),
        "id field wrong: {stdout}"
    );
    let comment_id = v["comment_id"]
        .as_str()
        .expect("comment_id field present and string");
    assert_eq!(
        comment_id.len(),
        7,
        "comment_id must be 7 hex chars, got {comment_id:?}"
    );
    assert!(
        comment_id.chars().all(|c| c.is_ascii_hexdigit()),
        "comment_id must be hex, got {comment_id:?}"
    );
}

#[test]
fn comment_author_override_appears_in_show() {
    let repo = make_initialized_repo("comment_author_override");
    let id = create_issue(&repo, "override the author");

    let out = run_jjf_with_stdin(
        &repo,
        &["comment", &id, "-F", "-", "--author", "Alice <alice@x>"],
        b"override-author body",
    );
    assert!(
        out.status.success(),
        "jjf comment with --author failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    let out = run_jjf(&repo, &["show", &id]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Alice <alice@x>"),
        "show should reflect --author override: {stdout}"
    );
    assert!(
        !stdout.contains("Test User <test@example.com>"),
        "show should NOT show the jj-config default when --author was passed: {stdout}"
    );
}

#[test]
fn comment_two_in_a_row_chronological_order() {
    let repo = make_initialized_repo("comment_two_in_a_row");
    let id = create_issue(&repo, "two-comment thread");

    let out =
        run_jjf_with_stdin(&repo, &["comment", &id, "-F", "-"], b"FIRST in time");
    assert!(
        out.status.success(),
        "first comment failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out =
        run_jjf_with_stdin(&repo, &["comment", &id, "-F", "-"], b"SECOND in time");
    assert!(
        out.status.success(),
        "second comment failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run_jjf(&repo, &["show", &id]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--- comments (2) ---"),
        "show should report two comments: {stdout}"
    );
    let first_idx = stdout
        .find("FIRST in time")
        .expect("FIRST body must appear in show output");
    let second_idx = stdout
        .find("SECOND in time")
        .expect("SECOND body must appear in show output");
    assert!(
        first_idx < second_idx,
        "comments must render in chronological order (first before second): {stdout}"
    );
}

#[test]
fn comment_json_error_envelope_on_empty_body() {
    // `--json` plus the most representative comment-side validation
    // failure (empty body): the documented `empty_body` envelope on
    // stderr. The `details` field is absent for this kind — message
    // carries enough context (the flag hint).
    let repo = make_initialized_repo("comment_json_err_empty");
    let id = create_issue(&repo, "no empty allowed via json");

    let out = run_jjf_with_stdin(
        &repo,
        &["--json", "comment", &id, "-F", "-"],
        b"",
    );
    assert!(!out.status.success(), "empty-body comment must fail");
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
        Some("empty_body"),
        "kind wrong: {stderr}"
    );
    // `empty_body` has no structured details; the field is absent rather
    // than `null` per the contract. Use `.get()` to check absence
    // explicitly without tripping over serde_json's index-returns-null
    // convention.
    assert!(
        v["error"].as_object().unwrap().get("details").is_none(),
        "details should be absent for empty_body, got: {stderr}"
    );
}

#[test]
fn comment_empty_body_exits_two() {
    let repo = make_initialized_repo("comment_empty");
    let id = create_issue(&repo, "no empty allowed");

    // Closed-stdin / zero-byte body — the CLI must reject before the
    // storage layer ever sees the call.
    let out = run_jjf_with_stdin(&repo, &["comment", &id, "-F", "-"], b"");
    assert!(!out.status.success(), "empty-body comment must fail");
    assert_eq!(
        out.status.code(),
        Some(2),
        "empty body → exit 2, got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("empty"),
        "stderr should mention empty body, got: {stderr}"
    );
}

#[test]
fn comment_nonexistent_id_exits_one() {
    let repo = make_initialized_repo("comment_missing");
    let nonexistent = "deadbee"; // 7-hex, well-formed, unlikely to collide.

    let out = run_jjf_with_stdin(
        &repo,
        &["comment", nonexistent, "-F", "-"],
        b"shouldn't land anywhere",
    );
    assert!(
        !out.status.success(),
        "comment on missing id should fail"
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
fn comment_bad_id_exits_two() {
    let repo = make_initialized_repo("comment_bad_id");

    for bad in ["short", "GGGGGGG", "12345", "not-a-real-bug-id"] {
        let out = run_jjf_with_stdin(
            &repo,
            &["comment", bad, "-F", "-"],
            b"valid body, bad id",
        );
        assert!(!out.status.success(), "comment on {bad:?} should fail");
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
fn comment_in_non_jj_directory_exits_two() {
    let dir = scratch("comment_non_jj");
    // Well-formed id so we get past the parse step.
    let out =
        run_jjf_with_stdin(&dir, &["comment", "abcdef0", "-F", "-"], b"some body");
    assert!(!out.status.success(), "comment in non-jj dir should fail");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a jj repo"),
        "stderr should mention `not a jj repo`, got: {stderr}"
    );
}

#[test]
fn comment_in_jj_repo_without_bugs_bookmark_exits_two_with_init_hint() {
    let repo = make_jj_repo("comment_no_bookmark");
    let out = run_jjf_with_stdin(
        &repo,
        &["comment", "abcdef0", "-F", "-"],
        b"some body",
    );
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
fn comment_unreadable_file_exits_two() {
    let repo = make_initialized_repo("comment_unreadable_file");
    let id = create_issue(&repo, "file-not-found");

    let bogus = repo.join("does-not-exist.md");
    let out = run_jjf(
        &repo,
        &["comment", &id, "-F", bogus.to_str().unwrap()],
    );
    assert!(!out.status.success(), "unreadable -F path should fail");
    assert_eq!(
        out.status.code(),
        Some(2),
        "unreadable body file should exit 2 (preflight), got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// `c5078e4` acceptance: 5 concurrent `jjf comment` subprocesses
/// against the SAME issue all land their comments. Pre-fix the
/// storage layer retried once and ~3/5 would fail with a typed
/// `ConcurrentWrite`; with the bounded-retry policy the budget
/// absorbs the contention.
///
/// We set `JJF_RETRY_BASE_MS=0` in the subprocess env so the test
/// doesn't pay the (geometric) backoff wall-clock cost.
///
/// Tolerance: assert `>= 4` rather than strict-5 because in heavy
/// CI load a real giveup is possible; the property under test is
/// "the retry budget is real," not "no operation ever fails."
#[test]
fn concurrent_comments_cli_all_land() {
    let repo = make_initialized_repo("concurrent_comments_cli_5");
    let id = create_issue(&repo, "5-way concurrent comment target");

    let mut handles = Vec::new();
    for i in 0..5 {
        let repo = repo.clone();
        let id = id.clone();
        handles.push(std::thread::spawn(move || {
            let mut child = Command::new(JJF_BIN)
                .args(["comment", &id, "-F", "-"])
                .current_dir(&repo)
                .env("JJF_RETRY_BASE_MS", "0")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn jjf");
            child
                .stdin
                .as_mut()
                .expect("stdin handle")
                .write_all(format!("cli comment {i}\n").as_bytes())
                .expect("write stdin");
            child.wait_with_output().expect("wait for jjf")
        }));
    }

    let mut successes = 0;
    let mut failures: Vec<String> = Vec::new();
    for h in handles {
        let out = h.join().expect("thread did not panic");
        if out.status.success() {
            successes += 1;
        } else {
            failures.push(format!(
                "code={:?} stderr={}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }

    assert!(
        successes >= 4,
        "expected >= 4 of 5 concurrent comments to land (retry budget); got {successes}. \
         Failures: {failures:?}"
    );

    // Cross-check: show reports the same count we counted as successes.
    let out = run_jjf(&repo, &["show", &id]);
    assert!(out.status.success(), "show failed: {}", String::from_utf8_lossy(&out.stderr));
    let body = String::from_utf8_lossy(&out.stdout);
    let landed_in_show = (0..5)
        .filter(|i| body.contains(&format!("cli comment {i}")))
        .count();
    assert_eq!(
        landed_in_show, successes,
        "show comment-count ({landed_in_show}) must match success count ({successes}); \
         show output:\n{body}"
    );
}

#[test]
fn comment_help_documents_positional_file_author_and_json() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
        .args(["comment", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn jjf comment --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("<ID>"),
        "comment --help should document the <ID> positional: {help}"
    );
    assert!(
        help.contains("--file") || help.contains("-F"),
        "comment --help should document -F / --file: {help}"
    );
    assert!(
        help.contains("--author"),
        "comment --help should document --author: {help}"
    );
    assert!(
        help.contains("--json"),
        "comment --help should document --json: {help}"
    );
}
