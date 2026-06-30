//! Integration tests for the four `jjf` memory verbs (`remember`,
//! `memories`, `recall`, `forget`) plus the `jjf show --include-memories`
//! flag. v2.2 spec §10.
//!
//! Mirrors the hermetic-scratch style of `init.rs` and the other test
//! files in this crate.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;
use common::{scratch, run_jjf, JJF_BIN};

/// Make a directory that's an initialized jj+jjf repo (i.e. the
/// `issues` bookmark exists).
fn make_initialized(name: &str) -> PathBuf {
    let dir = scratch(name);
    let out = Command::new("git")
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("spawn git init");
    assert!(out.status.success(), "git init failed: {}", String::from_utf8_lossy(&out.stderr));
    // Set repo-local identity so verbs that commit have an author even in
    // bare CI where no global ~/.gitconfig exists.
    let out = Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(&dir)
        .output()
        .expect("spawn git config user.name");
    assert!(
        out.status.success(),
        "git config user.name failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("git")
        .args(["config", "user.email", "test@jjforge.invalid"])
        .current_dir(&dir)
        .output()
        .expect("spawn git config user.email");
    assert!(
        out.status.success(),
        "git config user.email failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let init = Command::new(JJF_BIN)
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("spawn jjf init");
    assert!(
        init.status.success(),
        "jjf init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    dir
}

fn run_jjf_stdin(cwd: &Path, args: &[&str], stdin: &str) -> Output {
    use std::io::Write;
    use std::process::Stdio;
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
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().expect("wait jjf")
}

#[test]
fn remember_then_recall_round_trips() {
    let repo = make_initialized("memory_round_trip");
    let out = run_jjf(&repo, &["remember", "always run tests with -race flag"]);
    assert!(
        out.status.success(),
        "remember failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Remembered ["),
        "expected `Remembered [...]` line, got: {stdout}"
    );

    // Auto slugified key.
    let recall = run_jjf(
        &repo,
        &["recall", "always-run-tests-with-race-flag"],
    );
    assert!(
        recall.status.success(),
        "recall failed: {}",
        String::from_utf8_lossy(&recall.stderr)
    );
    let recalled = String::from_utf8_lossy(&recall.stdout);
    assert_eq!(recalled.trim(), "always run tests with -race flag");
}

#[test]
fn remember_with_explicit_key_uses_it() {
    let repo = make_initialized("memory_explicit_key");
    let out = run_jjf(
        &repo,
        &[
            "remember",
            "Dolt phantom DBs hide in three places",
            "--key",
            "dolt-phantoms",
        ],
    );
    assert!(out.status.success(), "remember --key failed: {}", String::from_utf8_lossy(&out.stderr));

    let recall = run_jjf(&repo, &["recall", "dolt-phantoms"]);
    assert!(recall.status.success());
    assert_eq!(
        String::from_utf8_lossy(&recall.stdout).trim(),
        "Dolt phantom DBs hide in three places"
    );
}

#[test]
fn remember_upserts_existing_key() {
    let repo = make_initialized("memory_upsert");
    run_jjf(&repo, &["remember", "first value", "--key", "kkk"]);
    let out = run_jjf(&repo, &["remember", "second value", "--key", "kkk"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Updated [kkk]"),
        "expected `Updated [k]` for upsert, got: {stdout}"
    );

    let recall = run_jjf(&repo, &["recall", "kkk"]);
    assert_eq!(
        String::from_utf8_lossy(&recall.stdout).trim(),
        "second value"
    );
}

#[test]
fn forget_removes_memory() {
    let repo = make_initialized("memory_forget");
    run_jjf(&repo, &["remember", "value", "--key", "doomed"]);
    let out = run_jjf(&repo, &["forget", "doomed"]);
    assert!(out.status.success(), "forget failed: {}", String::from_utf8_lossy(&out.stderr));

    let recall = run_jjf(&repo, &["recall", "doomed"]);
    assert!(
        !recall.status.success(),
        "recall after forget should fail; stdout={} stderr={}",
        String::from_utf8_lossy(&recall.stdout),
        String::from_utf8_lossy(&recall.stderr),
    );
    assert_eq!(recall.status.code(), Some(1));
}

#[test]
fn forget_on_unknown_key_exits_1_with_memory_not_found() {
    let repo = make_initialized("memory_forget_missing");
    let out = run_jjf(&repo, &["forget", "no-such-key"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no memory with key"),
        "expected `no memory with key`, got: {stderr}"
    );
}

#[test]
fn memories_lists_all_keys_alphabetically() {
    let repo = make_initialized("memory_list");
    run_jjf(&repo, &["remember", "zz", "--key", "zebra"]);
    run_jjf(&repo, &["remember", "aa", "--key", "alpha"]);
    run_jjf(&repo, &["remember", "mm", "--key", "middle"]);

    let out = run_jjf(&repo, &["memories"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let alpha_pos = stdout.find("alpha").unwrap();
    let middle_pos = stdout.find("middle").unwrap();
    let zebra_pos = stdout.find("zebra").unwrap();
    assert!(alpha_pos < middle_pos);
    assert!(middle_pos < zebra_pos);
}

#[test]
fn memories_filters_by_substring_case_insensitive() {
    let repo = make_initialized("memory_filter");
    run_jjf(
        &repo,
        &["remember", "uses JWT not sessions", "--key", "auth-jwt"],
    );
    run_jjf(
        &repo,
        &["remember", "use race flag for tests", "--key", "test-race"],
    );

    // Filter on key substring.
    let out = run_jjf(&repo, &["memories", "AUTH"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("auth-jwt"));
    assert!(!stdout.contains("test-race"), "filter should exclude test-race, got: {stdout}");

    // Filter on value substring.
    let out2 = run_jjf(&repo, &["memories", "RACE"]);
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    assert!(stdout2.contains("test-race"));
    assert!(!stdout2.contains("auth-jwt"), "value-filter should exclude auth-jwt, got: {stdout2}");
}

#[test]
fn memories_json_returns_bare_array() {
    let repo = make_initialized("memory_json");
    run_jjf(&repo, &["remember", "v1", "--key", "key-1"]);
    run_jjf(&repo, &["remember", "v2", "--key", "key-2"]);

    let out = run_jjf(&repo, &["memories", "--json"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert!(v.is_array(), "expected array, got: {stdout}");
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    let keys: Vec<&str> = arr
        .iter()
        .map(|m| m["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["key-1", "key-2"]);
}

#[test]
fn recall_json_emits_envelope_with_found_field() {
    let repo = make_initialized("memory_recall_json");
    run_jjf(&repo, &["remember", "v", "--key", "kkk"]);

    let out = run_jjf(&repo, &["recall", "--json", "kkk"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(v["key"].as_str(), Some("kkk"));
    assert_eq!(v["value"].as_str(), Some("v"));
    assert_eq!(v["found"].as_bool(), Some(true));
}

#[test]
fn recall_unknown_key_exits_1() {
    let repo = make_initialized("memory_recall_unknown");
    let out = run_jjf(&repo, &["recall", "no-such-thing"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn remember_with_no_value_or_file_exits_2() {
    let repo = make_initialized("memory_remember_no_value");
    let out = run_jjf(&repo, &["remember"]);
    // Clap may catch this OR our preflight; either way exit code !=0.
    assert!(!out.status.success());
}

#[test]
fn remember_from_stdin_with_explicit_key() {
    let repo = make_initialized("memory_stdin");
    let out = run_jjf_stdin(
        &repo,
        &["remember", "--key", "from-stdin", "-F", "-"],
        "value from stdin",
    );
    assert!(
        out.status.success(),
        "remember -F - failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let recall = run_jjf(&repo, &["recall", "from-stdin"]);
    assert!(recall.status.success());
    assert_eq!(
        String::from_utf8_lossy(&recall.stdout).trim(),
        "value from stdin"
    );
}

#[test]
fn show_include_memories_appends_memory_block() {
    let repo = make_initialized("memory_show_include");
    // Create an issue to show.
    let new_out = run_jjf(&repo, &["--json", "new", "-t", "the issue"]);
    assert!(
        new_out.status.success(),
        "new failed: {}",
        String::from_utf8_lossy(&new_out.stderr)
    );
    let new_v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&new_out.stdout).trim())
            .expect("valid json");
    let id = new_v["id"].as_str().unwrap().to_owned();

    // Add two memories so the block has structure to assert on.
    run_jjf(&repo, &["remember", "value 1", "--key", "alpha"]);
    run_jjf(&repo, &["remember", "value 2", "--key", "beta"]);

    let out = run_jjf(&repo, &["show", &id, "--include-memories"]);
    assert!(
        out.status.success(),
        "show --include-memories failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("## Persistent Memories (2)"),
        "missing header, got: {stdout}"
    );
    // Sections in alphabetical order.
    let alpha_pos = stdout.find("### alpha").expect("missing ### alpha");
    let beta_pos = stdout.find("### beta").expect("missing ### beta");
    assert!(alpha_pos < beta_pos);
    assert!(stdout.contains("value 1"));
    assert!(stdout.contains("value 2"));
}

#[test]
fn show_without_include_memories_does_not_append_block() {
    let repo = make_initialized("memory_show_default");
    let new_out = run_jjf(&repo, &["--json", "new", "-t", "the issue"]);
    let new_v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&new_out.stdout).trim())
            .expect("valid json");
    let id = new_v["id"].as_str().unwrap().to_owned();
    run_jjf(&repo, &["remember", "value", "--key", "kkk"]);

    let out = run_jjf(&repo, &["show", &id]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("Persistent Memories"),
        "default show should NOT include memories, got: {stdout}"
    );
}

#[test]
fn show_include_memories_empty_does_not_append_block() {
    let repo = make_initialized("memory_show_empty");
    let new_out = run_jjf(&repo, &["--json", "new", "-t", "the issue"]);
    let new_v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&new_out.stdout).trim())
            .expect("valid json");
    let id = new_v["id"].as_str().unwrap().to_owned();

    let out = run_jjf(&repo, &["show", &id, "--include-memories"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("Persistent Memories"),
        "empty memory list should suppress the block, got: {stdout}"
    );
}

#[test]
fn remember_json_envelope_shape() {
    let repo = make_initialized("memory_remember_json");
    let out = run_jjf(&repo, &["--json", "remember", "v", "--key", "kkk"]);
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
            .expect("valid json");
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
    assert_eq!(v["key"], serde_json::Value::String("kkk".into()));
    assert_eq!(v["action"], serde_json::Value::String("remembered".into()));

    // Second call → action: updated.
    let upsert = run_jjf(&repo, &["--json", "remember", "v2", "--key", "kkk"]);
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&upsert.stdout).trim())
            .expect("valid json");
    assert_eq!(v["action"], serde_json::Value::String("updated".into()));
}

#[test]
fn forget_json_envelope_shape() {
    let repo = make_initialized("memory_forget_json");
    run_jjf(&repo, &["remember", "v", "--key", "kkk"]);
    let out = run_jjf(&repo, &["--json", "forget", "kkk"]);
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
            .expect("valid json");
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
    assert_eq!(v["key"], serde_json::Value::String("kkk".into()));
    assert_eq!(v["action"], serde_json::Value::String("forgot".into()));
}

#[test]
fn forget_missing_key_json_error_envelope() {
    let repo = make_initialized("memory_forget_missing_json");
    let out = run_jjf(&repo, &["--json", "forget", "no-key"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value = serde_json::from_str(stderr.trim()).expect("valid json");
    assert_eq!(v["ok"], serde_json::Value::Bool(false));
    assert_eq!(v["error"]["kind"].as_str(), Some("memory_not_found"));
    assert_eq!(v["error"]["details"]["key"].as_str(), Some("no-key"));
}

#[test]
fn remember_value_with_no_alphanumerics_errors_with_hint() {
    let repo = make_initialized("memory_no_slug");
    let out = run_jjf(&repo, &["remember", "!!! ..."]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--key"),
        "expected hint to pass --key, got: {stderr}"
    );
}
