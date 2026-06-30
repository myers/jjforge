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

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

mod common;
use common::*;

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
/// `<id>\t<status>\t<priority>\t<type>\t<title>` (326bbf7, v2.8 row
/// rework); we split on tabs and drop empty lines. The tuple shape
/// is `(id, status, priority, type, title)`.
fn parse_ls_rows(stdout: &str) -> Vec<(String, String, String, String, String)> {
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let parts: Vec<&str> = l.split('\t').collect();
            assert_eq!(
                parts.len(),
                5,
                "expected 5 tab-separated columns, got {}: {l:?}",
                parts.len()
            );
            (
                parts[0].to_owned(),
                parts[1].to_owned(),
                parts[2].to_owned(),
                parts[3].to_owned(),
                parts[4].to_owned(),
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
        // 326bbf7 (v2.8): third column is the priority bucket. These
        // fixtures didn't set -p, so the column carries `-`.
        assert_eq!(r.2, "-", "priority column wrong in row: {r:?}");
        // 326bbf7 (v2.8): fourth column is the type wire spelling.
        // These fixtures didn't set --type, so default is
        // `unspecified`.
        assert_eq!(r.3, "unspecified", "type column wrong in row: {r:?}");
    }
    let titles: Vec<&str> = rows.iter().map(|r| r.4.as_str()).collect();
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

/// Pipe `stdin_bytes` into a `git` invocation under `cwd` and return
/// its trimmed stdout. Used by the corrupt-ref tests to hash a junk
/// blob and then point an issue ref at the blob via plain
/// `git update-ref`. Mirrors the helper of the same name in
/// `crates/jjf-storage/tests/v3_read_path.rs` and `integration.rs`.
fn git_capture_with_stdin(args: &[&str], stdin_bytes: &[u8], cwd: &Path) -> String {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git");
    child
        .stdin
        .as_mut()
        .expect("stdin handle")
        .write_all(stdin_bytes)
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait git");
    assert!(
        out.status.success(),
        "`git {}` failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Repoint `refs/jjf/issues/<id>` at a junk blob. The simplest
/// reproduction of the silent-drop bug from ticket `4928ae6`.
fn corrupt_issue_ref(repo: &Path, id: &str) {
    let blob_oid = git_capture_with_stdin(
        &["hash-object", "-w", "--stdin"],
        b"junk content\n",
        repo,
    );
    let blob_oid = blob_oid.trim();
    let refname = format!("refs/jjf/issues/{}", id);
    let out = Command::new("git")
        .args(["update-ref", &refname, blob_oid])
        .current_dir(repo)
        .output()
        .expect("spawn git update-ref");
    assert!(
        out.status.success(),
        "git update-ref {} {} failed: stderr={}",
        refname,
        blob_oid,
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn ls_warns_on_corrupt_issue_ref_plain_text() {
    // Ticket `4928ae6`: pointing an issue ref at a non-commit object
    // used to silently drop the issue from `jjf ls` with no
    // diagnostic. The fix: stdout still shows the survivors, but
    // stderr names the casualty via a `jjf: warning:` line.
    let repo = make_initialized_repo("ls_warn_corrupt_issue");
    let alive = create_issue(&repo, "alive issue", b"", &[]);
    let corrupt = create_issue(&repo, "soon-to-be-corrupt", b"", &[]);

    corrupt_issue_ref(&repo, &corrupt);

    let out = run_jjf(&repo, &["ls"]);
    assert!(
        out.status.success(),
        "ls must still exit 0 even with corrupt refs: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows = parse_ls_rows(&stdout);
    let ids: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert!(ids.iter().any(|x| x == &alive), "alive issue must appear: {stdout:?}");
    assert!(
        !ids.iter().any(|x| x == &corrupt),
        "corrupt issue must NOT appear in stdout: {stdout:?}"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("jjf: warning:"),
        "stderr must carry the `jjf: warning:` header, got: {stderr:?}"
    );
    let expected_ref = format!("refs/jjf/issues/{}", corrupt);
    assert!(
        stderr.contains(&expected_ref),
        "stderr must name the corrupt ref ({expected_ref}), got: {stderr:?}"
    );
    assert!(
        stderr.contains("skipped from listing"),
        "stderr must explain the consequence (`skipped from listing`), got: {stderr:?}"
    );
}

#[test]
fn ls_json_preserves_bare_array_with_warning_on_stderr() {
    // Per ticket `4928ae6` design note: keep stdout's bare-array
    // shape stable so existing `--json` consumers don't break.
    // Warnings ride stderr as a one-line JSON envelope so the
    // operator and a wrapping orchestrator can both pick them up
    // without parsing the success stream.
    let repo = make_initialized_repo("ls_warn_corrupt_json");
    let alive = create_issue(&repo, "alive json", b"", &[]);
    let corrupt = create_issue(&repo, "corrupt json", b"", &[]);
    corrupt_issue_ref(&repo, &corrupt);

    let out = run_jjf(&repo, &["--json", "ls"]);
    assert!(
        out.status.success(),
        "ls --json must still exit 0: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    // STDOUT: bare array of Issue records (back-compat shape).
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("ls --json stdout must be valid JSON");
    let arr = v.as_array().expect("ls --json stdout must remain a bare array");
    assert_eq!(arr.len(), 1, "only the alive issue should appear: {stdout}");
    assert_eq!(arr[0]["id"].as_str(), Some(alive.as_str()));

    // STDERR: one JSON envelope per warning batch.
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stderr_lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        !stderr_lines.is_empty(),
        "expected at least one stderr line, got nothing"
    );
    // Find the warning envelope. Tolerate other (non-warning)
    // diagnostic lines that might land before it.
    let envelope_line = stderr_lines
        .iter()
        .find(|l| l.contains("\"warning\""))
        .unwrap_or_else(|| panic!("no warning envelope on stderr: {stderr:?}"));
    let env: serde_json::Value =
        serde_json::from_str(envelope_line).expect("warning envelope must be valid JSON");
    assert_eq!(env["warning"].as_str(), Some("unreadable_refs"));
    assert_eq!(env["count"].as_u64(), Some(1));
    let refs = env["refs"].as_array().expect("envelope.refs must be an array");
    let expected_ref = format!("refs/jjf/issues/{}", corrupt);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].as_str(), Some(expected_ref.as_str()));
}

#[test]
fn ls_no_warning_on_healthy_repo() {
    // Baseline: a clean repo MUST NOT emit the warning. Pin this so
    // a future refactor that fires the warning unconditionally is
    // caught immediately.
    let repo = make_initialized_repo("ls_no_warn_clean");
    create_issue(&repo, "first", b"", &[]);
    create_issue(&repo, "second", b"", &[]);

    let out = run_jjf(&repo, &["ls"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("warning"),
        "clean repo must not emit any warning, got stderr: {stderr:?}"
    );
}

/// Repoint `refs/jjf/meta/format-version` at a junk blob to simulate
/// the QA red-team `c1` attack (issue `de59159`). Mirrors the helper
/// pattern of `corrupt_issue_ref`, but for the sentinel ref.
fn corrupt_sentinel_to_blob(repo: &Path) -> String {
    let blob_oid = git_capture_with_stdin(
        &["hash-object", "-w", "--stdin"],
        b"not a commit\n",
        repo,
    );
    let blob_oid = blob_oid.trim().to_owned();
    let out = Command::new("git")
        .args(["update-ref", "refs/jjf/meta/format-version", &blob_oid])
        .current_dir(repo)
        .output()
        .expect("spawn git update-ref");
    assert!(
        out.status.success(),
        "git update-ref refs/jjf/meta/format-version {} failed: stderr={}",
        blob_oid,
        String::from_utf8_lossy(&out.stderr),
    );
    blob_oid
}

#[test]
fn ls_exits_1_with_corrupt_sentinel_envelope() {
    // Ticket `de59159` (QA sub-pass 3, attack `c1`): when someone
    // hand-wires the v3 format-version sentinel to a blob,
    // `jjf ls` used to silently exit 0 with normal output because
    // `detect_storage_mode` only checked presence of the ref. The
    // fix surfaces the typed `corrupt_sentinel` envelope.
    let repo = make_initialized_repo("ls_corrupt_sentinel");
    let blob_oid = corrupt_sentinel_to_blob(&repo);

    let out = run_jjf(&repo, &["--json", "ls"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "ls must exit 1 with a corrupt sentinel, got code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let envelope_line = stderr
        .lines()
        .find(|l| l.contains("\"ok\""))
        .unwrap_or_else(|| panic!("no JSON envelope on stderr: {stderr:?}"));
    let env: serde_json::Value =
        serde_json::from_str(envelope_line).expect("envelope must be valid JSON");
    assert_eq!(env["ok"].as_bool(), Some(false));
    assert_eq!(env["error"]["kind"].as_str(), Some("corrupt_sentinel"));
    assert_eq!(
        env["error"]["details"]["object_type"].as_str(),
        Some("blob")
    );
    assert_eq!(env["error"]["details"]["oid"].as_str(), Some(blob_oid.as_str()));
}

#[test]
fn ls_parent_flag_filters_to_parent_child_children() {
    let repo = make_initialized_repo("ls_parent_basic");
    let epic_id = parse_id_from_stdout(
        &run_jjf(&repo, &["new", "--json", "-t", "Epic", "--type", "epic", "--slug", "demo"]).stdout,
    );
    let child_id = parse_id_from_stdout(
        &run_jjf(&repo, &["new", "--json", "-t", "child", "--parent", epic_id.as_str()]).stdout,
    );
    let _sibling = run_jjf(&repo, &["new", "--json", "-t", "sibling"]);

    let out = run_jjf(&repo, &["ls", "--json", "--parent", "demo"]);
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"].as_str().unwrap(), child_id.as_str());
}

#[test]
fn ls_parent_unknown_handle_exits_two() {
    let repo = make_initialized_repo("ls_parent_unknown");
    let out = run_jjf(&repo, &["ls", "--parent", "no-such-slug"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn ls_parent_bad_hex_exits_one_issue_not_found() {
    // A well-formed 7-char hex id that doesn't match any issue must
    // surface as `issue_not_found` (exit 1), the same shape as
    // `jjf show <bad-hex>`. Today it silently matches nothing (exit 0).
    let repo = make_initialized_repo("ls_parent_bad_hex");
    let out = run_jjf(&repo, &["--json", "ls", "--parent", "deadbee"]);
    assert!(
        !out.status.success(),
        "expected failure, got success with stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("stderr must be valid JSON envelope");
    assert_eq!(v["ok"], serde_json::Value::Bool(false));
    assert_eq!(
        v["error"]["kind"].as_str(),
        Some("issue_not_found"),
        "kind wrong: {stderr}"
    );
}

#[test]
fn ls_parent_composes_with_type_and_status() {
    let repo = make_initialized_repo("ls_parent_compose");
    let epic_out = run_jjf(&repo, &["new", "--json", "-t", "E", "--type", "epic", "--slug", "epic-parent"]);
    assert!(
        epic_out.status.success(),
        "failed to create epic: {}",
        String::from_utf8_lossy(&epic_out.stderr)
    );
    let epic_id = parse_id_from_stdout(&epic_out.stdout);

    let bug_out = run_jjf(&repo, &["new", "--json", "-t", "bug", "--type", "bug", "--parent", epic_id.as_str()]);
    assert!(
        bug_out.status.success(),
        "failed to create bug: {}",
        String::from_utf8_lossy(&bug_out.stderr)
    );
    let bug_id = parse_id_from_stdout(&bug_out.stdout);

    let feat_out = run_jjf(&repo, &["new", "--json", "-t", "feat", "--type", "feature", "--parent", epic_id.as_str()]);
    assert!(
        feat_out.status.success(),
        "failed to create feature: {}",
        String::from_utf8_lossy(&feat_out.stderr)
    );

    // Close the bug; it should disappear from the default --status open listing.
    run_jjf(&repo, &["close", bug_id.as_str()]);

    let out = run_jjf(&repo, &["ls", "--json", "--parent", "epic-parent", "--type", "bug"]);
    assert!(
        out.status.success(),
        "ls failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(arr.len(), 0, "closed bug should be hidden by default --status open");

    let out = run_jjf(&repo, &["ls", "--json", "--parent", "epic-parent", "--type", "bug", "--status", "all"]);
    assert!(
        out.status.success(),
        "ls --status all failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(arr.len(), 1);
}

#[test]
fn ls_meta_bare_key_rejected_at_clap_parse_time() {
    // A bare `--meta foo` (no `=`) must fail at clap parse time with exit 2
    // and a message explaining the required format. Before the value_parser
    // was added, this was silently accepted and treated as key="" (bug).
    let repo = make_initialized_repo("ls_meta_bare_key_rejected");
    let out = run_jjf(&repo, &["ls", "--meta", "foo"]);
    assert!(
        !out.status.success(),
        "bare --meta key (no '=') should be rejected; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("expected key=value"),
        "stderr should explain the parser; got: {}",
        stderr
    );
}

#[test]
fn ls_help_documents_status_and_label_flags() {
    // --help should mention both --status and --label. Keeps the public
    // surface stable against accidental renames.
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
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
