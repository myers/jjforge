//! Integration tests for the v2.1 `type` + `slug` surface added in
//! issue `7100b51`. Drives the compiled `jjf` binary against per-test
//! scratch repos and asserts:
//!
//! - `jjf new --type` / `--slug` round-trip via `Storage::read`.
//! - `jjf update --type` / `--slug` / `--unset-slug` mutate the
//!   record.
//! - `jjf ls --type` / `--slug` filter correctly.
//! - Every id-taking verb (`show`, `update`, `close`, `comment`,
//!   `label add`) accepts a slug in place of the id.
//! - The new error envelopes (`invalid_slug`, `slug_collision`,
//!   `slug_not_found`) carry the documented `details` shape.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use jjf_storage::{IssueType, Status, Storage};

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
    assert!(out.status.success());
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
        "jjf init failed: {}",
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
        .unwrap()
        .write_all(stdin_bytes)
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn new_with_type_and_slug_round_trips_via_storage() {
    let repo = make_initialized_repo("type_and_slug_new");
    let out = run_jjf(
        &repo,
        &[
            "new",
            "-t",
            "ticket title",
            "--type",
            "feature",
            "--slug",
            "agent-ready",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id_str = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let id = jjf_storage::IssueId::parse(&id_str).unwrap();

    let storage = Storage::open(&repo).unwrap();
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.type_, IssueType::Feature);
    assert_eq!(issue.slug.as_deref(), Some("agent-ready"));
}

#[test]
fn new_invalid_slug_surfaces_json_error_envelope() {
    let repo = make_initialized_repo("type_and_slug_invalid");
    let out = run_jjf(
        &repo,
        &["--json", "new", "-t", "bad", "--slug", "Bad_Slug"],
    );
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    // JSON envelope per docs/cli-json.md.
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("envelope must be json");
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["kind"], "invalid_slug");
    assert_eq!(v["error"]["details"]["slug"], "Bad_Slug");
    assert_eq!(v["error"]["details"]["reason"], "bad_charset");
}

#[test]
fn new_slug_collision_among_open_issues_is_exit_two() {
    let repo = make_initialized_repo("type_and_slug_collision");
    let first_out = run_jjf(
        &repo,
        &["new", "-t", "first", "--slug", "shared-slug"],
    );
    assert!(first_out.status.success());
    let first_id = String::from_utf8_lossy(&first_out.stdout).trim().to_owned();

    let second_out = run_jjf(
        &repo,
        &["--json", "new", "-t", "second", "--slug", "shared-slug"],
    );
    assert_eq!(second_out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&second_out.stderr);
    let v: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
    assert_eq!(v["error"]["kind"], "slug_collision");
    assert_eq!(v["error"]["details"]["slug"], "shared-slug");
    assert_eq!(v["error"]["details"]["conflicts_with"], first_id);
}

#[test]
fn update_type_and_slug_lands_one_commit() {
    let repo = make_initialized_repo("type_and_slug_update");
    let new = run_jjf(&repo, &["new", "-t", "baseline"]);
    let id = String::from_utf8_lossy(&new.stdout).trim().to_owned();
    let out = run_jjf(
        &repo,
        &[
            "--json",
            "update",
            &id,
            "--type",
            "bug",
            "--slug",
            "baseline-slug",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    let fields = v["fields"].as_array().unwrap();
    assert!(fields.iter().any(|f| f == "slug"));
    assert!(fields.iter().any(|f| f == "type"));

    let storage = Storage::open(&repo).unwrap();
    let issue = storage
        .read(&jjf_storage::IssueId::parse(&id).unwrap())
        .unwrap();
    assert_eq!(issue.type_, IssueType::Bug);
    assert_eq!(issue.slug.as_deref(), Some("baseline-slug"));
}

#[test]
fn update_unset_slug_clears_field() {
    let repo = make_initialized_repo("type_and_slug_unset");
    let new = run_jjf(
        &repo,
        &["new", "-t", "has slug", "--slug", "will-go-away"],
    );
    let id = String::from_utf8_lossy(&new.stdout).trim().to_owned();

    let out = run_jjf(&repo, &["update", &id, "--unset-slug"]);
    assert!(out.status.success());

    let storage = Storage::open(&repo).unwrap();
    let issue = storage
        .read(&jjf_storage::IssueId::parse(&id).unwrap())
        .unwrap();
    assert_eq!(issue.slug, None);
}

#[test]
fn slug_and_unset_slug_are_mutually_exclusive() {
    // clap's `conflicts_with` enforces this — exit-2 from arg parsing
    // (clap's own error path; doesn't go through our JSON envelope).
    let repo = make_initialized_repo("type_and_slug_mutex");
    let new = run_jjf(&repo, &["new", "-t", "anything"]);
    let id = String::from_utf8_lossy(&new.stdout).trim().to_owned();

    let out = run_jjf(
        &repo,
        &["update", &id, "--slug", "new-slug", "--unset-slug"],
    );
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn id_taking_verbs_accept_slug_show() {
    let repo = make_initialized_repo("slug_handle_show");
    let new = run_jjf(
        &repo,
        &["new", "-t", "handle-me", "--slug", "handle-me-slug"],
    );
    let id = String::from_utf8_lossy(&new.stdout).trim().to_owned();

    let by_slug = run_jjf(&repo, &["show", "handle-me-slug"]);
    assert!(
        by_slug.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&by_slug.stderr)
    );
    let stdout = String::from_utf8_lossy(&by_slug.stdout);
    assert!(stdout.contains(&id), "show by slug must mention id: {stdout}");
    assert!(stdout.contains("slug: handle-me-slug"));
}

#[test]
fn id_taking_verbs_accept_slug_close_and_update_and_comment() {
    let repo = make_initialized_repo("slug_handle_other_verbs");
    let new = run_jjf(
        &repo,
        &["new", "-t", "lifecycle", "--slug", "lifecycle-slug"],
    );
    let id = String::from_utf8_lossy(&new.stdout).trim().to_owned();

    // close <slug>
    let close = run_jjf(&repo, &["close", "lifecycle-slug"]);
    assert!(close.status.success());

    // open <slug>
    let open = run_jjf(&repo, &["open", "lifecycle-slug"]);
    assert!(open.status.success());

    // update <slug> --title
    let upd = run_jjf(
        &repo,
        &["update", "lifecycle-slug", "--title", "renamed"],
    );
    assert!(upd.status.success());

    // comment <slug> -F path
    let body_path = repo.join("body.txt");
    fs::write(&body_path, "via slug").unwrap();
    let cmt = run_jjf(
        &repo,
        &[
            "comment",
            "lifecycle-slug",
            "-F",
            body_path.to_str().unwrap(),
            "--author",
            "tester <t@example.com>",
        ],
    );
    assert!(
        cmt.status.success(),
        "comment failed: {}",
        String::from_utf8_lossy(&cmt.stderr)
    );

    // label add <slug> needs-review
    let lbl = run_jjf(&repo, &["label", "add", "lifecycle-slug", "needs-review"]);
    assert!(lbl.status.success());

    let storage = Storage::open(&repo).unwrap();
    let issue = storage
        .read(&jjf_storage::IssueId::parse(&id).unwrap())
        .unwrap();
    assert_eq!(issue.title, "renamed");
    assert_eq!(issue.status, Status::Open);
    assert!(issue.labels.contains(&"needs-review".to_owned()));
    assert_eq!(issue.comments.len(), 1);
    assert_eq!(issue.comments[0].body, "via slug");
}

#[test]
fn unknown_handle_surfaces_slug_not_found() {
    let repo = make_initialized_repo("slug_handle_not_found");
    let out = run_jjf(&repo, &["--json", "show", "no-such-handle"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
    assert_eq!(v["error"]["kind"], "slug_not_found");
    assert_eq!(v["error"]["details"]["handle"], "no-such-handle");
}

#[test]
fn ls_type_filter_or_semantics_and_slug_substring() {
    let repo = make_initialized_repo("ls_filters");
    run_jjf(&repo, &["new", "-t", "alpha", "--type", "bug", "--slug", "alpha-slug"]);
    run_jjf(&repo, &["new", "-t", "beta", "--type", "feature", "--slug", "beta-slug"]);
    run_jjf(&repo, &["new", "-t", "gamma"]);

    // --type bug returns alpha only.
    let bugs = run_jjf(&repo, &["--json", "ls", "--type", "bug"]);
    assert!(bugs.status.success());
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&bugs.stdout).trim()).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["title"], "alpha");

    // --type bug --type feature OR-matches both.
    let either = run_jjf(
        &repo,
        &["--json", "ls", "--type", "bug", "--type", "feature"],
    );
    assert!(either.status.success());
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&either.stdout).trim()).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);

    // --slug alpha substring-matches alpha-slug.
    let slug_filt = run_jjf(&repo, &["--json", "ls", "--slug", "alpha"]);
    assert!(slug_filt.status.success());
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&slug_filt.stdout).trim()).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["title"], "alpha");
}

#[test]
fn show_json_includes_type_and_slug_fields() {
    let repo = make_initialized_repo("show_json_with_type");
    let out = run_jjf(
        &repo,
        &[
            "new",
            "-t",
            "showme",
            "--type",
            "research",
            "--slug",
            "show-me",
        ],
    );
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();

    let show = run_jjf(&repo, &["--json", "show", &id]);
    assert!(show.status.success());
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&show.stdout).trim()).unwrap();
    assert_eq!(v["type"], "research");
    assert_eq!(v["slug"], "show-me");
}

#[test]
fn comment_stdin_via_slug_for_completeness() {
    // Quick sanity: stdin path still works when the handle is a slug.
    // Distinct from the file-path test above because the storage call
    // path differs (we already exercise the slug -> id resolve there).
    let repo = make_initialized_repo("comment_stdin_via_slug");
    run_jjf(&repo, &["new", "-t", "slot", "--slug", "comment-via-slug"]);
    let out = run_jjf_with_stdin(
        &repo,
        &[
            "comment",
            "comment-via-slug",
            "-F",
            "-",
            "--author",
            "t <t@x>",
        ],
        b"stdin-body\n",
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
