//! Integration tests for `jjf search` — drive the compiled binary
//! against per-test scratch repos and assert exit code, stdout
//! (plain + `--json`), the filter matrix, and the envelope shape:
//!
//! - happy path: title hit, body hit, comments-off vs comments-on,
//! - `--json` envelope is `{ok:true, results:[...]}`,
//! - status/label/type filters compose AND with the substring match,
//! - `--limit` truncates after the score sort,
//! - empty query → silent (plain) or empty envelope (`--json`).
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as `ls.rs`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const JJF_BIN: &str = env!("CARGO_BIN_EXE_jjf");

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
    Command::new(JJF_BIN).args(args).current_dir(cwd).output().expect("spawn jjf")
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

/// Create an issue, return its id.
fn create_issue(repo: &Path, title: &str, body: &[u8], extra_args: &[&str]) -> String {
    let mut args: Vec<&str> = vec!["new", "-t", title, "-F", "-"];
    args.extend_from_slice(extra_args);
    let out = run_jjf_with_stdin(repo, &args, body);
    assert!(
        out.status.success(),
        "jjf new failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Append a comment to an issue via the real `jjf comment` verb.
fn add_comment(repo: &Path, id: &str, body: &[u8]) {
    let out = run_jjf_with_stdin(
        repo,
        &["comment", id, "-F", "-", "--author", "alice <a@x>"],
        body,
    );
    assert!(
        out.status.success(),
        "jjf comment failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Parse `jjf search` plain-text rows into `(id, title, field, snippet)`.
fn parse_search_rows(stdout: &str) -> Vec<(String, String, String, String)> {
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let parts: Vec<&str> = l.splitn(4, '\t').collect();
            assert_eq!(
                parts.len(),
                4,
                "expected 4 tab-separated columns, got {}: {l:?}",
                parts.len()
            );
            (
                parts[0].to_owned(),
                parts[1].to_owned(),
                parts[2].to_owned(),
                parts[3].to_owned(),
            )
        })
        .collect()
}

/// Parse the `id` field from JSON-formatted stdout (e.g., from `jjf new --json`).
fn parse_id_from_stdout(stdout: &[u8]) -> String {
    let json_str = String::from_utf8_lossy(stdout);
    let json_obj: serde_json::Value = serde_json::from_str(&json_str)
        .unwrap_or_else(|e| panic!("stdout is not valid JSON: {e}\nstdout: {json_str}"));
    json_obj["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no 'id' field in JSON: {json_obj}"))
        .to_owned()
}

// --- tests ---------------------------------------------------------

#[test]
fn search_title_hit_emits_one_row() {
    let repo = make_initialized_repo("search_title_hit");
    let id = create_issue(&repo, "panic on segfault", b"some body", &[]);
    let _other = create_issue(&repo, "unrelated", b"unrelated", &[]);

    let out = run_jjf(&repo, &["search", "segfault"]);
    assert!(
        out.status.success(),
        "search failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows = parse_search_rows(&stdout);
    assert_eq!(rows.len(), 1, "stdout: {stdout:?}");
    assert_eq!(rows[0].0, id);
    assert_eq!(rows[0].1, "panic on segfault");
    assert_eq!(rows[0].2, "title");
    assert!(rows[0].3.contains("segfault"));
}

#[test]
fn search_json_envelope_shape() {
    let repo = make_initialized_repo("search_json_shape");
    let id = create_issue(&repo, "panic on segfault", b"body mentions widget", &[]);

    let out = run_jjf(&repo, &["search", "widget", "--json"]);
    assert!(
        out.status.success(),
        "search --json failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("search --json must be valid JSON");
    assert_eq!(v["ok"], serde_json::json!(true));
    let results = v["results"].as_array().expect("results is array");
    assert_eq!(results.len(), 1);
    let hit = &results[0];
    assert_eq!(hit["id"], serde_json::json!(id));
    assert_eq!(hit["title"], serde_json::json!("panic on segfault"));
    assert_eq!(hit["matched_field"], serde_json::json!("body"));
    assert_eq!(hit["score"], serde_json::json!(1));
    assert!(hit["snippet"].as_str().unwrap().contains("widget"));
}

#[test]
fn search_empty_query_plain_is_silent_json_is_empty_envelope() {
    let repo = make_initialized_repo("search_empty_query");
    let _id = create_issue(&repo, "anything", b"body content", &[]);

    // Plain text: silent on empty result (matches `ls`).
    let out = run_jjf(&repo, &["search", ""]);
    assert!(out.status.success(), "empty query is exit 0");
    assert!(out.stdout.is_empty(), "plain text must be silent");

    // JSON: empty envelope, NOT silence (matches the ticket contract).
    let out = run_jjf(&repo, &["search", "", "--json"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(
        v["results"].as_array().expect("results array").len(),
        0,
        "empty query → empty results array"
    );
}

#[test]
fn search_label_and_status_filters_compose_with_query() {
    let repo = make_initialized_repo("search_filter_compose");
    let want = create_issue(&repo, "first widget bug", b"", &["-l", "backend"]);
    let no_label = create_issue(&repo, "second widget bug", b"", &[]);
    let closed = create_issue(&repo, "third widget bug", b"", &["-l", "backend"]);
    // Close one so `--status open` excludes it.
    let _ = run_jjf(&repo, &["close", &closed]);

    // No filter: all three hit.
    let out = run_jjf(&repo, &["search", "widget"]);
    assert!(out.status.success());
    let rows = parse_search_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 3, "default status=all returns all matches");

    // --status open eliminates the closed one.
    let out = run_jjf(&repo, &["search", "widget", "--status", "open"]);
    assert!(out.status.success());
    let rows = parse_search_rows(&String::from_utf8_lossy(&out.stdout));
    let ids: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert!(ids.contains(&want.as_str()));
    assert!(ids.contains(&no_label.as_str()));
    assert!(!ids.contains(&closed.as_str()), "closed must be excluded");

    // --label backend eliminates the unlabeled one.
    let out = run_jjf(
        &repo,
        &["search", "widget", "--status", "open", "--label", "backend"],
    );
    assert!(out.status.success());
    let rows = parse_search_rows(&String::from_utf8_lossy(&out.stdout));
    let ids: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(rows.len(), 1, "label=backend AND status=open: {ids:?}");
    assert_eq!(ids[0], want);
}

#[test]
fn search_include_comments_widens_result_set() {
    let repo = make_initialized_repo("search_include_comments");
    let title_hit = create_issue(&repo, "needle in title", b"unrelated body", &[]);
    let body_hit = create_issue(&repo, "unrelated", b"body mentions needle", &[]);
    let comment_host = create_issue(&repo, "totally unrelated", b"unrelated body", &[]);
    add_comment(&repo, &comment_host, b"comment mentions needle here");

    // Without the flag, only title and body hits surface.
    let out = run_jjf(&repo, &["search", "needle"]);
    assert!(out.status.success());
    let rows = parse_search_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 2);
    let ids: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert!(ids.contains(&title_hit.as_str()));
    assert!(ids.contains(&body_hit.as_str()));
    assert!(!ids.contains(&comment_host.as_str()));

    // With the flag, the comment-only host hits too.
    let out = run_jjf(&repo, &["search", "needle", "--include-comments"]);
    assert!(out.status.success());
    let rows = parse_search_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 3);
    let ids: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert!(ids.contains(&comment_host.as_str()));
}

#[test]
fn search_limit_truncates_after_sort() {
    let repo = make_initialized_repo("search_limit_truncates");
    let _a = create_issue(&repo, "alpha widget bug", b"", &[]);
    let _b = create_issue(&repo, "beta widget bug", b"", &[]);
    let _c = create_issue(&repo, "gamma widget bug", b"", &[]);

    let out = run_jjf(&repo, &["search", "widget", "--limit", "2"]);
    assert!(out.status.success());
    let rows = parse_search_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 2, "limit truncates: {rows:?}");
}

#[test]
fn search_no_match_is_silent_exit_zero() {
    let repo = make_initialized_repo("search_no_match");
    let _id = create_issue(&repo, "alpha", b"body", &[]);

    let out = run_jjf(&repo, &["search", "nonexistent"]);
    assert!(out.status.success(), "no match is exit 0");
    assert!(out.stdout.is_empty(), "no match → silent plain text");

    let out = run_jjf(&repo, &["search", "nonexistent", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(v["results"].as_array().unwrap().len(), 0);
}

#[test]
fn search_outside_jj_repo_is_preflight_failure() {
    // No `jjf init`, no jj repo at all.
    let dir = scratch("search_no_repo");
    let out = run_jjf(&dir, &["search", "anything"]);
    assert!(!out.status.success(), "non-jj cwd must fail preflight");
    assert_eq!(out.status.code(), Some(2), "exit 2 = preflight");
}

#[test]
fn search_parent_flag_intersects_with_query() {
    let repo = make_initialized_repo("search_parent");
    let epic_id = parse_id_from_stdout(
        &run_jjf(&repo, &["new", "--json", "-t", "Epic", "--type", "epic", "--slug", "epic"]).stdout,
    );
    // Two children, both with "needle" in the title — only one is parented.
    let parented_id = parse_id_from_stdout(
        &run_jjf(&repo, &["new", "--json", "-t", "needle child", "--parent", &epic_id]).stdout,
    );
    let _orphan = run_jjf(&repo, &["new", "--json", "-t", "needle orphan"]);

    let out = run_jjf(&repo, &["search", "--json", "--parent", "epic", "needle"]);
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = envelope["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["id"].as_str().unwrap(), parented_id.as_str());
}
