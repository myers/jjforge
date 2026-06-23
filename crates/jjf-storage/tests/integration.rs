//! Integration test: drive the 4-CLI write-path dance against a real
//! throwaway `jj` repo and assert what landed in the working copy and
//! commit history.
//!
//! Mirrors the hermetic-scratch style of `experiments/`: a per-test
//! directory under `tests/.scratch/`, wiped on each run, gitignored.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use jjf_storage::{
    Error as StorageError, IssueDraft, IssueId, IssueType, Op,
    ReadyFilter, SlugInvalidReason, Status, Storage, UpdateFields,
};
use serde::Serialize;

/// Build a scratch jj repo with a seeded `issues` bookmark. Returns the
/// absolute path to the repo root.
///
/// Bootstrap is delegated to `Storage::init` — that's the function
/// under test for the `storage-bootstrap` ticket, and using it here
/// means every other integration test exercises it incidentally.
fn make_scratch_repo(name: &str) -> PathBuf {
    let abs = make_empty_jj_repo(name);
    // `init` is idempotent and produces the seed commit + `bugs`
    // bookmark in one call; the storage crate's first `jj new
    // bookmarks(issues)` then branches from that seed cleanly.
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

/// Read a file's contents from the `issues` bookmark tip.
fn read_at_bookmark(repo: &Path, relpath: &str) -> String {
    jj_capture(
        &[
            "file",
            "show",
            "-r",
            "bookmarks(issues)",
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

    let draft = IssueDraft {
        title: "segfault on empty input".into(),
        body: "Running `./app` with no arguments crashes.".into(),
        labels: vec!["bug".into(), "p1".into()],
        dependencies: vec![],
        assignee: Some("alice".into()),
        ..Default::default()
    };
    let id = storage.create_issue(&draft).expect("create_issue");
    let id_s = id.to_string();
    assert_eq!(id_s.len(), 7);

    // bugs/<id>.json exists at the bookmark tip with the schema fields.
    // (The dance's step 4 — `jj new root()` — moves @ off the bookmark,
    // so the file is not in the working copy. The authoritative copy
    // lives at the bookmark; read via `jj file show`.)
    let json_text = read_at_bookmark(&repo, &format!("issues/{}.json", id_s));
    let v: serde_json::Value = serde_json::from_str(&json_text).unwrap();
    assert_eq!(v["version"], 2);
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
        read_at_bookmark(&repo, &format!("issues/{}.comments.jsonl", id_s));
    assert_eq!(comments_text, "");

    // set_status to closed.
    storage.set_status(&id, Status::Closed).expect("set_status");

    // bugs/<id>.json at the bookmark reflects the new status.
    let json_text = read_at_bookmark(&repo, &format!("issues/{}.json", id_s));
    let v: serde_json::Value = serde_json::from_str(&json_text).unwrap();
    assert_eq!(v["status"], "closed");
    assert_eq!(v["version"], 2);

    // `jj log` for the file should show two mutating commits on top of
    // the seed commit (which doesn't touch this path). Newest first.
    let log = jj_capture(
        &[
            "log",
            "--no-graph",
            "-T",
            "description ++ \"\\n----\\n\"",
            &format!("root:issues/{}.json", id_s),
        ],
        &repo,
    );
    let entries: Vec<&str> = log.split("\n----\n").filter(|s| !s.trim().is_empty()).collect();
    assert_eq!(
        entries.len(),
        2,
        "expected 2 commits touching issues/{id_s}.json, got {}:\n{log}",
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
        set_status_msg.contains(&format!("Jjf-Issue: {}", id_s)),
        "set-status commit missing Jjf-Issue trailer:\n{set_status_msg}"
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
        create_msg.contains(&format!("Jjf-Issue: {}", id_s)),
        "create commit missing Jjf-Issue trailer:\n{create_msg}"
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
    // checking `jj log -r bookmarks(issues)` shows the set-status commit.
    let tip = jj_capture(
        &[
            "log",
            "--no-graph",
            "-r",
            "bookmarks(issues)",
            "-T",
            "description.first_line() ++ \"\\n\"",
        ],
        &repo,
    );
    assert!(
        tip.contains("set-status"),
        "issues bookmark should point at the set-status commit, got: {tip}"
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
        !at_at.contains("issues"),
        "@ should not be on the issues bookmark after a mutation, got: {at_at}"
    );
}

#[test]
fn add_comment_lands_jsonl_line_and_trailer() {
    let repo = make_scratch_repo("add_comment");
    let storage = Storage::open(&repo).unwrap();
    let id: IssueId = storage
        .create_issue(&IssueDraft {
            title: "needs more info".into(),
            ..Default::default()
        })
        .unwrap();
    let id_s = id.to_string();

    storage
        .add_comment(&id, "first thought", "alice <alice@example.com>")
        .unwrap();

    let body = read_at_bookmark(&repo, &format!("issues/{}.comments.jsonl", id_s));
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
            "bookmarks(issues)",
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
        .create_issue(&IssueDraft {
            title: "initial title".into(),
            body: "first body".into(),
            labels: vec!["bug".into()],
            dependencies: vec![],
            assignee: None,
            ..Default::default()
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
    assert_eq!(bug.dependencies, Vec::<IssueId>::new());
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
fn read_missing_bug_returns_issue_not_found() {
    let repo = make_scratch_repo("read_missing");
    let storage = Storage::open(&repo).unwrap();
    let missing = IssueId::parse("deadbee").unwrap();
    match storage.read(&missing) {
        Err(jjf_storage::Error::IssueNotFound(got)) => assert_eq!(got, missing),
        other => panic!("expected IssueNotFound, got {:?}", other),
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
        .create_issue(&IssueDraft {
            title: "round-trip me".into(),
            body: "body line 1\nbody line 2".into(),
            labels: vec!["needs-info".into(), "bug".into()],
            dependencies: vec![],
            assignee: Some("alice".into()),
            ..Default::default()
        })
        .unwrap();
    storage.add_label(&id, "p2").unwrap();
    storage.add_comment(&id, "hi", "alice <a@x>").unwrap();

    let id_s = id.to_string();
    let on_disk = read_at_bookmark(&repo, &format!("issues/{}.json", id_s));

    // Re-serialize the Bug back through the same writer convention
    // (pretty-printed, 2-space indent, trailing newline) and the
    // bytes must match. The shape used here mirrors the writer's
    // private `IssueRecord` exactly — that's the contract.
    let bug = storage.read(&id).expect("read");

    #[derive(Serialize)]
    struct CanonicalRecord<'a> {
        version: u32,
        id: &'a IssueId,
        title: &'a str,
        slug: Option<&'a str>,
        body: &'a str,
        status: &'a str,
        #[serde(rename = "type")]
        type_: &'a str,
        labels: &'a [String],
        dependencies: &'a [IssueId],
        assignee: Option<&'a str>,
        created_at: &'a str,
        updated_at: &'a str,
    }

    let canonical = CanonicalRecord {
        version: 2,
        id: &bug.id,
        title: &bug.title,
        slug: bug.slug.as_deref(),
        body: &bug.body,
        status: match bug.status {
            Status::Open => "open",
            Status::Closed => "closed",
        },
        type_: bug.type_.as_str(),
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
        read_at_bookmark(&repo, &format!("issues/{}.comments.jsonl", id_s));
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
        .create_issue(&IssueDraft {
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
        .create_issue(&IssueDraft {
            title: "first title".into(),
            body: "initial body".into(),
            labels: vec!["bug".into(), "p1".into()],
            dependencies: vec![],
            assignee: Some("alice".into()),
            ..Default::default()
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
        Op::Create { issue_id, title, status } => {
            assert_eq!(issue_id, &id);
            assert_eq!(title, "first title");
            assert_eq!(*status, Status::Open);
        }
        other => panic!("history[0] expected Create, got {:?}", other),
    }
    match &history[1].op {
        Op::SetBody { issue_id, body_hash } => {
            assert_eq!(issue_id, &id);
            assert_eq!(body_hash.len(), 64, "sha-256 hex is 64 chars");
        }
        other => panic!("history[1] expected SetBody, got {:?}", other),
    }
    match &history[2].op {
        Op::LabelAdd { issue_id, label } => {
            assert_eq!(issue_id, &id);
            assert_eq!(label, "bug"); // labels sorted alphabetically
        }
        other => panic!("history[2] expected LabelAdd(bug), got {:?}", other),
    }
    match &history[3].op {
        Op::LabelAdd { issue_id, label } => {
            assert_eq!(issue_id, &id);
            assert_eq!(label, "p1");
        }
        other => panic!("history[3] expected LabelAdd(p1), got {:?}", other),
    }
    match &history[4].op {
        Op::SetAssignee { issue_id, assignee } => {
            assert_eq!(issue_id, &id);
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
        Op::SetTitle { issue_id, title } => {
            assert_eq!(issue_id, &id);
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
        Op::SetStatus { issue_id, status } => {
            assert_eq!(issue_id, &id);
            assert_eq!(*status, Status::Closed);
        }
        other => panic!("history[6] expected SetStatus, got {:?}", other),
    }

    // ---- comment-add commit ----
    match &history[7].op {
        Op::CommentAdd { issue_id, comment_id } => {
            assert_eq!(issue_id, &id);
            // Comment id should match the one in the comments file.
            let bug = storage.read(&id).unwrap();
            assert_eq!(bug.comments.len(), 1);
            assert_eq!(comment_id, &bug.comments[0].id);
        }
        other => panic!("history[7] expected CommentAdd, got {:?}", other),
    }

    // ---- label-rm commit ----
    match &history[8].op {
        Op::LabelRm { issue_id, label } => {
            assert_eq!(issue_id, &id);
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
fn read_history_missing_bug_returns_issue_not_found() {
    let repo = make_scratch_repo("read_history_missing");
    let storage = Storage::open(&repo).unwrap();
    let missing = IssueId::parse("deadbee").unwrap();
    match storage.read_history(&missing) {
        Err(jjf_storage::Error::IssueNotFound(got)) => assert_eq!(got, missing),
        other => panic!("expected IssueNotFound, got {:?}", other),
    }
}

// ---------------------------------------------------------------------
// Storage::update tests (issue fdd0c7f).
//
// The whole point of the typed update API is multi-op-per-commit: a
// caller bundles N field changes; the storage layer lands ONE commit
// carrying N `Jjf-Op:` trailers. The read-back record reflects every
// change, and `read_history` exposes the trailers as N entries that
// share a single `commit` id.
// ---------------------------------------------------------------------

#[test]
fn update_lands_one_commit_with_one_trailer_per_populated_field() {
    use jjf_storage::UpdateFields;

    let repo = make_scratch_repo("update_multi_op");
    let storage = Storage::open(&repo).unwrap();

    // Seed a bug.
    let id = storage
        .create_issue(&IssueDraft {
            title: "before".into(),
            body: "before body".into(),
            labels: vec![],
            dependencies: vec![],
            assignee: None,
            ..Default::default()
        })
        .unwrap();

    // Baseline op count, so we can assert the delta below.
    let baseline = storage.read_history(&id).expect("read_history baseline").len();

    // Populate three fields. The fourth (assignee) is None — left alone.
    storage
        .update(
            &id,
            UpdateFields {
                title: Some("after".into()),
                status: Some(Status::Closed),
                body: Some("after body".into()),
                assignee: None,
                ..Default::default()
            },
        )
        .expect("update three fields");

    // Three populated fields => three NEW history entries, all on the
    // SAME commit (one commit, three trailers). This is the load-bearing
    // assertion the ticket calls out.
    let history = storage.read_history(&id).expect("read_history after");
    let new = &history[baseline..];
    assert_eq!(
        new.len(),
        3,
        "expected three new ops (title/status/body), got {}: {:#?}",
        new.len(),
        new,
    );
    let commit = &new[0].commit;
    assert!(
        new.iter().all(|e| &e.commit == commit),
        "all new ops must share one commit, got: {:#?}",
        new,
    );

    // Op order follows UpdateFields field-declaration order
    // (title, status, body, assignee). Spec §5.7 convention.
    match &new[0].op {
        Op::SetTitle { title, .. } => assert_eq!(title, "after"),
        other => panic!("new[0] expected SetTitle, got {:?}", other),
    }
    match &new[1].op {
        Op::SetStatus { status, .. } => assert_eq!(*status, Status::Closed),
        other => panic!("new[1] expected SetStatus, got {:?}", other),
    }
    match &new[2].op {
        Op::SetBody { body_hash, .. } => {
            assert_eq!(body_hash.len(), 64, "sha-256 hex is 64 chars");
        }
        other => panic!("new[2] expected SetBody, got {:?}", other),
    }

    // Record-level read agrees with what we wrote.
    let bug = storage.read(&id).unwrap();
    assert_eq!(bug.title, "after");
    assert_eq!(bug.status, Status::Closed);
    assert_eq!(bug.body, "after body");
    // assignee was None in the update bundle => unchanged.
    assert_eq!(bug.assignee, None);
}

#[test]
fn update_assignee_double_option_distinguishes_set_from_unset() {
    use jjf_storage::UpdateFields;

    let repo = make_scratch_repo("update_assignee_double_option");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "assign me".into(),
            body: String::new(),
            labels: vec![],
            dependencies: vec![],
            assignee: None,
            ..Default::default()
        })
        .unwrap();

    // Some(Some("alice")) sets the assignee.
    storage
        .update(
            &id,
            UpdateFields {
                assignee: Some(Some("alice".into())),
                ..UpdateFields::default()
            },
        )
        .expect("update assignee set");
    let bug = storage.read(&id).unwrap();
    assert_eq!(bug.assignee.as_deref(), Some("alice"));

    // Some(None) clears the assignee.
    storage
        .update(
            &id,
            UpdateFields {
                assignee: Some(None),
                ..UpdateFields::default()
            },
        )
        .expect("update assignee unset");
    let bug = storage.read(&id).unwrap();
    assert_eq!(bug.assignee, None);
}

#[test]
fn update_with_no_fields_is_an_error() {
    use jjf_storage::UpdateFields;

    let repo = make_scratch_repo("update_no_fields");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "noop".into(),
            body: String::new(),
            labels: vec![],
            dependencies: vec![],
            assignee: None,
            ..Default::default()
        })
        .unwrap();

    match storage.update(&id, UpdateFields::default()) {
        Err(jjf_storage::Error::Invalid(msg)) => {
            assert!(
                msg.contains("no fields"),
                "Invalid message should mention `no fields`, got: {msg}"
            );
        }
        other => panic!("expected Invalid, got {:?}", other),
    }
}

// ---------------------------------------------------------------------
// Bootstrap-path tests (issue 8b12f9d).
//
// `Storage::init` bootstraps the `issues` bookmark idempotently. Spec
// §1.1 pins the seed-commit description; the three distinct failure
// shapes (not-a-jj-repo, bookmark-missing, bookmark-present) all need
// coverage.
// ---------------------------------------------------------------------

#[test]
fn init_on_fresh_jj_repo_creates_bookmark_with_seed_commit() {
    let repo = make_empty_jj_repo("init_fresh");

    // Pre-condition: no `issues` bookmark.
    let pre = jj_capture(&["bookmark", "list", "-T", "name ++ \"\\n\""], &repo);
    assert!(
        !pre.lines().any(|l| l.trim() == "issues"),
        "pre-condition: bookmark should not exist yet, got: {pre}"
    );

    Storage::init(&repo).expect("Storage::init on fresh repo");

    // Post-condition: bookmark exists, points at one commit whose
    // description matches the spec.
    let post = jj_capture(&["bookmark", "list", "-T", "name ++ \"\\n\""], &repo);
    assert!(
        post.lines().any(|l| l.trim() == "issues"),
        "post-condition: bookmark should exist, got: {post}"
    );

    let seed_desc = jj_capture(
        &[
            "log",
            "--no-graph",
            "-r",
            "bookmarks(issues)",
            "-T",
            "description.first_line() ++ \"\\n\"",
        ],
        &repo,
    );
    assert_eq!(
        seed_desc.trim(),
        "jjf: seed issues bookmark",
        "seed commit description must match spec §1.1, got: {seed_desc:?}"
    );

    // The bookmark should resolve to exactly one commit (no chain
    // yet beyond the seed) when scoped to non-root().
    let count = jj_capture(
        &[
            "log",
            "--no-graph",
            "-r",
            "bookmarks(issues) ~ root()",
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
        !at_bookmarks.contains("issues"),
        "@ should not be on the issues bookmark after init, got: {at_bookmarks:?}"
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
            "bookmarks(issues)",
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
            "bookmarks(issues)",
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
            "ancestors(bookmarks(issues)) ~ root()",
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
fn init_then_create_issue_lands_on_top_of_seed() {
    // End-to-end: init bootstraps, then create_issue uses the bookmark
    // just like every other test does. Confirms the seed commit is a
    // viable parent for the first mutation.
    let repo = make_empty_jj_repo("init_then_create");
    let storage = Storage::init(&repo).unwrap();

    let id = storage
        .create_issue(&IssueDraft {
            title: "first ever bug".into(),
            ..Default::default()
        })
        .expect("create_issue on freshly-init'd repo");

    let bug = storage.read(&id).expect("read after create");
    assert_eq!(bug.title, "first ever bug");

    // The bookmark should now point at the create commit (not the
    // seed); the seed is one parent back.
    let chain = jj_capture(
        &[
            "log",
            "--no-graph",
            "-r",
            "ancestors(bookmarks(issues)) ~ root()",
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
        descs[0].contains(&format!("issue {}", id)),
        "newest commit should be the create, got: {chain:?}"
    );
    assert_eq!(
        descs[1], "jjf: seed issues bookmark",
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

    // Three bugs. Each one's create lands both `issues/<id>.json` AND
    // `issues/<id>.comments.jsonl` at the bookmark tip — the latter is
    // the regression we're guarding against (no double-counting).
    let mut created: Vec<IssueId> = Vec::with_capacity(3);
    for title in ["first", "second", "third"] {
        let id = storage
            .create_issue(&IssueDraft {
                title: (*title).into(),
                ..Default::default()
            })
            .expect("create_issue");
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

/// V1 → v2 migration end-to-end: synthesize a pre-migration repo by
/// renaming a freshly-created v2 repo back to the v1 shape (paths
/// `bugs/<id>.*` and bookmark `bugs`), then call `Storage::open` and
/// assert the migration runs AND post-migration `Storage::read`
/// finds the issue's full history.
///
/// This catches the regression that shipped in commit 20efe38: the
/// migration renamed paths correctly but the read-side path filter
/// only looked at `issues/<id>.*` — every pre-migration commit
/// (containing the `create` op for the issue) dropped out of the
/// chain and `read` failed with "no `create` op found."
#[test]
fn v1_to_v2_migration_preserves_history() {
    let repo = make_scratch_repo("v1_to_v2_migration_preserves_history");

    // Create an issue in v2 form so we have real `Jjf-Op:` trailers
    // and real on-disk record files. Land two ops (create + close)
    // so the history walker has a non-trivial chain to follow.
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "synthetic v1 issue".into(),
            ..IssueDraft::default()
        })
        .unwrap();
    storage.set_status(&id, Status::Closed).unwrap();
    drop(storage);

    // Rewrite the bookmark + paths to look like v1. We're synthesizing
    // a pre-migration state from a known-good post-migration state.
    // Three jj operations: rename the bookmark, move the files, then
    // step off (so the next Storage::open sees a clean working copy).
    let json_old = format!("issues/{}.json", id);
    let comments_old = format!("issues/{}.comments.jsonl", id);
    let json_new = format!("bugs/{}.json", id);
    let comments_new = format!("bugs/{}.comments.jsonl", id);

    // Edit the bookmark tip: a new commit that moves the files from
    // issues/ to bugs/. This is the inverse of the migration commit
    // the storage layer produces.
    sh("jj", &["new", "bookmarks(issues)", "-m", "synthesize v1 layout"], &repo);
    fs::create_dir_all(repo.join("bugs")).unwrap();
    fs::rename(repo.join(&json_old), repo.join(&json_new)).unwrap();
    fs::rename(repo.join(&comments_old), repo.join(&comments_new)).unwrap();
    let _ = fs::remove_dir(repo.join("issues"));

    // Rename bookmark issues → bugs.
    sh("jj", &["bookmark", "create", "bugs", "-r", "@"], &repo);
    sh("jj", &["bookmark", "delete", "issues"], &repo);
    sh("jj", &["new", "root()"], &repo);

    // Sanity check: we're now in v1 shape.
    let bookmarks = jj_capture(
        &["bookmark", "list", "-T", "name ++ \"\\n\""],
        &repo,
    );
    assert!(
        bookmarks.lines().any(|l| l.trim() == "bugs"),
        "synthesized v1 must have a `bugs` bookmark, got:\n{bookmarks}"
    );
    assert!(
        !bookmarks.lines().any(|l| l.trim() == "issues"),
        "synthesized v1 must NOT have an `issues` bookmark, got:\n{bookmarks}"
    );

    // The actual test: Storage::open detects v1, runs the migration,
    // and Storage::read succeeds with the full chain (NOT just the
    // migration commit — the original create + set-status ops must
    // be found via the v1 path filter).
    let storage = Storage::open(&repo).expect("Storage::open must succeed on v1 repo");
    let bug = storage
        .read(&id)
        .expect("Storage::read must succeed post-migration; the read-side path filter must include the v1 `bugs/` paths so pre-migration commits are visible");
    assert_eq!(bug.title, "synthetic v1 issue");
    assert_eq!(bug.status, Status::Closed);

    // Bookmark renamed.
    let bookmarks_post = jj_capture(
        &["bookmark", "list", "-T", "name ++ \"\\n\""],
        &repo,
    );
    assert!(
        bookmarks_post.lines().any(|l| l.trim() == "issues"),
        "post-migration must have an `issues` bookmark, got:\n{bookmarks_post}"
    );

    // History reader sees the full chain (create + set-status +
    // migration commit's op-free description = at least 2 trailer
    // entries: create's multi-op stanza, set-status).
    let history = storage.read_history(&id).expect("history readable after migration");
    let has_create = history.iter().any(|h| matches!(h.op, Op::Create { .. }));
    let has_set_status = history.iter().any(|h| matches!(h.op, Op::SetStatus { .. }));
    assert!(
        has_create,
        "history must include the original `create` op; the v1 path filter is what makes it visible. got {} entries",
        history.len()
    );
    assert!(
        has_set_status,
        "history must include the `set-status` op landed before the synthesized v1 rewrite. got {} entries",
        history.len()
    );
}

// ---------------------------------------------------------------------
// v2.1 type + slug tests (issue 7100b51).
//
// The ticket calls out:
//   - IssueType enum + serde roundtrip.
//   - Slug field + validation tests covering each rejection reason.
//   - Storage::resolve accepts id OR slug; tests cover both paths.
//   - Slug uniqueness enforced at write; collision integration test.
//   - Slug uniqueness scope is OPEN only — close releases, recreate
//     succeeds.
//   - Op::SetType and Op::SetSlug parse + replay + history-reader
//     surface.
//   - Storage::update lands both trailers; read_history verifies.
// ---------------------------------------------------------------------

#[test]
fn issue_type_serde_roundtrip() {
    // Each named variant's wire spelling is its lowercase
    // name. Default = Unspecified.
    for kind in [
        IssueType::Bug,
        IssueType::Feature,
        IssueType::Epic,
        IssueType::Research,
        IssueType::Roadmap,
        IssueType::Unspecified,
    ] {
        let s = serde_json::to_string(&kind).unwrap();
        let back: IssueType = serde_json::from_str(&s).unwrap();
        assert_eq!(back, kind);
    }
    assert_eq!(IssueType::default(), IssueType::Unspecified);
}

#[test]
fn validate_slug_accepts_canonical_shape() {
    // The good cases. Each must pass.
    for ok in ["abc", "agent-ready", "issue-type-and-slug-fields", "a1-2b"] {
        assert!(
            jjf_storage::validate_slug(ok).is_ok(),
            "expected slug {ok:?} to validate"
        );
    }
}

#[test]
fn validate_slug_rejects_each_failure_mode() {
    use SlugInvalidReason::*;
    // Each row pairs a bad slug with its expected rejection reason.
    // Length checks fire before charset checks, so a too-short slug
    // returns `TooShort` even if it ALSO contains an illegal char.
    let cases: &[(&str, SlugInvalidReason)] = &[
        ("ab", TooShort),
        ("", TooShort),
        (&"a".repeat(49), TooLong),
        ("Abc", BadCharset),
        ("a_b-c", BadCharset),
        ("a/b", BadCharset),
        ("a b", BadCharset),
        ("-abc", LeadingHyphen),
        ("abc-", TrailingHyphen),
        ("a--b", ConsecutiveHyphens),
    ];
    for (slug, expected) in cases {
        match jjf_storage::validate_slug(slug) {
            Err(got) => assert_eq!(
                got, *expected,
                "slug {slug:?}: expected {expected:?}, got {got:?}"
            ),
            Ok(()) => panic!("slug {slug:?} should have been rejected with {expected:?}"),
        }
    }
}

#[test]
fn create_issue_with_type_and_slug_round_trips() {
    let repo = make_scratch_repo("create_with_type_and_slug");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "agent-ready ticket".into(),
            type_: Some(IssueType::Feature),
            slug: Some("agent-ready".into()),
            ..Default::default()
        })
        .unwrap();

    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.type_, IssueType::Feature);
    assert_eq!(issue.slug.as_deref(), Some("agent-ready"));

    // The create commit's trailers must include set-type AND set-slug
    // (spec v2.1 §5.7).
    let history = storage.read_history(&id).unwrap();
    assert!(
        history.iter().any(|h| matches!(&h.op, Op::SetType { kind, .. } if *kind == IssueType::Feature)),
        "history must include set-type op: {:#?}",
        history
    );
    assert!(
        history.iter().any(|h| matches!(&h.op, Op::SetSlug { slug, .. } if slug.as_deref() == Some("agent-ready"))),
        "history must include set-slug op: {:#?}",
        history
    );
}

#[test]
fn create_issue_invalid_slug_is_rejected() {
    let repo = make_scratch_repo("create_invalid_slug");
    let storage = Storage::open(&repo).unwrap();
    let err = storage
        .create_issue(&IssueDraft {
            title: "bad slug".into(),
            slug: Some("Bad_Slug".into()),
            ..Default::default()
        })
        .unwrap_err();
    match err {
        StorageError::InvalidSlug { slug, reason } => {
            assert_eq!(slug, "Bad_Slug");
            assert_eq!(reason, SlugInvalidReason::BadCharset);
        }
        other => panic!("expected InvalidSlug, got {other:?}"),
    }
}

#[test]
fn slug_collision_detected_among_open_issues() {
    let repo = make_scratch_repo("slug_collision_open");
    let storage = Storage::open(&repo).unwrap();
    let first = storage
        .create_issue(&IssueDraft {
            title: "first".into(),
            slug: Some("the-slug".into()),
            ..Default::default()
        })
        .unwrap();
    let err = storage
        .create_issue(&IssueDraft {
            title: "second".into(),
            slug: Some("the-slug".into()),
            ..Default::default()
        })
        .unwrap_err();
    match err {
        StorageError::SlugCollision { slug, conflicts_with } => {
            assert_eq!(slug, "the-slug");
            assert_eq!(conflicts_with, first);
        }
        other => panic!("expected SlugCollision, got {other:?}"),
    }
}

#[test]
fn slug_uniqueness_scope_is_open_only() {
    // Closed issues release their slug. Spec v2.1 §3.1.
    let repo = make_scratch_repo("slug_open_only");
    let storage = Storage::open(&repo).unwrap();
    let first = storage
        .create_issue(&IssueDraft {
            title: "first".into(),
            slug: Some("the-slug".into()),
            ..Default::default()
        })
        .unwrap();
    storage.set_status(&first, Status::Closed).unwrap();
    // Now the slug is free — a second open issue can take it.
    let second = storage
        .create_issue(&IssueDraft {
            title: "second".into(),
            slug: Some("the-slug".into()),
            ..Default::default()
        })
        .unwrap();
    assert_ne!(first, second);
    let issue = storage.read(&second).unwrap();
    assert_eq!(issue.slug.as_deref(), Some("the-slug"));
}

#[test]
fn update_lands_set_type_and_set_slug_trailers_in_one_commit() {
    let repo = make_scratch_repo("update_type_and_slug");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "baseline".into(),
            ..Default::default()
        })
        .unwrap();
    let baseline = storage.read_history(&id).unwrap().len();
    storage
        .update(
            &id,
            UpdateFields {
                type_: Some(Some(IssueType::Bug)),
                slug: Some(Some("baseline-slug".into())),
                ..Default::default()
            },
        )
        .unwrap();
    let history = storage.read_history(&id).unwrap();
    let new = &history[baseline..];
    assert_eq!(new.len(), 2, "expected exactly two new ops, got {new:#?}");
    // Both new entries share one commit (single multi-op update).
    let commit = &new[0].commit;
    assert!(new.iter().all(|e| &e.commit == commit));
    // Field-declaration order: slug before type.
    match &new[0].op {
        Op::SetSlug { slug, .. } => assert_eq!(slug.as_deref(), Some("baseline-slug")),
        other => panic!("new[0] expected SetSlug, got {other:?}"),
    }
    match &new[1].op {
        Op::SetType { kind, .. } => assert_eq!(*kind, IssueType::Bug),
        other => panic!("new[1] expected SetType, got {other:?}"),
    }
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.type_, IssueType::Bug);
    assert_eq!(issue.slug.as_deref(), Some("baseline-slug"));
}

#[test]
fn update_unset_slug_clears_field() {
    let repo = make_scratch_repo("update_unset_slug");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "has slug".into(),
            slug: Some("kept".into()),
            ..Default::default()
        })
        .unwrap();
    storage
        .update(
            &id,
            UpdateFields {
                slug: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.slug, None);
}

#[test]
fn resolve_accepts_id_and_slug() {
    let repo = make_scratch_repo("resolve_id_and_slug");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "resolvable".into(),
            slug: Some("resolvable-slug".into()),
            ..Default::default()
        })
        .unwrap();
    // Id path.
    assert_eq!(storage.resolve(id.as_str()).unwrap(), id);
    // Slug path.
    assert_eq!(storage.resolve("resolvable-slug").unwrap(), id);
    // Unknown handle.
    let err = storage.resolve("no-such-handle").unwrap_err();
    match err {
        StorageError::SlugNotFound { handle } => {
            assert_eq!(handle, "no-such-handle");
        }
        other => panic!("expected SlugNotFound, got {other:?}"),
    }
}

#[test]
fn op_set_type_and_set_slug_round_trip_serde() {
    let issue_id = IssueId::parse("aa6600b").unwrap();
    let set_type = Op::SetType {
        issue_id: issue_id.clone(),
        kind: IssueType::Epic,
    };
    let set_slug = Op::SetSlug {
        issue_id: issue_id.clone(),
        slug: Some("epic-slug".into()),
    };
    for op in [&set_type, &set_slug] {
        let s = serde_json::to_string(op).unwrap();
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(&back, op);
    }
    // Trailer rendering shape spot-check.
    let block = set_type.to_trailer_block("2026-06-22T12:34:56.000000000Z");
    assert!(block.contains("Jjf-Op: set-type"));
    assert!(block.contains("Jjf-Type: epic"));
    let block = set_slug.to_trailer_block("2026-06-22T12:34:56.000000000Z");
    assert!(block.contains("Jjf-Op: set-slug"));
    assert!(block.contains("Jjf-Slug: epic-slug"));
}

// ---------------------------------------------------------------------
// `Storage::list_ready` tests (issue 69d5e1b).
//
// The ticket calls out:
//   - 3 issues, no deps → all returned, sorted oldest first (FIFO
//     within equal priority).
//   - 3 issues, B depends on A (open) → A and C returned, B blocked.
//   - 3 issues, B depends on A (closed) → all three returned.
//   - Label filter intersection.
//   - Limit clamps the returned vec.
// Plus a few additional pins for the v2.1 type-priority sort and
// roadmap exclusion since those are load-bearing for the agent loop.
// ---------------------------------------------------------------------

#[test]
fn list_ready_three_open_no_deps_returns_all_fifo() {
    let repo = make_scratch_repo("ready_three_no_deps");
    let storage = Storage::open(&repo).unwrap();
    // Three issues of the same type so the secondary key (FIFO by
    // created_at) is the only differentiator.
    let mut ids: Vec<IssueId> = Vec::new();
    for title in ["first", "second", "third"] {
        let id = storage
            .create_issue(&IssueDraft {
                title: (*title).into(),
                type_: Some(IssueType::Feature),
                ..Default::default()
            })
            .unwrap();
        ids.push(id);
        // Sleep one second between creates so the second-resolution
        // `created_at` timestamps differ. The storage layer's
        // `now_rfc3339` is second-precision; without the delay the
        // three issues can share a timestamp and the order is
        // ambiguous.
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }

    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    assert_eq!(ready.len(), 3, "expected all 3 issues ready, got {ready:#?}");
    // FIFO: insertion order matches the result order.
    assert_eq!(ready[0].id, ids[0], "oldest first");
    assert_eq!(ready[1].id, ids[1]);
    assert_eq!(ready[2].id, ids[2], "newest last");
}

#[test]
fn list_ready_open_dependency_blocks_the_dependent_issue() {
    let repo = make_scratch_repo("ready_open_dep_blocks");
    let storage = Storage::open(&repo).unwrap();
    // A is open. B depends on A. C is independent.
    let a = storage
        .create_issue(&IssueDraft {
            title: "A — blocker".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    let b = storage
        .create_issue(&IssueDraft {
            title: "B — blocked".into(),
            type_: Some(IssueType::Feature),
            dependencies: vec![a.clone()],
            ..Default::default()
        })
        .unwrap();
    let c = storage
        .create_issue(&IssueDraft {
            title: "C — independent".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();

    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    let ids: Vec<&IssueId> = ready.iter().map(|i| &i.id).collect();
    assert!(ids.contains(&&a), "A is open with no deps → ready");
    assert!(!ids.contains(&&b), "B depends on open A → blocked");
    assert!(ids.contains(&&c), "C is independent → ready");
    assert_eq!(ready.len(), 2, "exactly A and C: {ready:#?}");
}

#[test]
fn list_ready_closed_dependency_does_not_block() {
    let repo = make_scratch_repo("ready_closed_dep_unblocks");
    let storage = Storage::open(&repo).unwrap();
    // A is closed. B depends on A. C is independent.
    let a = storage
        .create_issue(&IssueDraft {
            title: "A — done".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    let b = storage
        .create_issue(&IssueDraft {
            title: "B — was blocked".into(),
            type_: Some(IssueType::Feature),
            dependencies: vec![a.clone()],
            ..Default::default()
        })
        .unwrap();
    let c = storage
        .create_issue(&IssueDraft {
            title: "C — independent".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    storage.set_status(&a, Status::Closed).unwrap();

    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    let ids: Vec<&IssueId> = ready.iter().map(|i| &i.id).collect();
    // A is closed → not in the OPEN ready set.
    assert!(!ids.contains(&&a), "A is closed → excluded from ready");
    // B's only dep (A) is closed → B is ready.
    assert!(ids.contains(&&b), "B's dep A is closed → B ready");
    assert!(ids.contains(&&c), "C independent → ready");
    assert_eq!(ready.len(), 2, "exactly B and C: {ready:#?}");
}

#[test]
fn list_ready_type_priority_orders_bug_before_feature_before_epic() {
    let repo = make_scratch_repo("ready_type_priority");
    let storage = Storage::open(&repo).unwrap();
    // File in a deliberately scrambled order so the sort is doing
    // real work. Created order: epic, bug, feature, research,
    // unspecified.
    let epic = storage
        .create_issue(&IssueDraft {
            title: "epic ticket".into(),
            type_: Some(IssueType::Epic),
            ..Default::default()
        })
        .unwrap();
    let bug = storage
        .create_issue(&IssueDraft {
            title: "bug ticket".into(),
            type_: Some(IssueType::Bug),
            ..Default::default()
        })
        .unwrap();
    let feature = storage
        .create_issue(&IssueDraft {
            title: "feature ticket".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    let research = storage
        .create_issue(&IssueDraft {
            title: "research ticket".into(),
            type_: Some(IssueType::Research),
            ..Default::default()
        })
        .unwrap();
    let unspec = storage
        .create_issue(&IssueDraft {
            title: "unspecified ticket".into(),
            // Default type_ = Unspecified.
            ..Default::default()
        })
        .unwrap();

    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    assert_eq!(ready.len(), 5);
    // Expected: bug > feature > research > epic > unspecified.
    assert_eq!(ready[0].id, bug, "bug first: {ready:#?}");
    assert_eq!(ready[1].id, feature);
    assert_eq!(ready[2].id, research);
    assert_eq!(ready[3].id, epic);
    assert_eq!(ready[4].id, unspec);
}

#[test]
fn list_ready_excludes_roadmap_type_entirely() {
    // The roadmap ticket isn't work to do — it's the planning
    // surface itself. Spec: never appears in `ready`.
    let repo = make_scratch_repo("ready_excludes_roadmap");
    let storage = Storage::open(&repo).unwrap();
    let _roadmap = storage
        .create_issue(&IssueDraft {
            title: "the roadmap".into(),
            type_: Some(IssueType::Roadmap),
            ..Default::default()
        })
        .unwrap();
    let bug = storage
        .create_issue(&IssueDraft {
            title: "a bug".into(),
            type_: Some(IssueType::Bug),
            ..Default::default()
        })
        .unwrap();

    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    assert_eq!(ready.len(), 1, "only the bug; roadmap excluded: {ready:#?}");
    assert_eq!(ready[0].id, bug);
}

#[test]
fn list_ready_label_intersection_filter() {
    let repo = make_scratch_repo("ready_label_filter");
    let storage = Storage::open(&repo).unwrap();
    let only_x = storage
        .create_issue(&IssueDraft {
            title: "only-x".into(),
            type_: Some(IssueType::Feature),
            labels: vec!["x".into()],
            ..Default::default()
        })
        .unwrap();
    let _only_y = storage
        .create_issue(&IssueDraft {
            title: "only-y".into(),
            type_: Some(IssueType::Feature),
            labels: vec!["y".into()],
            ..Default::default()
        })
        .unwrap();
    let both_xy = storage
        .create_issue(&IssueDraft {
            title: "both-xy".into(),
            type_: Some(IssueType::Feature),
            labels: vec!["x".into(), "y".into()],
            ..Default::default()
        })
        .unwrap();

    // --label x → only-x AND both-xy (2).
    let ready = storage
        .list_ready(&ReadyFilter {
            labels: vec!["x".into()],
            ..Default::default()
        })
        .unwrap();
    let ids: Vec<&IssueId> = ready.iter().map(|i| &i.id).collect();
    assert_eq!(ready.len(), 2);
    assert!(ids.contains(&&only_x));
    assert!(ids.contains(&&both_xy));

    // --label x --label y → both-xy only (intersection AND).
    let ready = storage
        .list_ready(&ReadyFilter {
            labels: vec!["x".into(), "y".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, both_xy);
}

#[test]
fn list_ready_type_filter_or_semantics() {
    let repo = make_scratch_repo("ready_type_filter");
    let storage = Storage::open(&repo).unwrap();
    let bug = storage
        .create_issue(&IssueDraft {
            title: "bug ticket".into(),
            type_: Some(IssueType::Bug),
            ..Default::default()
        })
        .unwrap();
    let feature = storage
        .create_issue(&IssueDraft {
            title: "feature ticket".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    let _epic = storage
        .create_issue(&IssueDraft {
            title: "epic ticket".into(),
            type_: Some(IssueType::Epic),
            ..Default::default()
        })
        .unwrap();

    // --type bug --type feature → exactly those two.
    let ready = storage
        .list_ready(&ReadyFilter {
            types: vec![IssueType::Bug, IssueType::Feature],
            ..Default::default()
        })
        .unwrap();
    let ids: Vec<&IssueId> = ready.iter().map(|i| &i.id).collect();
    assert_eq!(ready.len(), 2);
    assert!(ids.contains(&&bug));
    assert!(ids.contains(&&feature));
}

#[test]
fn list_ready_limit_clamps_after_sort() {
    let repo = make_scratch_repo("ready_limit_clamp");
    let storage = Storage::open(&repo).unwrap();
    // Five features in insertion order. Limit 2 should return the
    // two highest-priority = oldest-by-FIFO entries.
    let mut ids: Vec<IssueId> = Vec::new();
    for title in ["a", "b", "c", "d", "e"] {
        ids.push(
            storage
                .create_issue(&IssueDraft {
                    title: (*title).into(),
                    type_: Some(IssueType::Feature),
                    ..Default::default()
                })
                .unwrap(),
        );
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }

    let ready = storage
        .list_ready(&ReadyFilter {
            limit: Some(2),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(ready.len(), 2, "limit 2 should clamp: {ready:#?}");
    assert_eq!(ready[0].id, ids[0], "oldest first");
    assert_eq!(ready[1].id, ids[1]);
}

#[test]
fn list_ready_dangling_dependency_does_not_block() {
    // A dangling dep id (one that doesn't exist on the bookmark)
    // shouldn't wedge progress — a deleted/mistyped dep would
    // otherwise lock the issue out of `ready` forever. Closed-or-
    // dangling both pass; only open-and-extant blocks.
    let repo = make_scratch_repo("ready_dangling_dep");
    let storage = Storage::open(&repo).unwrap();
    let phantom = IssueId::parse("deadbee").unwrap();
    let issue = storage
        .create_issue(&IssueDraft {
            title: "depends on a ghost".into(),
            type_: Some(IssueType::Bug),
            dependencies: vec![phantom],
            ..Default::default()
        })
        .unwrap();

    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, issue);
}

#[test]
fn list_ready_on_empty_bookmark_returns_empty() {
    let repo = make_scratch_repo("ready_empty");
    let storage = Storage::open(&repo).unwrap();
    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    assert!(ready.is_empty(), "empty bookmark → empty ready: {ready:#?}");
}
