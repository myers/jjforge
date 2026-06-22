//! Integration test: drive the 4-CLI write-path dance against a real
//! throwaway `jj` repo and assert what landed in the working copy and
//! commit history.
//!
//! Mirrors the hermetic-scratch style of `experiments/`: a per-test
//! directory under `tests/.scratch/`, wiped on each run, gitignored.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use jjf_storage::{BugDraft, BugId, Op, Status, Storage};
use serde::Serialize;

/// Build a scratch jj repo with a seeded `bugs` bookmark. Returns the
/// absolute path to the repo root.
///
/// Bootstrap is delegated to `Storage::init` — that's the function
/// under test for the `storage-bootstrap` ticket, and using it here
/// means every other integration test exercises it incidentally.
fn make_scratch_repo(name: &str) -> PathBuf {
    let abs = make_empty_jj_repo(name);
    // `init` is idempotent and produces the seed commit + `bugs`
    // bookmark in one call; the storage crate's first `jj new
    // bookmarks(bugs)` then branches from that seed cleanly.
    Storage::init(&abs).expect("Storage::init on fresh repo");
    abs
}

/// Build a scratch directory that's a jj repo but has no `bugs`
/// bookmark yet. Returns the absolute path. Use this when a test
/// wants to drive `Storage::init` itself.
fn make_empty_jj_repo(name: &str) -> PathBuf {
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
    abs
}

/// Build a scratch directory that's NOT a jj repo. Used by the
/// `Storage::init` typed-error test.
fn make_non_jj_dir(name: &str) -> PathBuf {
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(".scratch")
        .join(name);
    if scratch.exists() {
        fs::remove_dir_all(&scratch).unwrap();
    }
    fs::create_dir_all(&scratch).unwrap();
    fs::canonicalize(&scratch).unwrap()
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

// ---------------------------------------------------------------------
// Read-path tests (issue b650d74).
//
// The acceptance criteria call for:
//   1. A seeded repo with several mutations, read-back, all-field
//      assertion.
//   2. A round-trip property test: write produces files + trailers;
//      read produces a struct; serializing the struct back byte-equals
//      the file on disk.
//
// Both are exercised below.
// ---------------------------------------------------------------------

#[test]
fn read_roundtrip_after_multiple_mutations() {
    let repo = make_scratch_repo("read_roundtrip");
    let storage = Storage::open(&repo).unwrap();

    // 1. Create.
    let id = storage
        .create_bug(&BugDraft {
            title: "initial title".into(),
            body: "first body".into(),
            labels: vec!["bug".into()],
            dependencies: vec![],
            assignee: None,
        })
        .unwrap();

    // 2. set-status, set-title, two comments, label-add (the recipe
    // from the ticket's acceptance criteria).
    storage.set_status(&id, Status::Closed).unwrap();
    storage.set_title(&id, "final title").unwrap();
    storage
        .add_comment(&id, "first comment", "alice <a@x>")
        .unwrap();
    storage
        .add_comment(&id, "second comment", "bob <b@x>")
        .unwrap();
    storage.add_label(&id, "p1").unwrap();

    // 3. Read back and assert every field is what we expect.
    let bug = storage.read(&id).expect("read after mutations");

    assert_eq!(bug.id, id);
    assert_eq!(bug.title, "final title");
    assert_eq!(bug.body, "first body");
    assert_eq!(bug.status, Status::Closed);
    // Labels are sorted alphabetically per spec §3.1.
    assert_eq!(bug.labels, vec!["bug".to_string(), "p1".to_string()]);
    assert_eq!(bug.dependencies, Vec::<BugId>::new());
    assert_eq!(bug.assignee, None);

    // Two comments, chronological. The first add gets created_at
    // strictly <= the second's because the storage layer stamps both
    // from the same monotonic clock-source.
    assert_eq!(bug.comments.len(), 2);
    assert_eq!(bug.comments[0].body, "first comment");
    assert_eq!(bug.comments[0].author, "alice <a@x>");
    assert_eq!(bug.comments[1].body, "second comment");
    assert_eq!(bug.comments[1].author, "bob <b@x>");
    assert!(
        bug.comments[0].created_at <= bug.comments[1].created_at,
        "comments must be chronological: {:?} then {:?}",
        bug.comments[0].created_at,
        bug.comments[1].created_at,
    );

    // Timestamps are well-formed RFC 3339 strings and updated_at >=
    // created_at.
    assert_eq!(bug.created_at.len(), "2026-06-21T12:00:00Z".len());
    assert_eq!(bug.updated_at.len(), "2026-06-21T12:00:00Z".len());
    assert!(
        bug.updated_at >= bug.created_at,
        "updated_at must be >= created_at: created={}, updated={}",
        bug.created_at,
        bug.updated_at
    );
}

#[test]
fn read_missing_bug_returns_bug_not_found() {
    let repo = make_scratch_repo("read_missing");
    let storage = Storage::open(&repo).unwrap();
    let missing = BugId::parse("deadbee").unwrap();
    match storage.read(&missing) {
        Err(jjf_storage::Error::BugNotFound(got)) => assert_eq!(got, missing),
        other => panic!("expected BugNotFound, got {:?}", other),
    }
}

#[test]
fn read_then_serialize_byte_equals_on_disk_record() {
    // The v1 storage contract: the file on disk IS the read-path
    // result, byte-for-byte (after applying the writer's pretty-print
    // + field-ordering rules). This test holds the writer to that
    // contract by reading the on-disk bytes, reading the parsed Bug,
    // converting the Bug back into the canonical record shape, and
    // asserting the two byte buffers match.
    let repo = make_scratch_repo("read_byte_equal");
    let storage = Storage::open(&repo).unwrap();

    let id = storage
        .create_bug(&BugDraft {
            title: "round-trip me".into(),
            body: "body line 1\nbody line 2".into(),
            labels: vec!["needs-info".into(), "bug".into()],
            dependencies: vec![],
            assignee: Some("alice".into()),
        })
        .unwrap();
    storage.add_label(&id, "p2").unwrap();
    storage.add_comment(&id, "hi", "alice <a@x>").unwrap();

    let id_s = id.to_string();
    let on_disk = read_at_bookmark(&repo, &format!("bugs/{}.json", id_s));

    // Re-serialize the Bug back through the same writer convention
    // (pretty-printed, 2-space indent, trailing newline) and the
    // bytes must match. The shape used here mirrors the writer's
    // private `BugRecord` exactly — that's the contract.
    let bug = storage.read(&id).expect("read");

    #[derive(Serialize)]
    struct CanonicalRecord<'a> {
        version: u32,
        id: &'a BugId,
        title: &'a str,
        body: &'a str,
        status: &'a str,
        labels: &'a [String],
        dependencies: &'a [BugId],
        assignee: Option<&'a str>,
        created_at: &'a str,
        updated_at: &'a str,
    }

    let canonical = CanonicalRecord {
        version: 1,
        id: &bug.id,
        title: &bug.title,
        body: &bug.body,
        status: match bug.status {
            Status::Open => "open",
            Status::Closed => "closed",
        },
        labels: &bug.labels,
        dependencies: &bug.dependencies,
        assignee: bug.assignee.as_deref(),
        created_at: &bug.created_at,
        updated_at: &bug.updated_at,
    };
    let mut reserialized = serde_json::to_string_pretty(&canonical).unwrap();
    reserialized.push('\n');

    assert_eq!(
        reserialized, on_disk,
        "round-trip byte-equality failed.\nfile on disk:\n{on_disk}\nreserialized:\n{reserialized}"
    );

    // Same byte-equality contract for the comments file: each line is
    // a Comment serialized as compact JSON, terminated by `\n`.
    let on_disk_comments =
        read_at_bookmark(&repo, &format!("bugs/{}.comments.jsonl", id_s));
    let mut reserialized_comments = String::new();
    for c in &bug.comments {
        reserialized_comments.push_str(&serde_json::to_string(c).unwrap());
        reserialized_comments.push('\n');
    }
    assert_eq!(
        reserialized_comments, on_disk_comments,
        "comments-file round-trip byte-equality failed.\nfile on disk:\n{on_disk_comments}\nreserialized:\n{reserialized_comments}"
    );
}

#[test]
fn read_after_add_then_remove_label_observes_neither() {
    // Exercises the op-replay path through label-rm: the file ends up
    // without the label, and the op chain (label-add then label-rm)
    // also ends up without it. The debug-build cross-check would fire
    // here if the two views ever disagreed.
    let repo = make_scratch_repo("read_label_lifecycle");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_bug(&BugDraft {
            title: "label lifecycle".into(),
            ..Default::default()
        })
        .unwrap();
    storage.add_label(&id, "ephemeral").unwrap();
    storage.add_label(&id, "permanent").unwrap();
    storage.remove_label(&id, "ephemeral").unwrap();

    let bug = storage.read(&id).unwrap();
    assert_eq!(bug.labels, vec!["permanent".to_string()]);
}

// ---------------------------------------------------------------------
// History-path tests (issue 2f7e085).
//
// `Storage::read_history` returns one `HistoryEntry` per `Jjf-Op:`
// trailer on the chain, oldest first. The acceptance criterion:
// 4-5 distinct mutations including a multi-op create and a
// comment-add, with the returned op stream matching what was written
// in order.
// ---------------------------------------------------------------------

#[test]
fn read_history_returns_op_per_trailer_in_chronological_order() {
    let repo = make_scratch_repo("read_history");
    let storage = Storage::open(&repo).unwrap();

    // Mutation 1: multi-op create. The writer emits `create` + (per
    // spec §5.7, in this order) `set-body`, `label-add` × N (sorted),
    // `dep-add` × N (sorted), `set-assignee`. With body + 2 labels +
    // assignee that's 5 ops in one commit.
    let id = storage
        .create_bug(&BugDraft {
            title: "first title".into(),
            body: "initial body".into(),
            labels: vec!["bug".into(), "p1".into()],
            dependencies: vec![],
            assignee: Some("alice".into()),
        })
        .unwrap();

    // Mutation 2: set-title (single-op commit).
    storage.set_title(&id, "second title").unwrap();

    // Mutation 3: set-status to closed (single-op commit).
    storage.set_status(&id, Status::Closed).unwrap();

    // Mutation 4: add-comment — comments are ops too, in the same
    // stream as scalar changes.
    storage
        .add_comment(&id, "a thought", "alice <a@x>")
        .unwrap();

    // Mutation 5: label-rm (proves rm-shaped ops are visible too).
    storage.remove_label(&id, "bug").unwrap();

    let history = storage.read_history(&id).expect("read_history");

    // 5 ops from mutation 1 + 1 + 1 + 1 + 1 + 1 = 9 entries.
    assert_eq!(
        history.len(),
        9,
        "expected 9 history entries, got {}: {:#?}",
        history.len(),
        history,
    );

    // Per-op assertions, oldest first.
    // ---- create commit (multi-op stanza per spec §5.7) ----
    match &history[0].op {
        Op::Create { bug_id, title, status } => {
            assert_eq!(bug_id, &id);
            assert_eq!(title, "first title");
            assert_eq!(*status, Status::Open);
        }
        other => panic!("history[0] expected Create, got {:?}", other),
    }
    match &history[1].op {
        Op::SetBody { bug_id, body_hash } => {
            assert_eq!(bug_id, &id);
            assert_eq!(body_hash.len(), 64, "sha-256 hex is 64 chars");
        }
        other => panic!("history[1] expected SetBody, got {:?}", other),
    }
    match &history[2].op {
        Op::LabelAdd { bug_id, label } => {
            assert_eq!(bug_id, &id);
            assert_eq!(label, "bug"); // labels sorted alphabetically
        }
        other => panic!("history[2] expected LabelAdd(bug), got {:?}", other),
    }
    match &history[3].op {
        Op::LabelAdd { bug_id, label } => {
            assert_eq!(bug_id, &id);
            assert_eq!(label, "p1");
        }
        other => panic!("history[3] expected LabelAdd(p1), got {:?}", other),
    }
    match &history[4].op {
        Op::SetAssignee { bug_id, assignee } => {
            assert_eq!(bug_id, &id);
            assert_eq!(assignee.as_deref(), Some("alice"));
        }
        other => panic!("history[4] expected SetAssignee, got {:?}", other),
    }

    // All 5 ops above share the same commit (the multi-op create),
    // which is the whole point of spec §5.5/§5.7.
    let create_commit = &history[0].commit;
    for i in 1..5 {
        assert_eq!(
            &history[i].commit, create_commit,
            "history[{}] should share the create commit but differs: {} vs {}",
            i, history[i].commit, create_commit,
        );
        assert_eq!(&history[i].timestamp, &history[0].timestamp);
        assert_eq!(&history[i].author, &history[0].author);
    }

    // ---- set-title commit ----
    match &history[5].op {
        Op::SetTitle { bug_id, title } => {
            assert_eq!(bug_id, &id);
            assert_eq!(title, "second title");
        }
        other => panic!("history[5] expected SetTitle, got {:?}", other),
    }
    assert_ne!(
        &history[5].commit, create_commit,
        "set-title must land on its own commit"
    );

    // ---- set-status commit ----
    match &history[6].op {
        Op::SetStatus { bug_id, status } => {
            assert_eq!(bug_id, &id);
            assert_eq!(*status, Status::Closed);
        }
        other => panic!("history[6] expected SetStatus, got {:?}", other),
    }

    // ---- comment-add commit ----
    match &history[7].op {
        Op::CommentAdd { bug_id, comment_id } => {
            assert_eq!(bug_id, &id);
            // Comment id should match the one in the comments file.
            let bug = storage.read(&id).unwrap();
            assert_eq!(bug.comments.len(), 1);
            assert_eq!(comment_id, &bug.comments[0].id);
        }
        other => panic!("history[7] expected CommentAdd, got {:?}", other),
    }

    // ---- label-rm commit ----
    match &history[8].op {
        Op::LabelRm { bug_id, label } => {
            assert_eq!(bug_id, &id);
            assert_eq!(label, "bug");
        }
        other => panic!("history[8] expected LabelRm, got {:?}", other),
    }

    // Timestamps strictly non-decreasing across commits (a commit
    // can't have an earlier author timestamp than its parent).
    for i in 1..history.len() {
        assert!(
            history[i].timestamp >= history[i - 1].timestamp,
            "history timestamps must be non-decreasing: history[{}]={} < history[{}]={}",
            i, history[i].timestamp,
            i - 1, history[i - 1].timestamp,
        );
    }

    // Every entry has a non-empty commit id (jj's commit_id is always
    // a 40-char hex sha-1) and a well-formed timestamp.
    for (i, entry) in history.iter().enumerate() {
        assert_eq!(
            entry.commit.len(),
            40,
            "history[{}] commit id should be 40 hex chars, got {:?}",
            i, entry.commit,
        );
        assert_eq!(
            entry.timestamp.len(),
            "2026-06-21T12:00:00Z".len(),
            "history[{}] timestamp should be RFC3339 Z-form, got {:?}",
            i, entry.timestamp,
        );
    }
}

#[test]
fn read_history_missing_bug_returns_bug_not_found() {
    let repo = make_scratch_repo("read_history_missing");
    let storage = Storage::open(&repo).unwrap();
    let missing = BugId::parse("deadbee").unwrap();
    match storage.read_history(&missing) {
        Err(jjf_storage::Error::BugNotFound(got)) => assert_eq!(got, missing),
        other => panic!("expected BugNotFound, got {:?}", other),
    }
}

// ---------------------------------------------------------------------
// Bootstrap-path tests (issue 8b12f9d).
//
// `Storage::init` bootstraps the `bugs` bookmark idempotently. Spec
// §1.1 pins the seed-commit description; the three distinct failure
// shapes (not-a-jj-repo, bookmark-missing, bookmark-present) all need
// coverage.
// ---------------------------------------------------------------------

#[test]
fn init_on_fresh_jj_repo_creates_bookmark_with_seed_commit() {
    let repo = make_empty_jj_repo("init_fresh");

    // Pre-condition: no `bugs` bookmark.
    let pre = jj_capture(&["bookmark", "list", "-T", "name ++ \"\\n\""], &repo);
    assert!(
        !pre.lines().any(|l| l.trim() == "bugs"),
        "pre-condition: bookmark should not exist yet, got: {pre}"
    );

    Storage::init(&repo).expect("Storage::init on fresh repo");

    // Post-condition: bookmark exists, points at one commit whose
    // description matches the spec.
    let post = jj_capture(&["bookmark", "list", "-T", "name ++ \"\\n\""], &repo);
    assert!(
        post.lines().any(|l| l.trim() == "bugs"),
        "post-condition: bookmark should exist, got: {post}"
    );

    let seed_desc = jj_capture(
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
    assert_eq!(
        seed_desc.trim(),
        "jjf: seed bugs bookmark",
        "seed commit description must match spec §1.1, got: {seed_desc:?}"
    );

    // The bookmark should resolve to exactly one commit (no chain
    // yet beyond the seed) when scoped to non-root().
    let count = jj_capture(
        &[
            "log",
            "--no-graph",
            "-r",
            "bookmarks(bugs) ~ root()",
            "-T",
            "\"x\"",
        ],
        &repo,
    );
    assert_eq!(
        count, "x",
        "exactly one non-root commit on the bookmark expected, got: {count:?}"
    );

    // Step 3 of bootstrap (`jj new root()`) leaves @ off the bookmark,
    // matching the invariant the writer dance relies on.
    let at_bookmarks = jj_capture(
        &["log", "--no-graph", "-r", "@", "-T", "bookmarks ++ \"\\n\""],
        &repo,
    );
    assert!(
        !at_bookmarks.contains("bugs"),
        "@ should not be on the bugs bookmark after init, got: {at_bookmarks:?}"
    );
}

#[test]
fn init_is_idempotent_when_called_twice() {
    let repo = make_empty_jj_repo("init_twice");

    Storage::init(&repo).expect("first init");

    // Capture the bookmark's commit id after the first init so we can
    // assert the second init didn't move it.
    let first_tip = jj_capture(
        &[
            "log",
            "--no-graph",
            "-r",
            "bookmarks(bugs)",
            "-T",
            "commit_id ++ \"\\n\"",
        ],
        &repo,
    );
    assert!(
        !first_tip.trim().is_empty(),
        "first init should have created a bookmark, got: {first_tip:?}"
    );

    Storage::init(&repo).expect("second init must be a no-op success");

    let second_tip = jj_capture(
        &[
            "log",
            "--no-graph",
            "-r",
            "bookmarks(bugs)",
            "-T",
            "commit_id ++ \"\\n\"",
        ],
        &repo,
    );
    assert_eq!(
        first_tip, second_tip,
        "second init must not move the bookmark: first={first_tip:?}, second={second_tip:?}"
    );

    // And exactly one commit (the seed) is reachable from the bookmark
    // — the second init must not have produced another seed.
    let non_root_count = jj_capture(
        &[
            "log",
            "--no-graph",
            "-r",
            "ancestors(bookmarks(bugs)) ~ root()",
            "-T",
            "\"x\\n\"",
        ],
        &repo,
    );
    assert_eq!(
        non_root_count.lines().count(),
        1,
        "exactly one non-root commit reachable from bookmark expected, got: {non_root_count:?}"
    );
}

#[test]
fn init_outside_any_jj_repo_returns_typed_error() {
    let bare = make_non_jj_dir("init_no_repo");
    match Storage::init(&bare) {
        Err(jjf_storage::Error::NotAJjRepo(got)) => assert_eq!(got, bare),
        other => panic!("expected NotAJjRepo, got {:?}", other),
    }
}

#[test]
fn init_then_create_bug_lands_on_top_of_seed() {
    // End-to-end: init bootstraps, then create_bug uses the bookmark
    // just like every other test does. Confirms the seed commit is a
    // viable parent for the first mutation.
    let repo = make_empty_jj_repo("init_then_create");
    let storage = Storage::init(&repo).unwrap();

    let id = storage
        .create_bug(&BugDraft {
            title: "first ever bug".into(),
            ..Default::default()
        })
        .expect("create_bug on freshly-init'd repo");

    let bug = storage.read(&id).expect("read after create");
    assert_eq!(bug.title, "first ever bug");

    // The bookmark should now point at the create commit (not the
    // seed); the seed is one parent back.
    let chain = jj_capture(
        &[
            "log",
            "--no-graph",
            "-r",
            "ancestors(bookmarks(bugs)) ~ root()",
            "-T",
            "description.first_line() ++ \"\\n\"",
        ],
        &repo,
    );
    let descs: Vec<&str> = chain.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        descs.len(),
        2,
        "expected seed + 1 mutation on the chain, got: {chain:?}"
    );
    assert!(
        descs[0].contains(&format!("bug {}", id)),
        "newest commit should be the create, got: {chain:?}"
    );
    assert_eq!(
        descs[1], "jjf: seed bugs bookmark",
        "oldest commit should be the seed, got: {chain:?}"
    );
}

// ---------------------------------------------------------------------
// Enumeration-path tests (issue 6b2b555).
//
// `Storage::list_ids` is the first multi-bug primitive — `jjf ls`'s
// foundation. Tests cover: empty bookmark returns empty; three bugs
// return their ids sorted ascending; comments-jsonl siblings don't
// cause double-counting.
// ---------------------------------------------------------------------

#[test]
fn list_ids_on_empty_bookmark_returns_empty() {
    let repo = make_scratch_repo("list_ids_empty");
    let storage = Storage::open(&repo).unwrap();
    let ids = storage.list_ids().expect("list_ids on empty bookmark");
    assert!(
        ids.is_empty(),
        "empty bookmark should yield zero ids, got: {ids:?}"
    );
}

#[test]
fn list_ids_returns_three_bugs_sorted_with_no_duplicates() {
    let repo = make_scratch_repo("list_ids_three");
    let storage = Storage::open(&repo).unwrap();

    // Three bugs. Each one's create lands both `bugs/<id>.json` AND
    // `bugs/<id>.comments.jsonl` at the bookmark tip — the latter is
    // the regression we're guarding against (no double-counting).
    let mut created: Vec<BugId> = Vec::with_capacity(3);
    for title in ["first", "second", "third"] {
        let id = storage
            .create_bug(&BugDraft {
                title: (*title).into(),
                ..Default::default()
            })
            .expect("create_bug");
        created.push(id);
    }

    let ids = storage.list_ids().expect("list_ids after 3 creates");

    // Exactly 3 (not 6 — `.comments.jsonl` siblings must not show up).
    assert_eq!(
        ids.len(),
        3,
        "expected 3 ids, got {} ({ids:?}): comments-jsonl files may be double-counting",
        ids.len(),
    );

    // Same set as what we created.
    let mut expected = created.clone();
    expected.sort();
    assert_eq!(ids, expected, "list_ids must return the same ids that were created");

    // Sorted ascending (the API contract). `sort()` on the expected
    // gives the same answer, but assert it explicitly so a regression
    // that returns insertion-order or reverse-sorted is caught.
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "ids must be sorted ascending");
}
