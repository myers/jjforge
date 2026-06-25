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

// --- prefix-lookup-broken (ticket `4940d78`) ---
//
// Restored behavior: every id-taking verb accepts an unambiguous hex
// prefix (1–6 chars) in place of the full 7-char id. The CLI tests
// here pin the JSON error envelope shapes for `id_not_found` and
// `ambiguous_prefix`, and the happy-path resolves via `show`.

#[test]
fn show_resolves_unique_prefix_to_full_id() {
    let repo = make_initialized_repo("prefix_show_unique");
    let out = run_jjf(&repo, &["new", "-t", "prefix-me"]);
    assert!(out.status.success());
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let prefix = &id[..4];
    let show = run_jjf(&repo, &["--json", "show", prefix]);
    assert!(
        show.status.success(),
        "show <prefix> failed; stderr: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&show.stdout).trim()).unwrap();
    assert_eq!(v["id"], id);
}

#[test]
fn show_unknown_hex_prefix_surfaces_id_not_found_envelope() {
    let repo = make_initialized_repo("prefix_show_unknown_hex");
    // Plant one issue so the snapshot isn't empty; the prefix below
    // is guaranteed not to match it because the first hex char
    // differs (we pick whichever of 0000 or ffff doesn't match).
    let out = run_jjf(&repo, &["new", "-t", "anchor"]);
    assert!(out.status.success());
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let probe = if id.starts_with('0') { "ffff" } else { "0000" };
    let show = run_jjf(&repo, &["--json", "show", probe]);
    assert_eq!(show.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&show.stderr);
    let v: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["kind"], "id_not_found");
    assert_eq!(v["error"]["details"]["handle"], probe);
}

#[test]
fn show_ambiguous_prefix_surfaces_envelope_with_matches() {
    // Mint enough issues to virtually guarantee at least one 1-char
    // hex-prefix collision (16 buckets; with 32 issues the
    // birthday-paradox argument says collisions are overwhelming).
    // Then iterate the 16 single-hex-digit prefixes until we find one
    // with two+ matches and assert the envelope shape on that
    // ambiguous probe.
    let repo = make_initialized_repo("prefix_show_ambiguous");
    let mut ids = Vec::new();
    for i in 0..32 {
        let out = run_jjf(&repo, &["new", "-t", &format!("issue-{i}")]);
        assert!(out.status.success());
        ids.push(String::from_utf8_lossy(&out.stdout).trim().to_owned());
    }
    // Find a 1-char prefix that matches 2+ ids.
    let probe = (b'0'..=b'9')
        .chain(b'a'..=b'f')
        .map(|b| (b as char).to_string())
        .find(|p| ids.iter().filter(|i| i.starts_with(p)).count() >= 2)
        .expect("expected at least one ambiguous single-hex-char prefix in 32 ids");
    let expected_matches: Vec<String> = {
        let mut m: Vec<String> = ids
            .iter()
            .filter(|i| i.starts_with(&probe))
            .cloned()
            .collect();
        m.sort();
        m
    };

    let show = run_jjf(&repo, &["--json", "show", &probe]);
    assert_eq!(show.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&show.stderr);
    let v: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["kind"], "ambiguous_prefix");
    assert_eq!(v["error"]["details"]["handle"], probe);
    let got_matches: Vec<String> = v["error"]["details"]["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(got_matches, expected_matches);
}

#[test]
fn show_full_id_still_resolves_post_prefix_change() {
    // Regression guard: the 7-char fast path can't have been broken
    // by the prefix-resolve refactor.
    let repo = make_initialized_repo("prefix_show_full_id");
    let out = run_jjf(&repo, &["new", "-t", "full-id"]);
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let show = run_jjf(&repo, &["--json", "show", &id]);
    assert!(
        show.status.success(),
        "show <id> failed; stderr: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&show.stdout).trim()).unwrap();
    assert_eq!(v["id"], id);
}
