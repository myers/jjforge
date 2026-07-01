//! Integration tests for `iss ready` — drive the compiled binary
//! against per-test scratch repos and assert exit code, stdout
//! (plain + `--json`), the dep-blocking filter, the type-priority
//! sort, label intersection, `--limit`, the empty-bookmark case,
//! and the preflight failure shapes.
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as the other
//! test files in this crate.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

mod common;
use common::*;

/// Create an issue via `iss new`, return its id. `extra_args` lets
/// the caller pin `--type`, `-d`, `-l`, etc. We always pass `-F -`
/// with an empty body to keep the call non-interactive.
fn create_issue(repo: &Path, title: &str, extra_args: &[&str]) -> String {
    let mut args: Vec<&str> = vec!["new", "-t", title, "-F", "-"];
    args.extend_from_slice(extra_args);
    let out = run_jjf_with_stdin(repo, &args, b"");
    assert!(
        out.status.success(),
        "jjf new failed during setup: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Close an issue via `iss close <id>`.
fn close_issue(repo: &Path, id: &str) {
    let out = run_jjf(repo, &["close", id]);
    assert!(
        out.status.success(),
        "jjf close failed during setup: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Parse the tab-separated `ready` plain-text output into rows.
/// Shape mirrors `ls`:
/// `<id>\t<status>\t<priority>\t<type>\t<title>` (326bbf7, v2.8).
/// The tuple shape is `(id, status, priority, type, title)`.
fn parse_ready_rows(stdout: &str) -> Vec<(String, String, String, String, String)> {
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
fn ready_dep_chain_returns_only_unblocked_issues() {
    // A is open. B depends on A. C is independent. Expected: A and
    // C, B excluded. This is the headline acceptance test.
    let repo = make_initialized_repo("ready_dep_chain");
    let a = create_issue(&repo, "A", &["--type", "feature"]);
    let _b = create_issue(&repo, "B", &["--type", "feature", "-d", &a]);
    let c = create_issue(&repo, "C", &["--type", "feature"]);

    let out = run_jjf(&repo, &["ready"]);
    assert!(
        out.status.success(),
        "jjf ready failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows = parse_ready_rows(&stdout);
    let ids: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        rows.len(),
        2,
        "expected 2 unblocked, got {}: {stdout:?}",
        rows.len()
    );
    assert!(ids.contains(&a.as_str()), "A unblocked, missing: {stdout:?}");
    assert!(ids.contains(&c.as_str()), "C unblocked, missing: {stdout:?}");
    for r in &rows {
        assert_eq!(r.1, "open", "row status must be open: {r:?}");
    }
}

#[test]
fn ready_json_limit_one_returns_single_element_array() {
    let repo = make_initialized_repo("ready_json_limit");
    let _a = create_issue(&repo, "A", &["--type", "feature"]);
    let _b = create_issue(&repo, "B", &["--type", "feature"]);
    let _c = create_issue(&repo, "C", &["--type", "feature"]);

    let out = run_jjf(&repo, &["ready", "--json", "--limit", "1"]);
    assert!(
        out.status.success(),
        "jjf ready --json --limit 1 failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("ready --json must be valid JSON");
    let arr = v.as_array().expect("ready --json must be an array");
    assert_eq!(
        arr.len(),
        1,
        "--limit 1 must produce exactly one element, got: {stdout}"
    );
    // Per-element shape — `Issue` projection (id, title, status,
    // labels, comments, type, slug).
    let el = &arr[0];
    assert!(el["id"].is_string(), "missing/wrong id: {el}");
    assert!(el["title"].is_string(), "missing/wrong title: {el}");
    assert_eq!(el["status"], "open", "ready issues must be open");
    assert!(el["labels"].is_array(), "missing labels: {el}");
    assert!(el["comments"].is_array(), "missing comments: {el}");
    assert!(el["type"].is_string(), "missing type: {el}");
}

#[test]
fn ready_label_filter_intersects_with_unblocked_set() {
    let repo = make_initialized_repo("ready_label_filter");
    let only_backend = create_issue(
        &repo,
        "only-backend",
        &["--type", "feature", "-l", "backend"],
    );
    let _frontend = create_issue(
        &repo,
        "only-frontend",
        &["--type", "feature", "-l", "frontend"],
    );
    let _both = create_issue(
        &repo,
        "both labels",
        &["--type", "feature", "-l", "backend", "-l", "frontend"],
    );

    let out = run_jjf(&repo, &["ready", "--label", "backend"]);
    assert!(
        out.status.success(),
        "jjf ready --label backend failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows = parse_ready_rows(&stdout);
    // Two issues carry `backend`: only-backend and both-labels.
    assert_eq!(rows.len(), 2, "expected 2 backend issues, got {rows:?}");
    let ids: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert!(ids.contains(&only_backend.as_str()));
}

#[test]
fn ready_nothing_unblocked_exits_zero_with_empty_output() {
    // Closed issues are not ready (`status != open`); open issues
    // with open deps are blocked. Build a state where neither
    // applies and the ready set is empty.
    let repo = make_initialized_repo("ready_empty_unblocked");
    let a = create_issue(&repo, "A", &["--type", "feature"]);
    let _b = create_issue(&repo, "B", &["--type", "feature", "-d", &a]);
    close_issue(&repo, &a);
    // A is now closed → excluded from ready. B's only dep is
    // closed → B is ready. Test the OTHER empty path: also close B.
    let b_id = {
        // Re-query: list_ids order is stable but we want the second
        // create's id. Re-grab via ls --json --status open.
        let out = run_jjf(&repo, &["ls", "--json", "--status", "open"]);
        assert!(out.status.success());
        let v: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1, "exactly B is open: {arr:#?}");
        arr[0]["id"].as_str().unwrap().to_owned()
    };
    close_issue(&repo, &b_id);

    let out = run_jjf(&repo, &["ready"]);
    assert!(
        out.status.success(),
        "ready on nothing-unblocked should exit 0: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        out.stdout.is_empty(),
        "nothing unblocked → no stdout, got: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );

    // The --json shape on empty is `[]`, not silence.
    let out = run_jjf(&repo, &["ready", "--json"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(v.is_array());
    assert_eq!(v.as_array().unwrap().len(), 0, "expected empty array");
}

#[test]
fn ready_type_priority_sort_puts_bug_first() {
    // Cross-checks the CLI plumbing: filing in epic→bug→feature
    // order, `ready` should still surface bug first.
    let repo = make_initialized_repo("ready_type_sort_cli");
    let _epic = create_issue(&repo, "epic", &["--type", "epic"]);
    let bug = create_issue(&repo, "bug", &["--type", "bug"]);
    let _feature = create_issue(&repo, "feature", &["--type", "feature"]);

    let out = run_jjf(&repo, &["ready"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows = parse_ready_rows(&stdout);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].0, bug, "bug must sort first: {rows:?}");
    // 326bbf7 (v2.8): fourth column is the type wire spelling (the
    // third column is now the priority bucket).
    assert_eq!(rows[0].3, "bug", "type column should be 'bug': {rows:?}");
    let types: Vec<&str> = rows.iter().map(|r| r.3.as_str()).collect();
    assert!(
        types.contains(&"epic") && types.contains(&"feature"),
        "type column should carry the wire spelling for every row: {rows:?}",
    );
}

#[test]
fn ready_in_non_jj_directory_exits_two() {
    let dir = scratch_non_git("ready_non_jj");
    let out = run_jjf(&dir, &["ready"]);
    assert!(!out.status.success(), "ready in non-jj dir should fail");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a jj repo"),
        "stderr should mention `not a jj repo`, got: {stderr}"
    );
}

#[test]
fn ready_in_jj_repo_without_issues_bookmark_exits_two_with_init_hint() {
    let repo = make_jj_repo("ready_no_bookmark");
    let out = run_jjf(&repo, &["ready"]);
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
        stderr.contains("`issues` bookmark") && stderr.contains("iss init"),
        "stderr should tell the user to run `iss init` first, got: {stderr}"
    );
}

#[test]
fn ready_json_error_envelope_on_non_jj_directory() {
    // `--json` outside a jj repo: error envelope on stderr.
    let dir = scratch_non_git("ready_json_err_non_jj");
    let out = run_jjf(&dir, &["--json", "ready"]);
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
}

#[test]
fn ready_help_documents_label_limit_and_type_flags() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(ISS_BIN)
        .args(["ready", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn jjf ready --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("--label"),
        "ready --help should document --label, got: {help}"
    );
    assert!(
        help.contains("--limit"),
        "ready --help should document --limit, got: {help}"
    );
    assert!(
        help.contains("--type"),
        "ready --help should document --type, got: {help}"
    );
    assert!(
        help.contains("--json"),
        "ready --help should mention --json (global), got: {help}"
    );
}

/// `iss ready` exhibits the same silent-drop-fix behavior as `jjf
/// ls`: a corrupt `refs/jjf/issues/<id>` ref drops out of the
/// candidate set but stderr carries a `jjf: warning:` line naming
/// the ref. Ticket `4928ae6`.
#[test]
fn ready_warns_on_corrupt_issue_ref() {
    let repo = make_initialized_repo("ready_warn_corrupt_issue");
    create_issue(&repo, "alive ticket", &[]);
    let corrupt = create_issue(&repo, "corrupt ticket", &[]);

    // Hash a junk blob and repoint the corrupt ticket's ref at it.
    let mut child = Command::new("git")
        .args(["hash-object", "-w", "--stdin"])
        .current_dir(&repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git hash-object");
    child
        .stdin
        .as_mut()
        .expect("stdin handle")
        .write_all(b"junk\n")
        .expect("write stdin");
    let blob_out = child.wait_with_output().expect("wait git");
    assert!(
        blob_out.status.success(),
        "git hash-object failed: {}",
        String::from_utf8_lossy(&blob_out.stderr)
    );
    let blob_oid = String::from_utf8_lossy(&blob_out.stdout).trim().to_owned();

    let refname = format!("refs/jjf/issues/{}", corrupt);
    let upd = Command::new("git")
        .args(["update-ref", &refname, &blob_oid])
        .current_dir(&repo)
        .output()
        .expect("spawn git update-ref");
    assert!(
        upd.status.success(),
        "git update-ref failed: {}",
        String::from_utf8_lossy(&upd.stderr)
    );

    let out = run_jjf(&repo, &["ready"]);
    assert!(
        out.status.success(),
        "ready must still exit 0: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("iss: warning:"),
        "ready stderr must carry the `jjf: warning:` header, got: {stderr:?}"
    );
    assert!(
        stderr.contains(&refname),
        "ready stderr must name the corrupt ref ({refname}), got: {stderr:?}"
    );
    assert!(
        stderr.contains("skipped from listing"),
        "ready stderr must explain consequence, got: {stderr:?}"
    );

    // Stdout: alive ticket remains visible, corrupt one absent.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("alive ticket"),
        "alive ticket should appear in ready output, got: {stdout:?}"
    );
    assert!(
        !stdout.contains("corrupt ticket"),
        "corrupt ticket title must NOT appear, got: {stdout:?}"
    );
}

#[test]
fn ready_parent_flag_filters_to_parent_child_children() {
    let repo = make_initialized_repo("ready_parent_basic");

    let epic_id = parse_id_from_stdout(
        &run_jjf_with_stdin(&repo, &["new", "--json", "-t", "Epic", "--type", "epic", "--slug", "demo-epic", "-F", "-"], b"")
            .stdout,
    );
    let child_id = parse_id_from_stdout(
        &run_jjf_with_stdin(&repo, &["new", "--json", "-t", "child", "--parent", epic_id.as_str(), "-F", "-"], b"").stdout,
    );
    let _sibling_id = parse_id_from_stdout(
        &run_jjf_with_stdin(&repo, &["new", "--json", "-t", "sibling", "-F", "-"], b"").stdout,
    );

    // Bare `ready` returns all three.
    let bare = run_jjf(&repo, &["ready", "--json"]);
    let bare_arr: Vec<serde_json::Value> = serde_json::from_slice(&bare.stdout).unwrap();
    assert_eq!(bare_arr.len(), 3);

    // `--parent <epic-id>` returns only the child.
    let filtered = run_jjf(&repo, &["ready", "--json", "--parent", epic_id.as_str()]);
    let filtered_arr: Vec<serde_json::Value> = serde_json::from_slice(&filtered.stdout).unwrap();
    assert_eq!(filtered_arr.len(), 1);
    assert_eq!(filtered_arr[0]["id"].as_str().unwrap(), child_id.as_str());

    // `--parent demo-epic` (by slug) works identically.
    let by_slug = run_jjf(&repo, &["ready", "--json", "--parent", "demo-epic"]);
    let by_slug_arr: Vec<serde_json::Value> = serde_json::from_slice(&by_slug.stdout).unwrap();
    assert_eq!(by_slug_arr.len(), 1);
    assert_eq!(by_slug_arr[0]["id"].as_str().unwrap(), child_id.as_str());
}

#[test]
fn ready_parent_composes_with_type_and_limit() {
    let repo = make_initialized_repo("ready_parent_compose");
    let epic_id = parse_id_from_stdout(
        &run_jjf_with_stdin(&repo, &["new", "--json", "-t", "E", "--type", "epic", "--slug", "epic", "-F", "-"], b"").stdout,
    );
    // Two children: one bug, one feature.
    let _bug = run_jjf_with_stdin(
        &repo,
        &["new", "--json", "-t", "bug-child", "--type", "bug", "--parent", epic_id.as_str(), "-F", "-"],
        b""
    );
    let _feat = run_jjf_with_stdin(
        &repo,
        &["new", "--json", "-t", "feat-child", "--type", "feature", "--parent", epic_id.as_str(), "-F", "-"],
        b""
    );

    // `--parent epic --type bug` returns just the bug.
    let out = run_jjf(&repo, &["ready", "--json", "--parent", "epic", "--type", "bug"]);
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["title"].as_str().unwrap(), "bug-child");

    // `--parent epic --limit 1` returns the top-priority one (bug, per type-priority sort).
    let out = run_jjf(&repo, &["ready", "--json", "--parent", "epic", "--limit", "1"]);
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["title"].as_str().unwrap(), "bug-child");
}

#[test]
fn ready_parent_unknown_handle_exits_two() {
    let repo = make_initialized_repo("ready_parent_unknown");
    let out = run_jjf(&repo, &["ready", "--parent", "no-such-slug"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("slug_not_found") || stderr.contains("no-such-slug"),
        "stderr should mention the bad handle: {stderr}"
    );
}

fn ready_parent_bad_hex_exits_one_issue_not_found() {
    // A well-formed 7-char hex id that doesn't match any issue must
    // surface as `issue_not_found` (exit 1), the same shape as
    // `iss show <bad-hex>`. Today it silently matches nothing (exit 0).
    let repo = make_initialized_repo("ready_parent_bad_hex");
    let out = run_jjf(&repo, &["--json", "ready", "--parent", "deadbee"]);
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
fn ready_filters_by_meta() {
    let repo = make_initialized_repo("ready_meta_filter");
    // Issue A gets a metadata tag; issue B does not.
    let id_a = create_issue(&repo, "issue-a", &[]);
    let id_b = create_issue(&repo, "issue-b", &[]);

    // Tag issue A with metadata.
    let set_out = run_jjf(
        &repo,
        &["metadata", "set", &id_a, "gc.routed_to", "worker-1"],
    );
    assert!(
        set_out.status.success(),
        "metadata set failed: stderr={}",
        String::from_utf8_lossy(&set_out.stderr)
    );

    // `ready --meta gc.routed_to=worker-1 --json` should include A, exclude B.
    let out = run_jjf(
        &repo,
        &["ready", "--meta", "gc.routed_to=worker-1", "--json"],
    );
    assert!(
        out.status.success(),
        "ready --meta should exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not valid JSON: {e}\nstdout: {stdout}"));
    let ids: Vec<&str> = arr
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&id_a.as_str()), "should include id_a; got: {stdout}");
    assert!(!ids.contains(&id_b.as_str()), "should NOT include id_b; got: {stdout}");
}
