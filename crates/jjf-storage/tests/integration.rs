//! Integration test: drive the 4-CLI write-path dance against a real
//! throwaway `jj` repo and assert what landed in the working copy and
//! commit history.
//!
//! Mirrors the hermetic-scratch style of `experiments/`: a per-test
//! directory under `tests/.scratch/`, wiped on each run, gitignored.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use jjf_storage::{BugDraft, BugId, Status, Storage};

/// Build a scratch jj repo with a seeded `bugs` bookmark. Returns the
/// absolute path to the repo root.
fn make_scratch_repo(name: &str) -> PathBuf {
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(".scratch")
        .join(name);
    if scratch.exists() {
        fs::remove_dir_all(&scratch).unwrap();
    }
    fs::create_dir_all(&scratch).unwrap();
    let abs = fs::canonicalize(&scratch).unwrap();

    sh("jj", &["git", "init"], &abs);

    // Seed: an empty commit with description `jjf: seed bugs bookmark`
    // per spec §1.1, then point the bookmark at it, then step @ off it
    // so the storage crate's first `jj new bookmarks(bugs)` is a clean
    // branch from the seed (not from a working copy holding stale data).
    sh(
        "jj",
        &["new", "root()", "-m", "jjf: seed bugs bookmark"],
        &abs,
    );
    sh("jj", &["bookmark", "create", "bugs", "-r", "@"], &abs);
    sh("jj", &["new", "root()"], &abs);

    abs
}

fn sh(prog: &str, args: &[&str], cwd: &Path) {
    let out = Command::new(prog).args(args).current_dir(cwd).output().unwrap();
    assert!(
        out.status.success(),
        "`{prog} {}` failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Read a file's contents from the `bugs` bookmark tip.
fn read_at_bookmark(repo: &Path, relpath: &str) -> String {
    jj_capture(
        &[
            "file",
            "show",
            "-r",
            "bookmarks(bugs)",
            &format!("root:{}", relpath),
        ],
        repo,
    )
}

fn jj_capture(args: &[&str], cwd: &Path) -> String {
    let out = Command::new("jj").args(args).current_dir(cwd).output().unwrap();
    assert!(
        out.status.success(),
        "`jj {}` failed in {}:\nstderr: {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn create_then_set_status_lands_two_commits_on_bookmark() {
    let repo = make_scratch_repo("create_then_set_status");
    let storage = Storage::open(&repo).expect("Storage::open");

    let draft = BugDraft {
        title: "segfault on empty input".into(),
        body: "Running `./app` with no arguments crashes.".into(),
        labels: vec!["bug".into(), "p1".into()],
        dependencies: vec![],
        assignee: Some("alice".into()),
    };
    let id = storage.create_bug(&draft).expect("create_bug");
    let id_s = id.to_string();
    assert_eq!(id_s.len(), 7);

    // bugs/<id>.json exists at the bookmark tip with the schema fields.
    // (The dance's step 4 — `jj new root()` — moves @ off the bookmark,
    // so the file is not in the working copy. The authoritative copy
    // lives at the bookmark; read via `jj file show`.)
    let json_text = read_at_bookmark(&repo, &format!("bugs/{}.json", id_s));
    let v: serde_json::Value = serde_json::from_str(&json_text).unwrap();
    assert_eq!(v["version"], 1);
    assert_eq!(v["id"], id_s);
    assert_eq!(v["title"], "segfault on empty input");
    assert_eq!(v["status"], "open");
    assert_eq!(v["labels"], serde_json::json!(["bug", "p1"]));
    assert_eq!(v["dependencies"], serde_json::json!([]));
    assert_eq!(v["assignee"], "alice");
    assert!(json_text.ends_with('\n'), "record must end with newline (spec §3)");
    // Pretty-printed: 2-space indent, contains a newline after the open brace.
    assert!(
        json_text.starts_with("{\n  \"version\""),
        "record must be pretty-printed with 2-space indent (spec §3): {json_text}"
    );

    // Empty comments file exists at the bookmark.
    let comments_text =
        read_at_bookmark(&repo, &format!("bugs/{}.comments.jsonl", id_s));
    assert_eq!(comments_text, "");

    // set_status to closed.
    storage.set_status(&id, Status::Closed).expect("set_status");

    // bugs/<id>.json at the bookmark reflects the new status.
    let json_text = read_at_bookmark(&repo, &format!("bugs/{}.json", id_s));
    let v: serde_json::Value = serde_json::from_str(&json_text).unwrap();
    assert_eq!(v["status"], "closed");
    assert_eq!(v["version"], 1);

    // `jj log` for the file should show two mutating commits on top of
    // the seed commit (which doesn't touch this path). Newest first.
    let log = jj_capture(
        &[
            "log",
            "--no-graph",
            "-T",
            "description ++ \"\\n----\\n\"",
            &format!("root:bugs/{}.json", id_s),
        ],
        &repo,
    );
    let entries: Vec<&str> = log.split("\n----\n").filter(|s| !s.trim().is_empty()).collect();
    assert_eq!(
        entries.len(),
        2,
        "expected 2 commits touching bugs/{id_s}.json, got {}:\n{log}",
        entries.len()
    );
    // Newest first: set-status commit, then create commit.
    let set_status_msg = entries[0];
    let create_msg = entries[1];

    assert!(
        set_status_msg.contains("Jjf-Op: set-status"),
        "set-status commit missing trailer:\n{set_status_msg}"
    );
    assert!(
        set_status_msg.contains(&format!("Jjf-Bug: {}", id_s)),
        "set-status commit missing Jjf-Bug trailer:\n{set_status_msg}"
    );
    assert!(
        set_status_msg.contains("Jjf-Status: closed"),
        "set-status commit missing Jjf-Status: closed:\n{set_status_msg}"
    );

    assert!(
        create_msg.contains("Jjf-Op: create"),
        "create commit missing trailer:\n{create_msg}"
    );
    assert!(
        create_msg.contains(&format!("Jjf-Bug: {}", id_s)),
        "create commit missing Jjf-Bug trailer:\n{create_msg}"
    );
    assert!(
        create_msg.contains("Jjf-Title: segfault on empty input"),
        "create commit missing Jjf-Title trailer:\n{create_msg}"
    );
    assert!(
        create_msg.contains("Jjf-Status: open"),
        "create commit missing Jjf-Status: open:\n{create_msg}"
    );

    // The bookmark should now point at the latest mutation. Verify by
    // checking `jj log -r bookmarks(bugs)` shows the set-status commit.
    let tip = jj_capture(
        &[
            "log",
            "--no-graph",
            "-r",
            "bookmarks(bugs)",
            "-T",
            "description.first_line() ++ \"\\n\"",
        ],
        &repo,
    );
    assert!(
        tip.contains("set-status"),
        "bugs bookmark should point at the set-status commit, got: {tip}"
    );

    // @ should not be on the bookmark (step 4 of the dance).
    let at_at = jj_capture(
        &[
            "log",
            "--no-graph",
            "-r",
            "@",
            "-T",
            "bookmarks ++ \"\\n\"",
        ],
        &repo,
    );
    assert!(
        !at_at.contains("bugs"),
        "@ should not be on the bugs bookmark after a mutation, got: {at_at}"
    );
}

#[test]
fn add_comment_lands_jsonl_line_and_trailer() {
    let repo = make_scratch_repo("add_comment");
    let storage = Storage::open(&repo).unwrap();
    let id: BugId = storage
        .create_bug(&BugDraft {
            title: "needs more info".into(),
            ..Default::default()
        })
        .unwrap();
    let id_s = id.to_string();

    storage
        .add_comment(&id, "first thought", "alice <alice@example.com>")
        .unwrap();

    let body = read_at_bookmark(&repo, &format!("bugs/{}.comments.jsonl", id_s));
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one comment line: {body:?}");
    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(v["body"], "first thought");
    assert_eq!(v["author"], "alice <alice@example.com>");
    assert!(
        v["id"].as_str().unwrap().len() == 7,
        "comment id must be 7 hex chars: {body}"
    );

    // The comment-add commit's description must carry the trailer +
    // the Jjf-Comment-Id matching the line in jsonl.
    let log = jj_capture(
        &[
            "log",
            "--no-graph",
            "-r",
            "bookmarks(bugs)",
            "-T",
            "description ++ \"\\n\"",
        ],
        &repo,
    );
    assert!(log.contains("Jjf-Op: comment-add"), "missing trailer:\n{log}");
    assert!(
        log.contains(&format!("Jjf-Comment-Id: {}", v["id"].as_str().unwrap())),
        "trailer comment id mismatch:\n{log}"
    );
}
