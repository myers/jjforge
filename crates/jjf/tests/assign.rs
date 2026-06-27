//! Integration tests for `jjf assign <id> <name>` — drive the compiled
//! binary against per-test scratch repos and assert exit code, stdout
//! (plain + `--json`), the round-trip semantics (set / clear / re-set),
//! and the preflight / runtime error matrix:
//!
//! - happy set + `show --json` reports `"assignee": "alice"`,
//! - empty `name` clears (round-trips to `null`),
//! - `--json` envelope shape on both set and unset,
//! - nonexistent id → exit 1 with `issue_not_found`,
//! - newline-bearing name → exit 1 with `invalid_input`.
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as the other test
//! files in this crate (see e.g. `close.rs`).

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
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn jjf")
}

fn run_jjf_with_stdin(cwd: &Path, args: &[&str], stdin_bytes: &[u8]) -> Output {
    let mut child = Command::new(JJF_BIN)
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

fn show_assignee(repo: &Path, id: &str) -> serde_json::Value {
    let out = run_jjf(repo, &["show", "--json", id]);
    assert!(
        out.status.success(),
        "jjf show --json failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("show --json valid");
    v["assignee"].clone()
}

// --- tests ---------------------------------------------------------

#[test]
fn assign_happy_path_sets_assignee() {
    let repo = make_initialized_repo("assign_happy");
    let id = create_issue(&repo, "assign me");

    // Baseline — fresh issue is unassigned.
    assert_eq!(show_assignee(&repo, &id), serde_json::Value::Null);

    let out = run_jjf(&repo, &["assign", &id, "alice"]);
    assert!(
        out.status.success(),
        "jjf assign failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), format!("assigned {id} to alice"));

    // Round-trip via `show --json`.
    assert_eq!(
        show_assignee(&repo, &id),
        serde_json::Value::String("alice".into())
    );
}

#[test]
fn assign_empty_name_clears_assignee() {
    let repo = make_initialized_repo("assign_clear");
    let id = create_issue(&repo, "assigned then cleared");

    // Set first so we have something to clear.
    let out = run_jjf(&repo, &["assign", &id, "bob"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        show_assignee(&repo, &id),
        serde_json::Value::String("bob".into())
    );

    // Now clear with empty name.
    // Sleep just past a wall-clock second so the second mutation's
    // `updated_at` is distinguishable from the first — same dance
    // close.rs's non-idempotency test does. Without this jj's
    // snapshotter can elide the second commit when the JSON shape
    // happens to byte-match a previous commit.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let out = run_jjf(&repo, &["assign", &id, ""]);
    assert!(
        out.status.success(),
        "jjf assign <id> \"\" failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), format!("unassigned {id}"));

    assert_eq!(show_assignee(&repo, &id), serde_json::Value::Null);
}

#[test]
fn assign_json_envelope_shape_on_set() {
    let repo = make_initialized_repo("assign_json_set");
    let id = create_issue(&repo, "json assign");

    let out = run_jjf(&repo, &["assign", "--json", &id, "carol"]);
    assert!(
        out.status.success(),
        "jjf assign --json failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("assign --json must be valid JSON");
    assert_eq!(v["ok"].as_bool(), Some(true), "ok wrong: {stdout}");
    assert_eq!(v["id"].as_str(), Some(id.as_str()), "id wrong: {stdout}");
    assert_eq!(
        v["assignee"].as_str(),
        Some("carol"),
        "assignee wrong: {stdout}"
    );

    // And the read path agrees.
    assert_eq!(
        show_assignee(&repo, &id),
        serde_json::Value::String("carol".into())
    );
}

#[test]
fn assign_json_envelope_shape_on_unset() {
    let repo = make_initialized_repo("assign_json_unset");
    let id = create_issue(&repo, "json unassign");

    // Plant an assignee first.
    let out = run_jjf(&repo, &["assign", &id, "dave"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Clear with `--json` empty name — envelope must carry
    // explicit `null`, NOT omit the field.
    let out = run_jjf(&repo, &["assign", "--json", &id, ""]);
    assert!(
        out.status.success(),
        "jjf assign --json <id> \"\" failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("assign --json must be valid JSON");
    assert_eq!(v["ok"].as_bool(), Some(true));
    assert_eq!(v["id"].as_str(), Some(id.as_str()));
    assert_eq!(
        v["assignee"],
        serde_json::Value::Null,
        "assignee field must be explicit null on unset, got: {stdout}"
    );
}

#[test]
fn assign_nonexistent_id_surfaces_issue_not_found() {
    let repo = make_initialized_repo("assign_missing");
    let nonexistent = "deadbee";

    let out = run_jjf(&repo, &["--json", "assign", nonexistent, "alice"]);
    assert!(!out.status.success(), "assign on missing id should fail");
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
fn assign_name_with_newline_surfaces_invalid_input() {
    // The storage layer guards against newlines in assignee
    // (`qa-trailer-injection`, issue `a902492`). The CLI shim
    // doesn't pre-validate — it lets the typed error bubble
    // up as `invalid_input` (exit 1).
    let repo = make_initialized_repo("assign_newline_name");
    let id = create_issue(&repo, "newline name");

    let out = run_jjf(&repo, &["--json", "assign", &id, "alice\nevil"]);
    assert!(!out.status.success(), "newline-in-name should fail");
    assert_eq!(
        out.status.code(),
        Some(1),
        "newline-in-name should exit 1 (runtime, from storage), got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("stderr must be valid JSON envelope");
    assert_eq!(v["ok"], serde_json::Value::Bool(false));
    assert_eq!(
        v["error"]["kind"].as_str(),
        Some("invalid_input"),
        "kind wrong: {stderr}"
    );

    // The issue should still be unassigned — storage rejected before
    // any write landed.
    assert_eq!(show_assignee(&repo, &id), serde_json::Value::Null);
}

#[test]
fn assign_help_documents_positionals_and_json() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
        .args(["assign", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn jjf assign --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("<ID>"),
        "assign --help should document the <ID> positional: {help}"
    );
    assert!(
        help.contains("<NAME>"),
        "assign --help should document the <NAME> positional: {help}"
    );
    assert!(
        help.contains("--json"),
        "assign --help should document --json: {help}"
    );
}
