//! Integration tests for `jjf dep add|rm|tree` (v2.4 `agent-dep-types`).
//!
//! Drives the compiled binary against per-test scratch repos and
//! asserts:
//!
//! - `jjf dep add A B` with default `--kind blocks` writes a blocks
//!   edge; `jjf show A` reports it under the new typed
//!   `dependencies:` section.
//! - All four `--kind` values round-trip (`blocks`, `parent-child`,
//!   `related`, `discovered-from`).
//! - `jjf dep rm` removes only the named kind; other-kind edges to
//!   the same target stay.
//! - `jjf dep tree` prints the parent-child tree under a given root.
//! - `jjf new --dep <kind>:<id>` accepts the inline kind syntax.
//! - `jjf ready` excludes children of in-flight parents via the
//!   parent-child cascade.
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as the other
//! test files.

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

// ---- tests ---------------------------------------------------------

#[test]
fn dep_add_default_kind_blocks_show_reports_edge() {
    let repo = make_initialized_repo("dep_add_default");
    let parent = create_issue(&repo, "parent");
    let child = create_issue(&repo, "child");

    let out = run_jjf(&repo, &["dep", "add", &child, &parent]);
    assert!(
        out.status.success(),
        "jjf dep add failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        format!("dep added: blocks {child} -> {parent}")
    );

    // `show` reports the edge under the new typed dependencies section.
    let out = run_jjf(&repo, &["show", &child]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("dependencies:"),
        "show should report typed dependencies section, got: {stdout}"
    );
    assert!(
        stdout.contains(&format!("blocks: {parent}")),
        "show should list `blocks: {parent}`, got: {stdout}"
    );
}

#[test]
fn dep_add_all_four_kinds_round_trip() {
    let repo = make_initialized_repo("dep_all_kinds");
    let parent = create_issue(&repo, "parent");
    let child = create_issue(&repo, "child");

    for kind in &["blocks", "parent-child", "related", "discovered-from"] {
        let out = run_jjf(&repo, &["dep", "add", &child, &parent, "--kind", kind]);
        assert!(
            out.status.success(),
            "jjf dep add --kind {kind} failed: stderr={}",
            String::from_utf8_lossy(&out.stderr),
        );
    }

    // `show` lists all four kinds.
    let out = run_jjf(&repo, &["show", &child]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    for kind in &["blocks", "parent-child", "related", "discovered-from"] {
        assert!(
            stdout.contains(&format!("{kind}: {parent}")),
            "show should list `{kind}: {parent}`, got: {stdout}"
        );
    }
}

#[test]
fn dep_rm_removes_only_named_kind() {
    let repo = make_initialized_repo("dep_rm_kind");
    let parent = create_issue(&repo, "parent");
    let child = create_issue(&repo, "child");

    // Add two kinds pointing at the same target.
    let out = run_jjf(&repo, &["dep", "add", &child, &parent, "--kind", "blocks"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = run_jjf(
        &repo,
        &["dep", "add", &child, &parent, "--kind", "parent-child"],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    // Remove only the `blocks` edge.
    let out = run_jjf(&repo, &["dep", "rm", &child, &parent, "--kind", "blocks"]);
    assert!(
        out.status.success(),
        "jjf dep rm failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `show` no longer lists `blocks:` but still lists `parent-child:`.
    let out = run_jjf(&repo, &["show", &child]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains(&format!("blocks: {parent}")),
        "blocks edge should be gone, got: {stdout}"
    );
    assert!(
        stdout.contains(&format!("parent-child: {parent}")),
        "parent-child edge should remain, got: {stdout}"
    );
}

#[test]
fn dep_tree_prints_parent_child_hierarchy() {
    let repo = make_initialized_repo("dep_tree");
    let a = create_issue(&repo, "epic A");
    let b = create_issue(&repo, "child B");
    let c = create_issue(&repo, "grandchild C");

    // B is child of A; C is child of B.
    let out = run_jjf(&repo, &["dep", "add", &b, &a, "--kind", "parent-child"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = run_jjf(&repo, &["dep", "add", &c, &b, "--kind", "parent-child"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["dep", "tree", &a]);
    assert!(
        out.status.success(),
        "jjf dep tree failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Every id appears, parent before child.
    let a_pos = stdout.find(&a).expect("A in tree");
    let b_pos = stdout.find(&b).expect("B in tree");
    let c_pos = stdout.find(&c).expect("C in tree");
    assert!(a_pos < b_pos, "A should appear before B, got: {stdout}");
    assert!(b_pos < c_pos, "B should appear before C, got: {stdout}");
}

#[test]
fn dep_tree_json_envelope_carries_nested_structure() {
    let repo = make_initialized_repo("dep_tree_json");
    let a = create_issue(&repo, "epic A");
    let b = create_issue(&repo, "child B");

    let out = run_jjf(&repo, &["dep", "add", &b, &a, "--kind", "parent-child"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["--json", "dep", "tree", &a]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(v["root"]["id"], a.as_str());
    let children = v["root"]["children"].as_array().expect("children array");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0]["id"], b.as_str());
}

#[test]
fn new_with_inline_dep_kind_syntax() {
    // `jjf new --dep parent-child:<id>` produces a parent-child edge.
    let repo = make_initialized_repo("new_inline_kind");
    let parent = create_issue(&repo, "parent");
    let spec = format!("parent-child:{parent}");

    let out = run_jjf_with_stdin(
        &repo,
        &["new", "-t", "child of parent", "-d", &spec, "-F", "-"],
        b"",
    );
    assert!(
        out.status.success(),
        "jjf new with --dep parent-child:<id> failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let child = String::from_utf8_lossy(&out.stdout).trim().to_owned();

    let out = run_jjf(&repo, &["show", &child]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!("parent-child: {parent}")),
        "show should list parent-child edge, got: {stdout}"
    );
    assert!(
        !stdout.contains(&format!("blocks: {parent}")),
        "show should NOT list a blocks edge, got: {stdout}"
    );
}

#[test]
fn new_with_unknown_dep_kind_is_preflight_failure() {
    // `jjf new --dep bogus:<id>` exits 2 with `bad_dep_kind`.
    let repo = make_initialized_repo("new_bad_dep_kind");
    let parent = create_issue(&repo, "parent");
    let spec = format!("bogus:{parent}");

    let out = run_jjf_with_stdin(
        &repo,
        &["--json", "new", "-t", "bad kind", "-d", &spec, "-F", "-"],
        b"",
    );
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value = serde_json::from_str(stderr.trim()).expect("json error");
    assert_eq!(v["error"]["kind"], "bad_dep_kind");
}

// ---- qa-dep-validation (d1a01f0) ------------------------------------
// Both rejection paths land at the CLI envelope: phantom dep target
// surfaces as `issue_not_found` (the existing kind, reused per the
// ticket); self-dep surfaces as `self_dependency` (new).
// ---------------------------------------------------------------------

#[test]
fn dep_add_phantom_target_exits_with_issue_not_found() {
    let repo = make_initialized_repo("dep_add_phantom_target");
    let child = create_issue(&repo, "child");
    // `deadbee` is a well-formed 7-hex id that has never existed.
    let out = run_jjf(&repo, &["--json", "dep", "add", &child, "deadbee"]);
    // `issue_not_found` is exit 1 (runtime) per the established
    // convention: a well-formed id that just doesn't exist isn't a
    // preflight failure. The kind is the load-bearing signal scripts
    // match on; the code distinguishes preflight from runtime.
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1, got code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("json error envelope");
    assert_eq!(v["error"]["kind"], "issue_not_found");
    assert_eq!(v["error"]["details"]["id"], "deadbee");

    // Verify no dep edge landed on the child.
    let show = run_jjf(&repo, &["show", &child]);
    let show_out = String::from_utf8_lossy(&show.stdout);
    assert!(
        !show_out.contains("blocks: deadbee"),
        "rejected dep_add must not land an edge, got: {show_out}"
    );
}

#[test]
fn dep_add_self_target_exits_2_with_self_dependency() {
    let repo = make_initialized_repo("dep_add_self_target");
    let child = create_issue(&repo, "self-targeted");
    let out = run_jjf(&repo, &["--json", "dep", "add", &child, &child]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit 2, got code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("json error envelope");
    assert_eq!(v["error"]["kind"], "self_dependency");
    assert_eq!(v["error"]["details"]["id"], child.as_str());

    let show = run_jjf(&repo, &["show", &child]);
    let show_out = String::from_utf8_lossy(&show.stdout);
    assert!(
        !show_out.contains(&format!("blocks: {child}")),
        "rejected self-dep must not land an edge, got: {show_out}"
    );
}

#[test]
fn dep_add_self_target_rejected_for_all_kinds() {
    let repo = make_initialized_repo("dep_add_self_all_kinds");
    let child = create_issue(&repo, "all-kinds-self");
    for kind in &["blocks", "parent-child", "related", "discovered-from"] {
        let out = run_jjf(
            &repo,
            &["--json", "dep", "add", &child, &child, "--kind", kind],
        );
        assert_eq!(
            out.status.code(),
            Some(2),
            "kind={kind}: expected exit 2, got code={:?} stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr),
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        let v: serde_json::Value =
            serde_json::from_str(stderr.trim()).expect("json error envelope");
        assert_eq!(v["error"]["kind"], "self_dependency", "kind={kind}");
    }
}

#[test]
fn new_with_phantom_dep_target_exits_with_issue_not_found() {
    // `jjf new -d <phantom>` — bare 7-hex form, default `blocks`
    // semantics — must reject at create time.
    let repo = make_initialized_repo("new_phantom_dep_bare");
    let out = run_jjf_with_stdin(
        &repo,
        &["--json", "new", "-t", "depends on ghost", "-d", "deadbee", "-F", "-"],
        b"",
    );
    // `issue_not_found` is exit 1 (runtime) per the established
    // convention: a well-formed id that just doesn't exist isn't a
    // preflight failure. The kind is the load-bearing signal scripts
    // match on; the code distinguishes preflight from runtime.
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1, got code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("json error envelope");
    assert_eq!(v["error"]["kind"], "issue_not_found");
    assert_eq!(v["error"]["details"]["id"], "deadbee");

    // `jjf ls` must show no issues.
    let ls = run_jjf(&repo, &["--json", "ls", "--status", "all"]);
    let ls_out = String::from_utf8_lossy(&ls.stdout);
    let v: serde_json::Value = serde_json::from_str(ls_out.trim()).expect("ls json");
    let items = v.as_array().expect("ls array");
    assert!(
        items.is_empty(),
        "rejected create must not land an issue, got: {ls_out}"
    );
}

#[test]
fn new_with_inline_phantom_dep_kind_exits_with_issue_not_found() {
    // `jjf new --dep blocks:<phantom>` — explicit-kind form. The
    // dep kind parses fine; the target doesn't exist; reject.
    let repo = make_initialized_repo("new_phantom_dep_kind");
    let out = run_jjf_with_stdin(
        &repo,
        &[
            "--json",
            "new",
            "-t",
            "depends on ghost",
            "-d",
            "blocks:deadbee",
            "-F",
            "-",
        ],
        b"",
    );
    // `issue_not_found` is exit 1 (runtime) per the established
    // convention: a well-formed id that just doesn't exist isn't a
    // preflight failure. The kind is the load-bearing signal scripts
    // match on; the code distinguishes preflight from runtime.
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1, got code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("json error envelope");
    assert_eq!(v["error"]["kind"], "issue_not_found");
    assert_eq!(v["error"]["details"]["id"], "deadbee");
}

#[test]
fn dep_rm_against_phantom_target_succeeds_as_noop() {
    // `jjf dep rm A <phantom>` is permissive — removing a
    // non-existent edge is a useful cleanup primitive. Don't
    // adopt the strict validation here.
    let repo = make_initialized_repo("dep_rm_phantom_noop");
    let child = create_issue(&repo, "real child");
    let out = run_jjf(&repo, &["dep", "rm", &child, "deadbee"]);
    assert!(
        out.status.success(),
        "dep rm against phantom should succeed, got code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn ready_excludes_child_of_blocked_parent_via_cascade() {
    // Setup: blocker (open) → parent (open, blocked by blocker) →
    // child (open, child of parent). `jjf ready` should exclude
    // both parent (blocked by blocker) and child (cascaded).
    let repo = make_initialized_repo("ready_cascade");
    let blocker = create_issue(&repo, "blocker");
    let parent = create_issue(&repo, "parent");
    let child = create_issue(&repo, "child");

    // parent blocks-edge to blocker
    let out = run_jjf(
        &repo,
        &["dep", "add", &parent, &blocker, "--kind", "blocks"],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // child parent-child to parent
    let out = run_jjf(
        &repo,
        &["dep", "add", &child, &parent, "--kind", "parent-child"],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["--json", "ready"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("ready json");
    let issues = v.as_array().expect("ready returns array");
    let ids: Vec<&str> = issues
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    // Only blocker is ready; parent (blocked) and child (cascaded)
    // are excluded.
    assert!(ids.contains(&blocker.as_str()), "blocker should be ready, got: {ids:?}");
    assert!(!ids.contains(&parent.as_str()), "parent should NOT be ready, got: {ids:?}");
    assert!(!ids.contains(&child.as_str()), "child should NOT be ready (cascade), got: {ids:?}");
}
