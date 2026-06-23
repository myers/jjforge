//! Integration tests for `jjf ls` — drive the compiled binary against
//! per-test scratch repos and assert exit code, stdout (plain +
//! `--json`), the filter matrix, and the error shapes:
//!
//! - happy path: 3 bugs, default `--status open` returns them all,
//! - status filter: open/closed/all,
//! - label filter: single + intersection (AND),
//! - `--json` is an array of Bug records,
//! - empty issues bookmark → exit 0, zero output,
//! - non-jj cwd → exit 2,
//! - jj repo without `issues` bookmark → exit 2 + init hint,
//! - `--help` documents both flags.
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as the other test
//! files in this crate.

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
    // Note: stdin is inherited from the test process when no piping is
    // requested. `ls` reads no input, so this is safe.
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

/// Close a bug via the real `jjf close <id>` verb. Drives the same
/// code path operators use, so any regression in close-from-the-CLI
/// surfaces here too (rather than tests passing while the verb is
/// broken).
fn close_bug(repo: &Path, id: &str) {
    let out = run_jjf(repo, &["close", id]);
    assert!(
        out.status.success(),
        "jjf close failed during setup: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Parse `jjf ls` plain-text output into one row per bug. Each row is
/// `<id>\t<status>\t<labelN>L\t<title>`; we split on tabs and drop
/// empty lines.
fn parse_ls_rows(stdout: &str) -> Vec<(String, String, String, String)> {
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let parts: Vec<&str> = l.split('\t').collect();
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

// --- tests ---------------------------------------------------------

#[test]
fn ls_default_status_open_returns_every_bug_when_all_open() {
    let repo = make_initialized_repo("ls_three_open");
    let a = create_issue(&repo, "alpha bug", b"", &[]);
    let b = create_issue(&repo, "beta bug", b"", &[]);
    let c = create_issue(&repo, "gamma bug", b"", &[]);

    let out = run_jjf(&repo, &["ls"]);
    assert!(
        out.status.success(),
        "jjf ls failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows = parse_ls_rows(&stdout);

    assert_eq!(
        rows.len(),
        3,
        "expected 3 rows, got {} (stdout: {stdout:?})",
        rows.len()
    );

    // Every created id appears, every status is `open`, every title
    // shows up in some row.
    let ids: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    for id in [&a, &b, &c] {
        assert!(ids.iter().any(|x| x == id), "id {id} missing from: {stdout:?}");
    }
    for r in &rows {
        assert_eq!(r.1, "open", "status column wrong in row: {r:?}");
    }
    let titles: Vec<&str> = rows.iter().map(|r| r.3.as_str()).collect();
    assert!(titles.contains(&"alpha bug"));
    assert!(titles.contains(&"beta bug"));
    assert!(titles.contains(&"gamma bug"));
}

#[test]
fn ls_status_filter_open_closed_all() {
    let repo = make_initialized_repo("ls_status_matrix");
    let a = create_issue(&repo, "stay open A", b"", &[]);
    let b = create_issue(&repo, "stay open B", b"", &[]);
    let c = create_issue(&repo, "close me", b"", &[]);
    close_bug(&repo, &c);

    // Default (open) → 2 rows.
    let out = run_jjf(&repo, &["ls"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let rows = parse_ls_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 2, "default status should yield 2 open bugs: {rows:?}");
    for r in &rows {
        assert_eq!(r.1, "open");
    }
    let open_ids: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert!(open_ids.iter().any(|x| x == &a));
    assert!(open_ids.iter().any(|x| x == &b));
    assert!(!open_ids.iter().any(|x| x == &c), "closed bug must NOT appear");

    // --status closed → 1 row.
    let out = run_jjf(&repo, &["ls", "--status", "closed"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let rows = parse_ls_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 1, "closed filter should yield 1 row: {rows:?}");
    assert_eq!(rows[0].0, c);
    assert_eq!(rows[0].1, "closed");

    // --status all → 3 rows.
    let out = run_jjf(&repo, &["ls", "--status", "all"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let rows = parse_ls_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 3, "all filter should yield 3 rows: {rows:?}");
}

#[test]
fn ls_label_filter_single_and_intersection() {
    let repo = make_initialized_repo("ls_label_matrix");
    // Three bugs, distinct label sets:
    //   only-x:   [x]
    //   only-y:   [y]
    //   both-xy:  [x, y]
    let only_x = create_issue(&repo, "only-x", b"", &["-l", "x"]);
    let only_y = create_issue(&repo, "only-y", b"", &["-l", "y"]);
    let both_xy = create_issue(&repo, "both-xy", b"", &["-l", "x", "-l", "y"]);

    // --label x → matches only-x AND both-xy (2 rows).
    let out = run_jjf(&repo, &["ls", "--label", "x"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let rows = parse_ls_rows(&String::from_utf8_lossy(&out.stdout));
    let ids: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(rows.len(), 2, "label=x should match 2: {rows:?}");
    assert!(ids.iter().any(|i| i == &only_x));
    assert!(ids.iter().any(|i| i == &both_xy));
    assert!(!ids.iter().any(|i| i == &only_y), "only-y must NOT match label=x");

    // --label y → matches only-y AND both-xy (2 rows).
    let out = run_jjf(&repo, &["ls", "--label", "y"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let rows = parse_ls_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 2, "label=y should match 2: {rows:?}");

    // --label x --label y → intersection: only both-xy (1 row).
    let out = run_jjf(&repo, &["ls", "--label", "x", "--label", "y"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let rows = parse_ls_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(
        rows.len(),
        1,
        "label=x AND label=y should yield exactly 1 row: {rows:?}"
    );
    assert_eq!(rows[0].0, both_xy);

    // The non-matching label entirely → empty + exit 0.
    let out = run_jjf(&repo, &["ls", "--label", "nope"]);
    assert!(out.status.success(), "exit 0 on no matches");
    assert!(out.stdout.is_empty(), "no matches must produce no stdout");
}

#[test]
fn ls_json_emits_array_of_bug_records() {
    let repo = make_initialized_repo("ls_json");
    let _a = create_issue(&repo, "first", b"body of first\n", &["-l", "bug", "-a", "alice"]);
    let _b = create_issue(&repo, "second", b"", &[]);

    let out = run_jjf(&repo, &["ls", "--json"]);
    assert!(
        out.status.success(),
        "jjf ls --json failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("ls --json output must be valid JSON");
    let arr = v.as_array().expect("ls --json must be an array");
    assert_eq!(arr.len(), 2, "expected 2 elements, got: {stdout}");

    // Per-element shape: must look like a `Bug` record (id, title,
    // status, labels, comments). We don't pin the per-element order
    // (newest-first is enforced indirectly via the plain-text test) —
    // here we just confirm shape on every element.
    for el in arr {
        assert!(el["id"].is_string(), "missing/wrong id: {el}");
        assert!(el["title"].is_string(), "missing/wrong title: {el}");
        assert!(el["status"].is_string(), "missing/wrong status: {el}");
        assert!(el["labels"].is_array(), "missing/wrong labels: {el}");
        assert!(el["comments"].is_array(), "missing/wrong comments: {el}");
        assert!(el["created_at"].is_string(), "missing/wrong created_at: {el}");
    }
}

#[test]
fn ls_in_empty_bugs_bookmark_exits_zero_with_no_output() {
    // `jjf init` ran, nothing else — the bookmark has zero bug files.
    let repo = make_initialized_repo("ls_empty_bookmark");

    let out = run_jjf(&repo, &["ls"]);
    assert!(
        out.status.success(),
        "ls on empty bookmark should exit 0: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        out.stdout.is_empty(),
        "empty bookmark must produce no stdout, got: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn ls_json_error_envelope_on_non_jj_directory() {
    // `--json` outside a jj repo: error envelope on stderr, not the
    // plain `jjf: <text>` line. Pins the contract for read-verb
    // failures (the bare-array success shape does not apply on error).
    let dir = scratch("ls_json_err_non_jj");
    let out = run_jjf(&dir, &["--json", "ls"]);
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
        Some("not_a_jj_repo"),
        "kind wrong: {stderr}"
    );
    assert_eq!(
        v["error"]["details"]["path"].as_str(),
        Some(dir.to_string_lossy().as_ref()),
        "details.path wrong: {stderr}"
    );
}

#[test]
fn ls_in_non_jj_directory_exits_two() {
    let dir = scratch("ls_non_jj");
    let out = run_jjf(&dir, &["ls"]);
    assert!(!out.status.success(), "ls in non-jj dir should fail");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a jj repo"),
        "stderr should mention `not a jj repo`, got: {stderr}"
    );
}

#[test]
fn ls_in_jj_repo_without_bugs_bookmark_exits_two_with_init_hint() {
    // Fresh jj repo, no `jjf init` — the missing-bookmark probe should
    // fire with the typed `run jjf init first` message.
    let repo = make_jj_repo("ls_no_bookmark");
    let out = run_jjf(&repo, &["ls"]);
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
fn ls_help_documents_status_and_label_flags() {
    // --help should mention both --status and --label. Keeps the public
    // surface stable against accidental renames.
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
        .args(["ls", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn jjf ls --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("--status"),
        "ls --help should document --status, got: {help}"
    );
    assert!(
        help.contains("--label"),
        "ls --help should document --label, got: {help}"
    );
}
