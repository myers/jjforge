//! Integration tests for `jjf label add <id> <label>` and `jjf label
//! rm <id> <label>` — drive the compiled binary against per-test
//! scratch repos and assert exit code, stdout (plain + `--json`), the
//! round-trip semantics (add-then-rm), the spec-mandated
//! non-idempotency of `label-add`/`label-rm` (a fresh trailer per
//! call regardless of whether the record actually changed), and the
//! preflight / runtime error matrix:
//!
//! - happy add + `show` reports the label (plain),
//! - add-then-rm round trip leaves an empty label set,
//! - `--json` envelope shape (both arms),
//! - double-add lands two `LabelAdd` ops (verified via
//!   `Storage::read_history`) but only one label on the record,
//! - rm of an absent label lands one `LabelRm` op (no-op-still-trails),
//! - empty label string → exit 2,
//! - nonexistent id → exit 1,
//! - bad id → exit 2,
//! - non-jj cwd → exit 2,
//! - jj repo without `issues` bookmark → exit 2 + init hint (both arms),
//! - `--help` documents the positionals + `--json` (both arms).
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as the other
//! test files in this crate. The two non-idempotency tests peek at
//! `Storage::read_history` directly to count `LabelAdd`/`LabelRm`
//! ops, because the CLI doesn't expose history yet (that's
//! `cli-history` territory) and the count is the load-bearing
//! assertion for those acceptance criteria.

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

// --- tests ---------------------------------------------------------

#[test]
fn label_add_happy_path_show_reports_label() {
    let repo = make_initialized_repo("label_add_happy");
    let id = create_issue(&repo, "label me");

    let out = run_jjf(&repo, &["label", "add", &id, "backend"]);
    assert!(
        out.status.success(),
        "jjf label add failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Plain-text output is `label added: <label> -> <id>`.
    assert_eq!(stdout.trim(), format!("label added: backend -> {id}"));

    // And `show` reports the new label in the labels line.
    let out = run_jjf(&repo, &["show", &id]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("labels: backend"),
        "show should report `labels: backend`, got: {stdout}"
    );
}

#[test]
fn label_add_then_rm_round_trip() {
    let repo = make_initialized_repo("label_add_rm_round_trip");
    let id = create_issue(&repo, "round trip");

    // add → show says backend.
    let out = run_jjf(&repo, &["label", "add", &id, "backend"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = run_jjf(&repo, &["show", &id]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("labels: backend"),
        "after add, show should report backend, got: {stdout}"
    );

    // rm → show says (none) — the empty-set sentinel `print_bug_plain`
    // renders.
    let out = run_jjf(&repo, &["label", "rm", &id, "backend"]);
    assert!(
        out.status.success(),
        "jjf label rm failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), format!("label removed: backend -> {id}"));

    let out = run_jjf(&repo, &["show", &id]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("labels: (none)"),
        "after rm, show should report `(none)`, got: {stdout}"
    );
}

#[test]
fn label_add_json_envelope_shape() {
    let repo = make_initialized_repo("label_add_json");
    let id = create_issue(&repo, "json add");

    let out = run_jjf(&repo, &["label", "--json", "add", &id, "frontend"]);
    assert!(
        out.status.success(),
        "jjf label --json add failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("label add --json output must be valid JSON");
    assert_eq!(v["ok"].as_bool(), Some(true), "ok wrong: {stdout}");
    assert_eq!(v["id"].as_str(), Some(id.as_str()), "id wrong: {stdout}");
    assert_eq!(
        v["label"].as_str(),
        Some("frontend"),
        "label wrong: {stdout}"
    );
    assert_eq!(
        v["action"].as_str(),
        Some("added"),
        "action wrong: {stdout}"
    );

    // And the read path agrees.
    let out = run_jjf(&repo, &["show", "--json", &id]);
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    let labels = v["labels"].as_array().expect("labels is array");
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].as_str(), Some("frontend"));
}

#[test]
fn label_rm_json_envelope_shape() {
    // Symmetric test for `rm` — same JSON shape, verb-specific
    // `action` word. Catches a regression where the two arms' JSON
    // payloads drift apart.
    let repo = make_initialized_repo("label_rm_json");
    let id = create_issue(&repo, "json rm");
    // Seed a label so the rm has something to take off.
    let out = run_jjf(&repo, &["label", "add", &id, "tooling"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["label", "--json", "rm", &id, "tooling"]);
    assert!(
        out.status.success(),
        "jjf label --json rm failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("label rm --json output must be valid JSON");
    assert_eq!(v["ok"].as_bool(), Some(true));
    assert_eq!(v["id"].as_str(), Some(id.as_str()));
    assert_eq!(v["label"].as_str(), Some("tooling"));
    assert_eq!(v["action"].as_str(), Some("removed"));
}

#[test]
fn label_double_add_lands_two_trailers_one_label() {
    // Per spec §5.2: adding an already-present label is a no-op at
    // the record level (the label set dedupes) but still lands a
    // fresh `label-add` op so the audit log records the intent. We
    // verify both: the record carries exactly one `backend`, and the
    // history carries exactly two `LabelAdd` entries.
    use jjf_storage::{IssueId, Op, Storage};

    let repo = make_initialized_repo("label_double_add");
    let id = create_issue(&repo, "double add");

    let storage = Storage::open(&repo).expect("Storage::open");
    let issue_id = IssueId::parse(&id).expect("parse id");
    let baseline = storage
        .read_history(&issue_id)
        .expect("read_history")
        .into_iter()
        .filter(|e| matches!(e.op, Op::LabelAdd { .. }))
        .count();
    assert_eq!(
        baseline, 0,
        "fresh bug should have zero LabelAdd ops, got {baseline}"
    );

    // First add.
    let out = run_jjf(&repo, &["label", "add", &id, "backend"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // Same-second guard: see comment on `close_twice_lands_two_set_status_trailers`.
    // Two adds in the same wall-clock second produce a byte-identical
    // JSON record (`updated_at` is second-resolution per spec §3.1),
    // which jj's snapshotter records as no file change — the path-
    // filtered history then misses the second commit.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    // Second add — same label.
    let out = run_jjf(&repo, &["label", "add", &id, "backend"]);
    assert!(
        out.status.success(),
        "second add should still succeed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    let after = storage
        .read_history(&issue_id)
        .expect("read_history")
        .into_iter()
        .filter(|e| matches!(e.op, Op::LabelAdd { .. }))
        .count();
    assert_eq!(
        after, 2,
        "two CLI adds must land two LabelAdd ops (spec non-idempotency), got {after}"
    );

    // Record-level dedupe: one `backend`, not two.
    let bug = storage.read(&issue_id).expect("read");
    let backend_count = bug.labels.iter().filter(|l| *l == "backend").count();
    assert_eq!(
        backend_count, 1,
        "record should carry exactly one `backend` after double-add, got {backend_count}"
    );
}

#[test]
fn label_rm_absent_label_lands_trailer() {
    // Per spec §5.2: removing an absent label is a no-op at the
    // record level but still lands a fresh `label-rm` op. The CLI
    // exits 0 either way.
    use jjf_storage::{IssueId, Op, Storage};

    let repo = make_initialized_repo("label_rm_absent");
    let id = create_issue(&repo, "rm absent");

    let storage = Storage::open(&repo).expect("Storage::open");
    let issue_id = IssueId::parse(&id).expect("parse id");
    let baseline = storage
        .read_history(&issue_id)
        .expect("read_history")
        .into_iter()
        .filter(|e| matches!(e.op, Op::LabelRm { .. }))
        .count();
    assert_eq!(
        baseline, 0,
        "fresh bug should have zero LabelRm ops, got {baseline}"
    );

    // Remove a label that was never added.
    let out = run_jjf(&repo, &["label", "rm", &id, "neverwas"]);
    assert!(
        out.status.success(),
        "rm of absent label should still exit 0: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    let after = storage
        .read_history(&issue_id)
        .expect("read_history")
        .into_iter()
        .filter(|e| matches!(e.op, Op::LabelRm { .. }))
        .count();
    assert_eq!(
        after, 1,
        "no-op rm must still land one LabelRm op (spec §5.2), got {after}"
    );

    // Record's label set stays empty — nothing was there to remove.
    let bug = storage.read(&issue_id).expect("read");
    assert!(
        bug.labels.is_empty(),
        "record labels should stay empty after no-op rm, got {:?}",
        bug.labels
    );
}

#[test]
fn label_json_error_envelope_on_empty_label() {
    // `--json label add <id> ""`: empty-label is the canonical
    // CLI-layer rejection for this verb. The envelope's `kind` is
    // `empty_label`; `details` is absent (the message is enough).
    // Covers both arms transitively — the empty check lives in
    // `run_label` before the LabelOp branch.
    let repo = make_initialized_repo("label_json_err_empty");
    let id = create_issue(&repo, "json envelope empty label");

    let out = run_jjf(&repo, &["--json", "label", "add", &id, ""]);
    assert!(!out.status.success());
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
        Some("empty_label"),
        "kind wrong: {stderr}"
    );
    assert!(
        v["error"].as_object().unwrap().get("details").is_none(),
        "details should be absent for empty_label, got: {stderr}"
    );
}

#[test]
fn label_add_empty_label_exits_two() {
    let repo = make_initialized_repo("label_empty_add");
    let id = create_issue(&repo, "empty label add");

    let out = run_jjf(&repo, &["label", "add", &id, ""]);
    assert!(!out.status.success(), "empty label should fail");
    assert_eq!(
        out.status.code(),
        Some(2),
        "empty label should exit 2, got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("label"),
        "stderr should mention `label`, got: {stderr}"
    );
}

#[test]
fn label_rm_empty_label_exits_two() {
    // Symmetric for `rm` — same validation, same exit. Catches a
    // regression where one arm's empty-label check diverges from the
    // other's.
    let repo = make_initialized_repo("label_empty_rm");
    let id = create_issue(&repo, "empty label rm");

    let out = run_jjf(&repo, &["label", "rm", &id, ""]);
    assert!(!out.status.success(), "empty label should fail");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn label_add_nonexistent_id_exits_one() {
    let repo = make_initialized_repo("label_add_missing");
    let nonexistent = "deadbee"; // 7-hex, well-formed, unlikely to collide.

    let out = run_jjf(&repo, &["label", "add", nonexistent, "backend"]);
    assert!(!out.status.success(), "add on missing id should fail");
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
fn label_add_bad_id_exits_two() {
    let repo = make_initialized_repo("label_add_bad_id");

    for bad in ["short", "GGGGGGG", "12345", "not-a-real-bug-id"] {
        let out = run_jjf(&repo, &["label", "add", bad, "backend"]);
        assert!(!out.status.success(), "add on {bad:?} should fail");
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
fn label_rm_bad_id_exits_two() {
    // Symmetric for `rm`. Same matrix as `add` — bad-id rejection
    // happens before the storage call so both arms must behave the
    // same way.
    let repo = make_initialized_repo("label_rm_bad_id");

    let out = run_jjf(&repo, &["label", "rm", "not-hex", "backend"]);
    assert!(!out.status.success(), "rm on bad id should fail");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn label_in_non_jj_directory_exits_two() {
    let dir = scratch("label_non_jj");
    // Well-formed id so we get past the parse step.
    let out = run_jjf(&dir, &["label", "add", "abcdef0", "backend"]);
    assert!(!out.status.success(), "label in non-jj dir should fail");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a jj repo"),
        "stderr should mention `not a jj repo`, got: {stderr}"
    );
}

#[test]
fn label_add_in_jj_repo_without_bugs_bookmark_exits_two_with_init_hint() {
    let repo = make_jj_repo("label_add_no_bookmark");
    let out = run_jjf(&repo, &["label", "add", "abcdef0", "backend"]);
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
fn label_rm_in_jj_repo_without_bugs_bookmark_exits_two_with_init_hint() {
    // Symmetric for `rm`. Catches a regression where one arm's
    // preflight diverges from the other's.
    let repo = make_jj_repo("label_rm_no_bookmark");
    let out = run_jjf(&repo, &["label", "rm", "abcdef0", "backend"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("`issues` bookmark") && stderr.contains("jjf init"),
        "stderr should tell the user to run `jjf init` first, got: {stderr}"
    );
}

#[test]
fn label_help_documents_subcommands() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
        .args(["label", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn jjf label --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("add"), "label --help should list `add`: {help}");
    assert!(help.contains("rm"), "label --help should list `rm`: {help}");
}

#[test]
fn label_add_help_documents_positionals_and_json() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
        .args(["label", "add", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn jjf label add --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("<ID>"), "add --help should document <ID>: {help}");
    assert!(help.contains("<LABEL>"), "add --help should document <LABEL>: {help}");
    assert!(help.contains("--json"), "add --help should document --json: {help}");
}

#[test]
fn label_rm_help_documents_positionals_and_json() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
        .args(["label", "rm", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn jjf label rm --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("<ID>"), "rm --help should document <ID>: {help}");
    assert!(help.contains("<LABEL>"), "rm --help should document <LABEL>: {help}");
    assert!(help.contains("--json"), "rm --help should document --json: {help}");
}
