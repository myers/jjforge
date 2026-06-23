//! Integration tests for `jjf close <id>` and `jjf open <id>` — drive
//! the compiled binary against per-test scratch repos and assert exit
//! code, stdout (plain + `--json`), the round-trip semantics
//! (close-then-open, open-then-close), the spec-mandated
//! non-idempotency of `set-status` (a fresh trailer per call), and the
//! preflight / runtime error matrix:
//!
//! - happy close + `show` reports `closed` (plain),
//! - `--json` envelope shape,
//! - close-then-open round-trip,
//! - close-twice lands two trailers (verified via `Storage::read_history`),
//! - nonexistent id → exit 1,
//! - bad id → exit 2,
//! - non-jj cwd → exit 2,
//! - jj repo without `issues` bookmark → exit 2 + init hint,
//! - `--help` documents the positional + `--json`.
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as the other
//! test files in this crate. The one departure: the non-idempotency
//! test peeks at `Storage::read_history` directly to count
//! `set-status` ops, because the CLI doesn't expose history yet
//! (that's `cli-history` territory) and the count is the
//! load-bearing assertion for that acceptance criterion.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const JJF_BIN: &str = env!("CARGO_BIN_EXE_jjf");

/// Per-test scratch root. Gitignored via the workspace-level rule.
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
    dir
}

fn make_initialized_repo(name: &str) -> PathBuf {
    let repo = make_jj_repo(name);
    let out = Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
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

fn run_jjf(cwd: &Path, args: &[&str]) -> Output {
    Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn jjf")
}

fn run_jjf_with_stdin(cwd: &Path, args: &[&str], stdin_bytes: &[u8]) -> Output {
    let mut child = Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn jjf");
    child
        .stdin
        .as_mut()
        .expect("stdin handle")
        .write_all(stdin_bytes)
        .expect("write stdin");
    child.wait_with_output().expect("wait for jjf")
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
fn close_happy_path_show_reports_closed() {
    let repo = make_initialized_repo("close_happy");
    let id = create_issue(&repo, "to be closed");

    let out = run_jjf(&repo, &["close", &id]);
    assert!(
        out.status.success(),
        "jjf close failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Plain-text output is `closed <id>` — one line, no decoration.
    assert_eq!(stdout.trim(), format!("closed {id}"));

    // And `show` reports the new status.
    let out = run_jjf(&repo, &["show", &id]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[closed]"),
        "show should report [closed] after jjf close: {stdout}"
    );
}

#[test]
fn close_json_envelope_shape() {
    let repo = make_initialized_repo("close_json");
    let id = create_issue(&repo, "json close");

    let out = run_jjf(&repo, &["close", "--json", &id]);
    assert!(
        out.status.success(),
        "jjf close --json failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("close --json output must be valid JSON");
    assert_eq!(v["ok"].as_bool(), Some(true), "ok field wrong: {stdout}");
    assert_eq!(
        v["id"].as_str(),
        Some(id.as_str()),
        "id field wrong: {stdout}"
    );
    assert_eq!(
        v["status"].as_str(),
        Some("closed"),
        "status field wrong: {stdout}"
    );

    // And the read path agrees.
    let out = run_jjf(&repo, &["show", "--json", &id]);
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(v["status"].as_str(), Some("closed"));
}

#[test]
fn close_then_open_round_trip() {
    let repo = make_initialized_repo("close_then_open");
    let id = create_issue(&repo, "flip-flop");

    // close → show says closed.
    let out = run_jjf(&repo, &["close", &id]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = run_jjf(&repo, &["show", &id]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("[closed]"));

    // open → show says open. Plain-text output for `open` is
    // `opened <id>` (past tense matches `closed <id>`).
    let out = run_jjf(&repo, &["open", &id]);
    assert!(
        out.status.success(),
        "jjf open failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), format!("opened {id}"));

    let out = run_jjf(&repo, &["show", &id]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[open]"),
        "show should report [open] after jjf open: {stdout}"
    );
}

#[test]
fn open_json_envelope_shape() {
    // Same shape as close, but verifies the verb-specific status word
    // and that opening an already-open bug succeeds (the
    // non-idempotency test below covers the trailer-counting case;
    // this one is purely the JSON shape).
    let repo = make_initialized_repo("open_json");
    let id = create_issue(&repo, "json open");

    let out = run_jjf(&repo, &["open", "--json", &id]);
    assert!(
        out.status.success(),
        "jjf open --json failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("open --json output must be valid JSON");
    assert_eq!(v["ok"].as_bool(), Some(true));
    assert_eq!(v["id"].as_str(), Some(id.as_str()));
    assert_eq!(v["status"].as_str(), Some("open"));
}

#[test]
fn close_twice_lands_two_set_status_trailers() {
    // Per the spec: closing an already-closed bug is NOT a no-op — it
    // lands a fresh `set-status` op so the audit log records the
    // intent. We verify by counting `SetStatus` entries in the bug's
    // history.
    use jjf_storage::{IssueId, Op, Storage};

    let repo = make_initialized_repo("close_twice");
    let id = create_issue(&repo, "close me twice");

    // Baseline: zero set-status ops (just the create-time multi-op).
    let storage = Storage::open(&repo).expect("Storage::open");
    let issue_id = IssueId::parse(&id).expect("parse id");
    let baseline = storage
        .read_history(&issue_id)
        .expect("read_history")
        .into_iter()
        .filter(|e| matches!(e.op, Op::SetStatus { .. }))
        .count();
    assert_eq!(
        baseline, 0,
        "fresh bug should have zero set-status ops, got {baseline}"
    );

    // First close.
    let out = run_jjf(&repo, &["close", &id]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // Same-second guard: the writer stamps `updated_at` at second
    // resolution (spec §3.1). If two `close`s land in the same wall-
    // clock second, the JSON record is byte-identical, jj's snapshotter
    // records no file change, and the path-filtered `read_history`
    // misses the second commit. Sleep just past a second boundary so
    // the second close gets a distinguishable file delta. See spec
    // §5.6 "same-second collision" note.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    // Second close — same bug, same target status.
    let out = run_jjf(&repo, &["close", &id]);
    assert!(
        out.status.success(),
        "second close should still succeed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    let after = storage
        .read_history(&issue_id)
        .expect("read_history")
        .into_iter()
        .filter(|e| matches!(e.op, Op::SetStatus { .. }))
        .count();
    assert_eq!(
        after, 2,
        "two CLI closes must land two set-status ops (spec non-idempotency), got {after}"
    );
}

#[test]
fn close_json_error_envelope_on_nonexistent_id() {
    // `--json close <missing>`: the documented `issue_not_found` envelope.
    // Same shape as `update`'s and `comment`'s nonexistent-id envelope;
    // `open` runs through the same `run_set_status` code path and is
    // covered transitively. The matching test for `open` lives below.
    let repo = make_initialized_repo("close_json_err_missing");
    let nonexistent = "deadbee";

    let out = run_jjf(&repo, &["--json", "close", nonexistent]);
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
fn open_json_error_envelope_on_nonexistent_id() {
    // `open` shares `run_set_status` with `close`, but we pin its
    // envelope shape too so a future refactor that splits the verbs
    // can't silently regress one of them.
    let repo = make_initialized_repo("open_json_err_missing");
    let nonexistent = "deadbee";

    let out = run_jjf(&repo, &["--json", "open", nonexistent]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("stderr must be valid JSON envelope");
    assert_eq!(v["ok"], serde_json::Value::Bool(false));
    assert_eq!(v["error"]["kind"].as_str(), Some("issue_not_found"));
    assert_eq!(
        v["error"]["details"]["id"].as_str(),
        Some(nonexistent),
    );
}

#[test]
fn close_nonexistent_id_exits_one() {
    let repo = make_initialized_repo("close_missing");
    let nonexistent = "deadbee"; // 7-hex, well-formed, unlikely to collide.

    let out = run_jjf(&repo, &["close", nonexistent]);
    assert!(!out.status.success(), "close on missing id should fail");
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
fn close_bad_id_exits_two() {
    let repo = make_initialized_repo("close_bad_id");

    // Same shape matrix as `show` — short, uppercase, non-hex.
    for bad in ["short", "GGGGGGG", "12345", "not-a-real-bug-id"] {
        let out = run_jjf(&repo, &["close", bad]);
        assert!(!out.status.success(), "close on {bad:?} should fail");
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
fn close_in_non_jj_directory_exits_two() {
    let dir = scratch("close_non_jj");
    // Well-formed id so we get past the parse step.
    let out = run_jjf(&dir, &["close", "abcdef0"]);
    assert!(!out.status.success(), "close in non-jj dir should fail");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a jj repo"),
        "stderr should mention `not a jj repo`, got: {stderr}"
    );
}

#[test]
fn close_in_jj_repo_without_bugs_bookmark_exits_two_with_init_hint() {
    let repo = make_jj_repo("close_no_bookmark");
    let out = run_jjf(&repo, &["close", "abcdef0"]);
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
fn open_in_jj_repo_without_bugs_bookmark_exits_two_with_init_hint() {
    // Symmetric test for `open` — same preflight, same exit, same
    // hint. Catches a regression where one verb's preflight diverges
    // from the other's.
    let repo = make_jj_repo("open_no_bookmark");
    let out = run_jjf(&repo, &["open", "abcdef0"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("`issues` bookmark") && stderr.contains("jjf init"),
        "stderr should tell the user to run `jjf init` first, got: {stderr}"
    );
}

#[test]
fn close_help_documents_positional_and_json() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
        .args(["close", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn jjf close --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("<ID>"), "close --help should document the <ID> positional: {help}");
    assert!(help.contains("--json"), "close --help should document --json: {help}");
}

#[test]
fn open_help_documents_positional_and_json() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
        .args(["open", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn jjf open --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("<ID>"), "open --help should document the <ID> positional: {help}");
    assert!(help.contains("--json"), "open --help should document --json: {help}");
}
