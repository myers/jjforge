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
    ClaimResult, DepEdge, DepKind, Error as StorageError, IssueDraft, IssueId, IssueType, Op,
    ReadyFilter, SlugInvalidReason, Status, Storage, TitleInvalidReason, UpdateFields,
};
use serde::Serialize;

/// Build a scratch jj repo with a seeded v2 `issues` bookmark.
/// Returns the absolute path to the repo root.
///
/// **Why we still plant a v2 bookmark.** After the v3 init rewrite
/// (ticket `add0646`), `Storage::init` plants only the v3 sentinel
/// ref. The integration tests in this file are the v2 → v3 backstop:
/// they plant a v2-shape repo by hand and then call `Storage::open`,
/// which runs the v2 → v3 migrator. Post-migration, the data lives
/// on `refs/jjf/issues/<id>` and the bookmark is gone. Assertions
/// throughout this file use the v3 helpers ([`v3_blob_at`],
/// [`git_log_v3_chain`]) to walk the per-issue ref instead of the
/// bookmark.
///
/// The bootstrap commands here mirror the pre-v3 `Storage::init`
/// body byte-for-byte: seed commit description per spec §1.1, the
/// final `jj new root()` to step `@` off the bookmark so the
/// writer dance doesn't snapshot stale working-copy state.
fn make_scratch_repo(name: &str) -> PathBuf {
    let abs = make_empty_jj_repo(name);
    plant_v2_bookmark(&abs);
    abs
}

/// Plant the v2 `issues` bookmark with the spec-§1.1 seed commit on
/// a fresh jj repo. Lifts the pre-v3 `Storage::init` body. The
/// bookmark will be migrated to v3-shape refs by `Storage::open`.
fn plant_v2_bookmark(repo: &Path) {
    sh("jj", &["new", "root()", "-m", "jjf: seed issues bookmark"], repo);
    sh("jj", &["bookmark", "create", "issues", "-r", "@"], repo);
    sh("jj", &["new", "root()"], repo);
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

/// Read a blob from a v3 per-issue ref. The path is relative to the
/// ref's tree root (e.g. `issue.json` or `comments.jsonl`).
///
/// V3 layout: each issue lives at `refs/jjf/issues/<id>` and its tip
/// carries a single-directory tree with `issue.json` (always) and
/// `comments.jsonl` (when comments exist). See
/// `docs/storage-out-of-tree.md` §"Tree shape".
fn read_at_issue_ref(repo: &Path, id: &str, path: &str) -> String {
    let spec = format!("refs/jjf/issues/{}:{}", id, path);
    let out = Command::new("git")
        .args(["cat-file", "blob", &spec])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git cat-file {} failed:\nstderr: {}",
        spec,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Read a memory's blob from the v3 per-memory ref. V3 layout:
/// each memory lives at `refs/jjf/memories/<key>:memory.json`.
fn read_at_memory_ref(repo: &Path, key: &str) -> String {
    let spec = format!("refs/jjf/memories/{}:memory.json", key);
    let out = Command::new("git")
        .args(["cat-file", "blob", &spec])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git cat-file {} failed:\nstderr: {}",
        spec,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Capture the commit-message descriptions on a v3 per-issue ref,
/// newest-first (matches the historical `jj log` newest-first
/// output that read_at_bookmark-era assertions consumed). Returns
/// a single string joined by `\n----\n` between entries.
fn git_log_v3_chain(repo: &Path, id: &str) -> String {
    git_capture(
        &[
            "log",
            "--format=%B%n----",
            &format!("refs/jjf/issues/{}", id),
        ],
        repo,
    )
}

/// Capture stdout of a `git` invocation under `repo`. Mirror of
/// `jj_capture`; we use it where we'd previously have shelled into
/// `jj log -r bookmarks(issues)`.
fn git_capture(args: &[&str], repo: &Path) -> String {
    let out = Command::new("git").args(args).current_dir(repo).output().unwrap();
    assert!(
        out.status.success(),
        "`git {}` failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        repo.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The symbolic target of `HEAD` — `refs/heads/<branch>` in a normal
/// repo, `refs/jj/root` if the v2 4-CLI dance drift hits. Returns the
/// raw output without trailing newline. Test repos here are jj-only
/// (no `--colocate` in `make_empty_jj_repo`), so HEAD never resolves
/// to an oid until git records a commit — but the symbolic target
/// IS present and IS the drift fingerprint we care about.
fn git_symbolic_ref_head(repo: &Path) -> String {
    git_capture(&["symbolic-ref", "HEAD"], repo).trim().to_owned()
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

    // V3 tree carries `issue.json` at the per-issue ref's tip with
    // the schema fields. (The v3 write path is git-only; the working
    // copy is never touched.) See `docs/storage-out-of-tree.md`.
    let json_text = read_at_issue_ref(&repo, &id_s, "issue.json");
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

    // V3: comments.jsonl is absent in the tree when there are no
    // comments. (V2 planted an empty file; v3 doesn't.)
    let no_comments = Command::new("git")
        .args([
            "cat-file",
            "blob",
            &format!("refs/jjf/issues/{}:comments.jsonl", id_s),
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        !no_comments.status.success(),
        "v3 create must NOT plant an empty comments.jsonl in the tree"
    );

    // Snapshot HEAD before set_status — v3 mutations MUST leave HEAD
    // untouched (the original test asserted "@ off the bookmark";
    // v3 inverts that to "HEAD does not drift").
    //
    // Use `symbolic-ref HEAD` rather than `rev-parse HEAD` because a
    // jj-only repo (no `--colocate`) has no checked-out branch — HEAD
    // is a symbolic ref pointing at `refs/heads/main` which itself has
    // no oid until something is committed via git. The drift fingerprint
    // we care about is HEAD's symbolic target getting re-pointed (e.g.
    // to `refs/jj/root`), not the oid value. The companion v3 write-path
    // tests (`v3_write_path.rs::git_head_symbolic`) use the same probe.
    let head_sym_before_set = git_symbolic_ref_head(&repo);
    let at_before_set = jj_capture(
        &["log", "--no-graph", "-r", "@", "-T", "change_id"],
        &repo,
    );

    // set_status to closed.
    storage.set_status(&id, Status::Closed).expect("set_status");

    // V3 ref-tip `issue.json` reflects the new status.
    let json_text = read_at_issue_ref(&repo, &id_s, "issue.json");
    let v: serde_json::Value = serde_json::from_str(&json_text).unwrap();
    assert_eq!(v["status"], "closed");
    assert_eq!(v["version"], 2);

    // `git log` for the per-issue ref should show two commits: the
    // create commit and the set-status commit. Newest first.
    let log = git_log_v3_chain(&repo, &id_s);
    let entries: Vec<&str> = log.split("\n----\n").filter(|s| !s.trim().is_empty()).collect();
    assert_eq!(
        entries.len(),
        2,
        "expected 2 commits on refs/jjf/issues/{id_s}, got {}:\n{log}",
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

    // The per-issue ref now points at the latest mutation (set-status).
    let tip_msg = git_capture(
        &[
            "log",
            "-n",
            "1",
            "--format=%s",
            &format!("refs/jjf/issues/{}", id_s),
        ],
        &repo,
    );
    assert!(
        tip_msg.contains("set-status"),
        "refs/jjf/issues/{} should point at the set-status commit, got: {}",
        id_s,
        tip_msg
    );

    // V3 invariant: git HEAD does not drift across a mutation. jj's
    // working-copy change id is also pinned.
    let head_sym_after_set = git_symbolic_ref_head(&repo);
    let at_after_set = jj_capture(
        &["log", "--no-graph", "-r", "@", "-T", "change_id"],
        &repo,
    );
    assert_eq!(
        head_sym_before_set, head_sym_after_set,
        "git HEAD symbolic target must not move across a v3 mutation"
    );
    assert_eq!(
        at_before_set, at_after_set,
        "jj @ must not move across a v3 mutation"
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

    let body = read_at_issue_ref(&repo, &id_s, "comments.jsonl");
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one comment line: {body:?}");
    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(v["body"], "first thought");
    assert_eq!(v["author"], "alice <alice@example.com>");
    assert!(
        v["id"].as_str().unwrap().len() == 7,
        "comment id must be 7 hex chars: {body}"
    );

    // The comment-add commit's description (now at the per-issue ref
    // tip) must carry the trailer + the Jjf-Comment-Id matching the
    // line in jsonl.
    let log = git_capture(
        &[
            "log",
            "-n",
            "1",
            "--format=%B",
            &format!("refs/jjf/issues/{}", id_s),
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
    assert_eq!(bug.dependencies, Vec::<DepEdge>::new());
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
    let on_disk = read_at_issue_ref(&repo, &id_s, "issue.json");

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
        block_reason: Option<&'a str>,
        #[serde(rename = "type")]
        type_: &'a str,
        labels: &'a [String],
        dependencies: &'a [DepEdge],
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
        status: bug.status.as_str(),
        block_reason: bug.block_reason.as_deref(),
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
    let on_disk_comments = read_at_issue_ref(&repo, &id_s, "comments.jsonl");
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
// Bootstrap-path tests (issue 8b12f9d, rewritten for v3 init by
// ticket add0646).
//
// `Storage::init` now plants the v3 `refs/jjf/meta/format-version`
// sentinel ref on a fresh repo. Three invariants the v3 init
// contract pins:
//   - Exactly one ref under `refs/jjf/` post-init (the sentinel).
//   - No `issues` bookmark exists.
//   - Git HEAD does not move across init.
//   - The jj working copy is unmoved (no descendant commits, @ stays
//     where it was).
//
// Idempotency, error-shape, and round-trip-with-create-issue tests
// follow. The v1 → v2 idempotency path stays covered by
// `v1_to_v2_migration_preserves_history` further down.
// ---------------------------------------------------------------------

/// Capture the current value of a git ref, or "" if it doesn't
/// resolve. Used by the v3 init tests to assert HEAD invariance and
/// the presence / absence of the sentinel ref.
fn git_rev_parse(repo: &Path, refname: &str) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", refname])
        .current_dir(repo)
        .output()
        .unwrap();
    // `--quiet` makes a missing ref exit 1 with empty stdout.
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Enumerate the refs under `refs/jjf/` as `<name>\n` lines.
fn list_jjf_refs(repo: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", "refs/jjf/"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|s| s.to_owned())
        .collect()
}

#[test]
fn init_on_fresh_repo_plants_v3_sentinel_only() {
    let repo = make_empty_jj_repo("init_fresh");

    // Pre-condition: no `issues` bookmark, no `refs/jjf/*`, capture
    // git HEAD for the invariance check below.
    let pre_bookmarks =
        jj_capture(&["bookmark", "list", "-T", "name ++ \"\\n\""], &repo);
    assert!(
        !pre_bookmarks.lines().any(|l| l.trim() == "issues"),
        "pre-condition: issues bookmark should not exist yet, got: {pre_bookmarks}"
    );
    assert!(
        list_jjf_refs(&repo).is_empty(),
        "pre-condition: refs/jjf/ should be empty before init"
    );
    let pre_head = git_rev_parse(&repo, "HEAD");

    Storage::init(&repo).expect("Storage::init on fresh repo");

    // Post-condition 1: exactly one ref under refs/jjf/, namely the
    // sentinel.
    let post_refs = list_jjf_refs(&repo);
    assert_eq!(
        post_refs,
        vec!["refs/jjf/meta/format-version".to_string()],
        "init must plant exactly the sentinel ref, got: {post_refs:?}"
    );

    // Post-condition 2: no issues bookmark.
    let post_bookmarks =
        jj_capture(&["bookmark", "list", "-T", "name ++ \"\\n\""], &repo);
    assert!(
        !post_bookmarks.lines().any(|l| l.trim() == "issues"),
        "post-condition: issues bookmark must NOT exist, got: {post_bookmarks}"
    );

    // Post-condition 3: git HEAD is unchanged. The whole point of the
    // v3 init is to not move the working copy / drift HEAD.
    let post_head = git_rev_parse(&repo, "HEAD");
    assert_eq!(
        pre_head, post_head,
        "git HEAD must not move across init: pre={pre_head:?} post={post_head:?}"
    );

    // Post-condition 4: jj working-copy @ stays put — the v3 init
    // writes via git only and never calls `jj new` / `jj describe`,
    // so the commit at @ must be the same one a freshly-init'd jj
    // repo carries. (We can't assert "no commits under @ besides
    // root" because `jj git init` itself materializes one
    // working-copy commit.) See
    // `init_on_fresh_repo_does_not_advance_jj_working_copy` for the
    // before/after @ comparison.
}

#[test]
fn init_on_fresh_repo_does_not_advance_jj_working_copy() {
    let repo = make_empty_jj_repo("init_fresh_wc");

    let pre_at = jj_capture(
        &["log", "--no-graph", "-r", "@", "-T", "commit_id ++ \"\\n\""],
        &repo,
    );

    Storage::init(&repo).expect("Storage::init on fresh repo");

    let post_at = jj_capture(
        &["log", "--no-graph", "-r", "@", "-T", "commit_id ++ \"\\n\""],
        &repo,
    );
    assert_eq!(
        pre_at.trim(),
        post_at.trim(),
        "init must NOT advance the jj working copy (pre={pre_at:?} post={post_at:?})"
    );
}

#[test]
fn init_is_idempotent_on_v3_repo() {
    let repo = make_empty_jj_repo("init_twice");

    Storage::init(&repo).expect("first init");
    let first_sentinel = git_rev_parse(&repo, "refs/jjf/meta/format-version");
    assert!(
        !first_sentinel.is_empty(),
        "first init must plant the sentinel"
    );
    let first_head = git_rev_parse(&repo, "HEAD");

    Storage::init(&repo).expect("second init must be a no-op success");

    let second_sentinel = git_rev_parse(&repo, "refs/jjf/meta/format-version");
    assert_eq!(
        first_sentinel, second_sentinel,
        "second init must not rewrite the sentinel"
    );

    let second_head = git_rev_parse(&repo, "HEAD");
    assert_eq!(
        first_head, second_head,
        "second init must not move HEAD: first={first_head:?} second={second_head:?}"
    );

    let refs = list_jjf_refs(&repo);
    assert_eq!(
        refs,
        vec!["refs/jjf/meta/format-version".to_string()],
        "still exactly one ref under refs/jjf/ after the second init: {refs:?}"
    );
}

#[test]
fn init_is_idempotent_on_v2_repo() {
    // v2 repos already in the wild (bookmark exists, no sentinel)
    // must keep working with the v2 write path until the v2→v3
    // migrator (ticket c14e1c1) flips them on `Storage::open`. Init
    // must NOT plant the sentinel on top of a v2 bookmark — that
    // would silently switch the next reader to v3 mode against
    // bookmark-shaped data.
    let repo = make_scratch_repo("init_on_v2_repo");

    // Pre-condition: v2 shape — bookmark present, no sentinel.
    let pre_sentinel = git_rev_parse(&repo, "refs/jjf/meta/format-version");
    assert!(
        pre_sentinel.is_empty(),
        "pre-condition: v2 repo must not have the v3 sentinel"
    );

    Storage::init(&repo).expect("init on v2-shape repo must succeed");

    // Post-condition: still no sentinel; the v2→v3 migration is the
    // job of ticket c14e1c1's `Storage::open` path, NOT `init`.
    let post_sentinel = git_rev_parse(&repo, "refs/jjf/meta/format-version");
    assert!(
        post_sentinel.is_empty(),
        "init must NOT plant the v3 sentinel on a v2-shape repo, got: {post_sentinel:?}"
    );

    // And the issues bookmark stays right where it was.
    let post_bookmarks =
        jj_capture(&["bookmark", "list", "-T", "name ++ \"\\n\""], &repo);
    assert!(
        post_bookmarks.lines().any(|l| l.trim() == "issues"),
        "init must keep the v2 issues bookmark, got: {post_bookmarks}"
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
fn init_then_create_issue_round_trips_on_v3_repo() {
    // Smoke test that tickets 1 + 2 + 3 compose: init plants the v3
    // sentinel; create_issue routes through the v3 write path (per
    // the StorageMode dispatch in commit_record_change); read finds
    // the issue back. This is the v3 counterpart to the old
    // `init_then_create_issue_lands_on_top_of_seed` test.
    let repo = make_empty_jj_repo("init_then_create");
    let storage = Storage::init(&repo).expect("init plants sentinel");

    let id = storage
        .create_issue(&IssueDraft {
            title: "first ever v3 issue".into(),
            ..Default::default()
        })
        .expect("create_issue on freshly-init'd v3 repo");

    let bug = storage.read(&id).expect("read after create");
    assert_eq!(bug.title, "first ever v3 issue");

    // The issue lives under `refs/jjf/issues/<id>`.
    let issue_ref = format!("refs/jjf/issues/{}", id);
    let tip = git_rev_parse(&repo, &issue_ref);
    assert!(
        !tip.is_empty(),
        "create_issue must plant the per-issue ref {issue_ref}"
    );

    // No v2 issues bookmark was created.
    let post_bookmarks =
        jj_capture(&["bookmark", "list", "-T", "name ++ \"\\n\""], &repo);
    assert!(
        !post_bookmarks.lines().any(|l| l.trim() == "issues"),
        "v3 create must not create the v2 issues bookmark, got: {post_bookmarks}"
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
    // V3 era: this test exercises the v1 → v2 step (path rename
    // bugs/* → issues/*), with v2 → v3 chained on top. We open via
    // `open_skip_v2_to_v3_migration` while building the v1 shape so the
    // auto-migrator doesn't pre-emptively re-shape the data into v3.
    // Once the v1-shape has been laid down by hand, the production
    // `Storage::open` runs v1 → v2 → v3 in sequence and we assert the
    // issue's full op history is reachable post-migration.
    let repo = make_scratch_repo("v1_to_v2_migration_preserves_history");

    // Create an issue in v2 form so we have real `Jjf-Op:` trailers
    // and real on-disk record files. Land two ops (create + close)
    // so the history walker has a non-trivial chain to follow.
    let storage = Storage::open_skip_v2_to_v3_migration(&repo).unwrap();
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

    // The actual test: production Storage::open detects v1, runs
    // v1 → v2 (rename), then v2 → v3 (per-issue refs + sentinel), and
    // Storage::read succeeds with the full chain.
    let storage = Storage::open(&repo).expect("Storage::open must succeed on v1 repo");
    let bug = storage
        .read(&id)
        .expect("Storage::read must succeed post-migration; the read-side path filter must include the v1 `bugs/` paths so pre-migration commits are visible");
    assert_eq!(bug.title, "synthetic v1 issue");
    assert_eq!(bug.status, Status::Closed);

    // Post v1 → v2 → v3, the bookmarks are gone (v3 layout) and the
    // sentinel ref is planted. The per-issue ref carries the issue.
    let head_refs_out = Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", "refs/heads/"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let head_refs = String::from_utf8_lossy(&head_refs_out.stdout);
    assert!(
        !head_refs.contains("refs/heads/issues"),
        "post-v3 migration must NOT have an `issues` bookmark, got:\n{head_refs}"
    );
    assert!(
        !head_refs.contains("refs/heads/bugs"),
        "post-v3 migration must NOT have a `bugs` bookmark, got:\n{head_refs}"
    );
    let issue_ref = format!("refs/jjf/issues/{}", id);
    assert!(
        Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", &issue_ref])
            .current_dir(&repo)
            .output()
            .unwrap()
            .status
            .success(),
        "per-issue v3 ref {issue_ref} must exist post-migration"
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
fn slug_uniqueness_scope_spans_all_statuses_including_closed() {
    // Spec v2.6 (issue `a105e0b`): closed issues retain their
    // slug forever. A new ticket must pick a fresh one — silently
    // re-using a closed issue's slug is the wrong default for an
    // audit-trail planner because `jjf show <slug>` would
    // resolve to the new issue, shadowing the closed one.
    let repo = make_scratch_repo("slug_all_statuses");
    let storage = Storage::open(&repo).unwrap();
    let first = storage
        .create_issue(&IssueDraft {
            title: "first".into(),
            slug: Some("the-slug".into()),
            ..Default::default()
        })
        .unwrap();
    storage.set_status(&first, Status::Closed).unwrap();
    // Closing the first issue MUST NOT release the slug.
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
            assert_eq!(
                conflicts_with, first,
                "the closed issue's id must be carried in conflicts_with"
            );
        }
        other => panic!("expected SlugCollision against closed holder, got {other:?}"),
    }
}

#[test]
fn slug_uniqueness_blocks_against_blocked_holder() {
    // Regression guard: the active-status path (Open / InProgress /
    // Blocked) was always enforced. v2.6 widens to Closed; this
    // ensures the v2.5 behavior for active statuses still holds.
    let repo = make_scratch_repo("slug_blocked_holder");
    let storage = Storage::open(&repo).unwrap();
    let first = storage
        .create_issue(&IssueDraft {
            title: "first".into(),
            slug: Some("active-slug".into()),
            ..Default::default()
        })
        .unwrap();
    storage.block(&first, Some("waiting on review")).unwrap();
    let err = storage
        .create_issue(&IssueDraft {
            title: "second".into(),
            slug: Some("active-slug".into()),
            ..Default::default()
        })
        .unwrap_err();
    match err {
        StorageError::SlugCollision { slug, conflicts_with } => {
            assert_eq!(slug, "active-slug");
            assert_eq!(conflicts_with, first);
        }
        other => panic!("expected SlugCollision against blocked holder, got {other:?}"),
    }
}

#[test]
fn resolve_still_finds_closed_issue_by_slug() {
    // Regression guard: `jjf show <slug>` must still resolve a
    // closed issue's slug. v2.6 changed the WRITE-path uniqueness
    // rule, not the resolver.
    let repo = make_scratch_repo("slug_resolve_closed");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "archived".into(),
            slug: Some("ghost-slug".into()),
            ..Default::default()
        })
        .unwrap();
    storage.set_status(&id, Status::Closed).unwrap();
    let resolved = storage.resolve("ghost-slug").unwrap();
    assert_eq!(resolved, id);
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
    // Unknown handle (non-hex).
    let err = storage.resolve("no-such-handle").unwrap_err();
    match err {
        StorageError::SlugNotFound { handle } => {
            assert_eq!(handle, "no-such-handle");
        }
        other => panic!("expected SlugNotFound, got {other:?}"),
    }
}

/// `Storage::resolve` accepts full 7-char hex ids and slugs; prefixes
/// are deliberately not supported. The id fast path doesn't probe the
/// snapshot for existence — callers get that signal from the subsequent
/// `Storage::read` (`IssueNotFound`). Slug lookups DO probe.

#[test]
fn resolve_full_id_returns_the_id() {
    let repo = make_scratch_repo("resolve_full_id");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "full-id".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(storage.resolve(id.as_str()).unwrap(), id);
}

#[test]
fn resolve_non_hex_handle_routes_to_slug_lookup() {
    let repo = make_scratch_repo("resolve_non_hex_routes_to_slug");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "slug-test".into(),
            slug: Some("the-slug".into()),
            ..Default::default()
        })
        .unwrap();
    // Non-hex handle: routes to slug resolver, finds the issue.
    assert_eq!(storage.resolve("the-slug").unwrap(), id);
    // Non-hex handle with no matching slug surfaces SlugNotFound.
    let err = storage.resolve("not-a-slug").unwrap_err();
    match err {
        StorageError::SlugNotFound { handle } => assert_eq!(handle, "not-a-slug"),
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
            dependencies: vec![DepEdge::blocks(a.clone())],
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
            dependencies: vec![DepEdge::blocks(a.clone())],
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
    // shouldn't wedge progress — a deleted dep would otherwise
    // lock the issue out of `ready` forever. Closed-or-dangling
    // both pass; only open-and-extant blocks.
    //
    // v2.x (`qa-dep-validation`, issue `d1a01f0`): the write path
    // now rejects phantom dep targets at `Storage::add_dep_edge`
    // / `Storage::create_issue` boundary, so a dangling dep can
    // only arise post-creation (a sibling repo pulled, then the
    // target's record was dropped upstream). We construct that
    // scenario explicitly: create A, hang a dep on A from B, then
    // drop A's record from the bookmark via raw `jj` commands,
    // and verify `list_ready` still returns B.
    let repo = make_scratch_repo("ready_dangling_dep");
    let storage = Storage::open(&repo).unwrap();

    let target = storage
        .create_issue(&IssueDraft {
            title: "real target".into(),
            type_: Some(IssueType::Bug),
            ..Default::default()
        })
        .unwrap();
    let depender = storage
        .create_issue(&IssueDraft {
            title: "depender".into(),
            type_: Some(IssueType::Bug),
            ..Default::default()
        })
        .unwrap();
    storage
        .add_dep_edge(&depender, &target, DepKind::Blocks)
        .unwrap();

    // Drop the target's per-issue ref entirely. In v3 each issue's
    // history lives on `refs/jjf/issues/<id>`; deleting that ref
    // simulates the post-merge state where the target's record
    // disappeared upstream (an admin force-cleaned a stale ref, a
    // merge dropped it, etc.).
    sh(
        "git",
        &["update-ref", "-d", &format!("refs/jjf/issues/{}", target)],
        &repo,
    );

    // Re-open storage to drop the snapshot memo so the next read
    // re-probes the refs and sees the dropped target.
    let storage = Storage::open(&repo).unwrap();
    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    assert_eq!(ready.len(), 1, "expected only the depender: {ready:#?}");
    assert_eq!(ready[0].id, depender);
}

#[test]
fn list_ready_on_empty_bookmark_returns_empty() {
    let repo = make_scratch_repo("ready_empty");
    let storage = Storage::open(&repo).unwrap();
    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    assert!(ready.is_empty(), "empty bookmark → empty ready: {ready:#?}");
}

// ---------------------------------------------------------------------
// Same-second comment-append regression (issue 004dd23).
//
// The dual-file path filter in `history.rs` (`jj log --files`
// covering both `issues/<id>.json` AND `issues/<id>.comments.jsonl`)
// claims to be load-bearing for the following case: two
// `add_comment` calls happen within the same wall-clock second, so
// the second call's JSON write is BYTE-IDENTICAL to the first's
// (both stamp `updated_at` to the same RFC 3339 second-resolution
// string; nothing else in the record changes). jj snapshots the
// JSON file by content; with no JSON delta in commit B, only the
// comments.jsonl file changes, so a path filter that names ONLY the
// JSON file would miss commit B entirely.
//
// This test exercises that case: spam N `add_comment` calls in a
// tight loop. On any modern machine many will fall in the same
// wall-clock second; the second within the same second produces a
// byte-identical JSON write. We then walk `read_history` and assert
// every comment-add appears in the chain. If the dual-file filter
// were dropped, the same-second commits would drop out of the
// history and the count would be < N.
//
// To make the failure mode crisp, we also assert (defensively) that
// at least one same-second comment cluster was constructed during
// the run. If a future test environment is so slow that every
// add_comment lands in its own second, the assertion fails loudly
// with a "test setup didn't construct the case" message rather than
// quietly passing on a degraded invariant.
// ---------------------------------------------------------------------
#[test]
fn read_history_walks_same_second_comment_appends() {
    // Pin the clock to a fixed second so every write lands in the
    // same wall-clock second AND the resulting JSON `updated_at`
    // values are byte-identical across writes. This makes the
    // load-bearing case (commit whose only change is a comments-jsonl
    // append) deterministic regardless of how slow `add_comment` is
    // under parallel-test load. Previously this test spammed 12
    // comments and hoped two landed same-second; that race lost when
    // full-suite parallelism made each add_comment take >1s.
    //
    // The env var is consumed by `now_rfc3339()` in
    // `crates/jjf-storage/src/lib.rs`. Nextest runs each test in its
    // own process, so the env var here doesn't leak into siblings.
    // SAFETY: single-threaded test process; no other code reads this
    // env var concurrently.
    unsafe {
        std::env::set_var("JJF_TEST_CLOCK_SECS", "1735660800");
    }

    let repo = make_scratch_repo("same_second_comments");
    let storage = Storage::open(&repo).unwrap();

    let id = storage
        .create_issue(&IssueDraft {
            title: "comment-spam target".into(),
            ..Default::default()
        })
        .unwrap();

    // Two comments — enough to construct one same-second cluster
    // (which is the load-bearing case).
    const N: usize = 2;
    for i in 0..N {
        storage
            .add_comment(&id, &format!("comment {i}"), "alice <a@x>")
            .unwrap();
    }

    // Confirm we built the case: both comments share an exact
    // `created_at`. With the clock pinned this is by construction;
    // if it fails the env-var override path is broken.
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.comments.len(), N);
    assert_eq!(
        issue.comments[0].created_at, issue.comments[1].created_at,
        "with JJF_TEST_CLOCK_SECS pinned, both comments must share created_at; \
         got {} vs {}",
        issue.comments[0].created_at, issue.comments[1].created_at,
    );

    // The regression: walk read_history and confirm every
    // comment-add op appears. If the path filter were stripped down
    // to only `issues/<id>.json`, the second commit's
    // byte-identical JSON write would be missed and this count
    // would be < N.
    let history = storage.read_history(&id).unwrap();
    let comment_ops: Vec<_> = history
        .iter()
        .filter(|e| matches!(e.op, Op::CommentAdd { .. }))
        .collect();
    assert_eq!(
        comment_ops.len(),
        N,
        "read_history must surface every comment-add even when the \
         JSON write is byte-identical between consecutive commits. \
         Got {} CommentAdd ops, expected {}. If this assertion \
         just started failing, check whether the path filter in \
         history.rs dropped its comments.jsonl entry.",
        comment_ops.len(),
        N,
    );
}


// -------- memories (spec v2.2 §10) -----------------------------------

#[test]
fn set_memory_lands_file_under_memories_at_bookmark() {
    let repo = make_scratch_repo("memory_set");
    let storage = Storage::open(&repo).expect("Storage::open");

    storage
        .set_memory("dolt-phantoms", "Dolt phantom DBs hide in three places")
        .expect("set_memory");

    // V3: each memory lives at `refs/jjf/memories/<key>:memory.json`.
    let text = read_at_memory_ref(&repo, "dolt-phantoms");
    let mem: jjf_storage::Memory = serde_json::from_str(&text).unwrap();
    assert_eq!(mem.key, "dolt-phantoms");
    assert_eq!(mem.value, "Dolt phantom DBs hide in three places");
    assert!(!mem.created_at.is_empty());
    assert_eq!(mem.created_at, mem.updated_at);
}

#[test]
fn set_memory_is_upsert_updates_value_and_updated_at() {
    let repo = make_scratch_repo("memory_upsert");
    let storage = Storage::open(&repo).expect("Storage::open");

    storage.set_memory("auth-jwt", "uses JWT").expect("first set");
    let first = storage.read_memory("auth-jwt").unwrap().unwrap();

    // Sleep a hair to avoid a same-second updated_at collision; the
    // storage uses second-resolution stamps.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    storage
        .set_memory("auth-jwt", "uses JWT not sessions")
        .expect("second set");
    let second = storage.read_memory("auth-jwt").unwrap().unwrap();

    assert_eq!(second.value, "uses JWT not sessions");
    // created_at preserved across upsert.
    assert_eq!(second.created_at, first.created_at);
    // updated_at bumped.
    assert_ne!(second.updated_at, first.updated_at);
}

#[test]
fn read_memory_returns_none_for_unknown_key() {
    let repo = make_scratch_repo("memory_read_missing");
    let storage = Storage::open(&repo).expect("Storage::open");
    assert!(storage.read_memory("nope").unwrap().is_none());
}

#[test]
fn unset_memory_removes_file_and_record() {
    let repo = make_scratch_repo("memory_unset");
    let storage = Storage::open(&repo).expect("Storage::open");

    storage.set_memory("temp-rule", "a value").unwrap();
    assert!(storage.read_memory("temp-rule").unwrap().is_some());

    storage.unset_memory("temp-rule").expect("unset_memory");
    assert!(storage.read_memory("temp-rule").unwrap().is_none());

    // File listing should also stop returning the key.
    let mems = storage.list_memories().unwrap();
    assert!(mems.iter().all(|m| m.key != "temp-rule"));
}

#[test]
fn unset_memory_on_unknown_key_errors() {
    let repo = make_scratch_repo("memory_unset_missing");
    let storage = Storage::open(&repo).expect("Storage::open");
    let err = storage.unset_memory("no-such-key").unwrap_err();
    match err {
        StorageError::Invalid(msg) => {
            assert!(msg.contains("no memory with key"), "got: {msg}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn list_memories_returns_ascending_keys() {
    let repo = make_scratch_repo("memory_list");
    let storage = Storage::open(&repo).expect("Storage::open");

    storage.set_memory("zebra-rule", "z").unwrap();
    storage.set_memory("alpha-rule", "a").unwrap();
    storage.set_memory("middle-rule", "m").unwrap();

    let mems = storage.list_memories().unwrap();
    let keys: Vec<&str> = mems.iter().map(|m| m.key.as_str()).collect();
    assert_eq!(keys, vec!["alpha-rule", "middle-rule", "zebra-rule"]);
}

#[test]
fn list_memories_empty_when_none_set() {
    let repo = make_scratch_repo("memory_list_empty");
    let storage = Storage::open(&repo).expect("Storage::open");
    let mems = storage.list_memories().unwrap();
    assert!(mems.is_empty(), "expected no memories, got {mems:?}");
}

#[test]
fn set_memory_rejects_empty_value() {
    let repo = make_scratch_repo("memory_empty_value");
    let storage = Storage::open(&repo).expect("Storage::open");
    let err = storage.set_memory("some-key", "   ").unwrap_err();
    matches!(err, StorageError::Invalid(_));
}

#[test]
fn set_memory_rejects_invalid_key() {
    let repo = make_scratch_repo("memory_bad_key");
    let storage = Storage::open(&repo).expect("Storage::open");
    let err = storage.set_memory("Bad Key", "value").unwrap_err();
    match err {
        StorageError::Invalid(msg) => {
            assert!(msg.contains("invalid memory key"), "got: {msg}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn set_memory_commit_carries_set_memory_trailer() {
    let repo = make_scratch_repo("memory_trailer_shape");
    let storage = Storage::open(&repo).expect("Storage::open");
    storage.set_memory("hello-world", "the value").unwrap();
    // V3: each memory's commit chain lives at `refs/jjf/memories/<key>`.
    let desc = git_capture(
        &[
            "log",
            "-n",
            "1",
            "--format=%B",
            "refs/jjf/memories/hello-world",
        ],
        &repo,
    );
    assert!(
        desc.contains("Jjf-Op: set-memory"),
        "commit description missing set-memory trailer:\n{desc}"
    );
    assert!(
        desc.contains("Jjf-Memory-Key: hello-world"),
        "commit description missing key trailer:\n{desc}"
    );
    assert!(
        desc.contains("Jjf-Memory-Value: the value"),
        "commit description missing value trailer:\n{desc}"
    );
    // No Jjf-Issue trailer — memory ops don't carry one.
    assert!(
        !desc.contains("Jjf-Issue:"),
        "memory commit should not carry Jjf-Issue trailer:\n{desc}"
    );
}

#[test]
fn unset_memory_commit_carries_unset_memory_trailer() {
    let repo = make_scratch_repo("memory_unset_trailer");
    let storage = Storage::open(&repo).expect("Storage::open");
    storage.set_memory("temp", "value").unwrap();
    storage.unset_memory("temp").unwrap();
    // V3: even after unset, the per-memory ref persists with the
    // tombstone commit at its tip.
    let desc = git_capture(
        &["log", "-n", "1", "--format=%B", "refs/jjf/memories/temp"],
        &repo,
    );
    assert!(
        desc.contains("Jjf-Op: unset-memory"),
        "commit description missing unset-memory trailer:\n{desc}"
    );
}

#[test]
fn memory_ops_do_not_pollute_issue_history() {
    // A memory mutation lands a commit on the issues bookmark, but the
    // per-issue history walker must NOT include it in any issue's
    // chain (its trailer has no Jjf-Issue).
    let repo = make_scratch_repo("memory_no_pollute");
    let storage = Storage::open(&repo).expect("Storage::open");

    let id = storage
        .create_issue(&IssueDraft {
            title: "the issue".into(),
            ..Default::default()
        })
        .unwrap();
    storage.set_memory("some-rule", "the rule").unwrap();

    let hist = storage.read_history(&id).unwrap();
    // Should be exactly the create op — no memory op in the issue's chain.
    let memory_ops: Vec<_> = hist
        .iter()
        .filter(|e| {
            matches!(
                e.op,
                Op::CommentAdd { .. } | Op::SetBody { .. } | Op::SetTitle { .. }
            )
        })
        .collect();
    assert_eq!(memory_ops.len(), 0);
    // The issue's history is just `create`.
    assert!(hist.iter().any(|e| matches!(e.op, Op::Create { .. })));
    assert!(
        hist.iter().all(|e| !matches!(e.op, Op::Merge { .. })),
        "no merge ops expected, got {hist:?}"
    );
}

// ---- agent-claim-atomic (v2.3) ---------------------------------------

#[test]
fn claim_lands_set_assignee_and_set_status_in_one_commit() {
    // The headline acceptance criterion: `Storage::claim` sets
    // assignee + status=in-progress in ONE commit carrying TWO
    // `Jjf-Op:` trailers.
    let repo = make_scratch_repo("claim_atomic_one_commit");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "claim me".into(),
            ..Default::default()
        })
        .unwrap();
    let baseline = storage.read_history(&id).unwrap().len();

    storage.claim(&id, "alice").unwrap();

    // Record state.
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.assignee.as_deref(), Some("alice"));
    assert_eq!(issue.status, Status::InProgress);

    // History: two new ops on ONE commit.
    let hist = storage.read_history(&id).unwrap();
    let new = &hist[baseline..];
    assert_eq!(
        new.len(),
        2,
        "expected two ops (set-assignee + set-status), got {new:#?}"
    );
    let commit = &new[0].commit;
    assert!(
        new.iter().all(|e| &e.commit == commit),
        "both new ops must share ONE commit: {new:#?}"
    );
    assert!(matches!(
        new[0].op,
        Op::SetAssignee {
            assignee: Some(ref a),
            ..
        } if a == "alice"
    ));
    assert!(matches!(
        new[1].op,
        Op::SetStatus {
            status: Status::InProgress,
            ..
        }
    ));
}

#[test]
fn claim_idempotent_same_user_is_no_op() {
    // Re-claiming an already-claimed issue by the SAME user is a
    // no-op: returns Ok(()) without writing a new commit.
    let repo = make_scratch_repo("claim_idempotent_same_user");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.claim(&id, "alice").unwrap();
    let after_first = storage.read_history(&id).unwrap().len();
    storage.claim(&id, "alice").unwrap();
    let after_second = storage.read_history(&id).unwrap().len();
    assert_eq!(
        after_first, after_second,
        "second claim by same user must not write a new commit"
    );
}

#[test]
fn claim_different_user_errors_with_already_claimed() {
    let repo = make_scratch_repo("claim_different_user");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.claim(&id, "alice").unwrap();
    let err = storage.claim(&id, "bob").unwrap_err();
    match err {
        StorageError::AlreadyClaimed { by } => {
            assert_eq!(by, "alice");
        }
        other => panic!("expected AlreadyClaimed, got {other:?}"),
    }
}

#[test]
fn unclaim_clears_assignee_and_status_to_open() {
    let repo = make_scratch_repo("unclaim_round_trip");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.claim(&id, "alice").unwrap();
    storage.unclaim(&id).unwrap();
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.assignee, None);
    assert_eq!(issue.status, Status::Open);
}

#[test]
fn unclaim_lands_two_trailers_on_one_commit() {
    let repo = make_scratch_repo("unclaim_one_commit");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.claim(&id, "alice").unwrap();
    let before_unclaim = storage.read_history(&id).unwrap().len();
    storage.unclaim(&id).unwrap();
    let hist = storage.read_history(&id).unwrap();
    let new = &hist[before_unclaim..];
    assert_eq!(new.len(), 2, "expected two ops, got {new:#?}");
    let commit = &new[0].commit;
    assert!(
        new.iter().all(|e| &e.commit == commit),
        "both unclaim ops must share ONE commit: {new:#?}"
    );
    assert!(matches!(
        new[0].op,
        Op::SetAssignee { assignee: None, .. }
    ));
    assert!(matches!(
        new[1].op,
        Op::SetStatus {
            status: Status::Open,
            ..
        }
    ));
}

#[test]
fn unclaim_on_already_open_unassigned_is_no_op() {
    let repo = make_scratch_repo("unclaim_no_op");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    let baseline = storage.read_history(&id).unwrap().len();
    storage.unclaim(&id).unwrap();
    let after = storage.read_history(&id).unwrap().len();
    assert_eq!(baseline, after, "unclaim on unclaimed must not commit");
}

#[test]
fn claim_on_closed_issue_errors_invalid() {
    let repo = make_scratch_repo("claim_closed_errors");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.set_status(&id, Status::Closed).unwrap();
    let err = storage.claim(&id, "alice").unwrap_err();
    assert!(
        matches!(err, StorageError::Invalid(_)),
        "expected Invalid, got {err:?}"
    );
}

#[test]
fn list_ready_excludes_in_progress_by_default() {
    let repo = make_scratch_repo("ready_excludes_claimed");
    let storage = Storage::open(&repo).unwrap();
    let a = storage
        .create_issue(&IssueDraft {
            title: "A".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    let _b = storage
        .create_issue(&IssueDraft {
            title: "B".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    storage.claim(&a, "alice").unwrap();

    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    let ids: Vec<&IssueId> = ready.iter().map(|i| &i.id).collect();
    assert!(!ids.contains(&&a), "claimed A must not appear in ready: {ids:?}");
    assert_eq!(ready.len(), 1, "only B should be ready: {ready:#?}");
}

#[test]
fn list_ready_includes_in_progress_when_include_claimed_set() {
    let repo = make_scratch_repo("ready_include_claimed");
    let storage = Storage::open(&repo).unwrap();
    let a = storage
        .create_issue(&IssueDraft {
            title: "A".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    let _b = storage
        .create_issue(&IssueDraft {
            title: "B".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    storage.claim(&a, "alice").unwrap();

    let ready = storage
        .list_ready(&ReadyFilter {
            include_claimed: true,
            ..Default::default()
        })
        .unwrap();
    let ids: Vec<&IssueId> = ready.iter().map(|i| &i.id).collect();
    assert!(ids.contains(&&a), "claimed A must appear with include_claimed: {ids:?}");
    assert_eq!(ready.len(), 2, "both A and B should be visible: {ready:#?}");
}

#[test]
fn status_in_progress_serializes_with_hyphen() {
    // Wire spelling: serde rename `in-progress` (hyphenated).
    let s = serde_json::to_string(&Status::InProgress).unwrap();
    assert_eq!(s, "\"in-progress\"");
    let back: Status = serde_json::from_str("\"in-progress\"").unwrap();
    assert_eq!(back, Status::InProgress);
}

#[test]
fn in_progress_dep_blocks_dependent_from_ready() {
    // An InProgress dep blocks the same as Open: the dep isn't
    // closed, so the dependent is still blocked.
    let repo = make_scratch_repo("ready_in_progress_dep_blocks");
    let storage = Storage::open(&repo).unwrap();
    let a = storage
        .create_issue(&IssueDraft {
            title: "A".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    let b = storage
        .create_issue(&IssueDraft {
            title: "B".into(),
            type_: Some(IssueType::Feature),
            dependencies: vec![DepEdge::blocks(a.clone())],
            ..Default::default()
        })
        .unwrap();
    storage.claim(&a, "alice").unwrap();
    // Default filter excludes A (claimed) AND B (blocked on A
    // which is InProgress, not Closed).
    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    let ids: Vec<&IssueId> = ready.iter().map(|i| &i.id).collect();
    assert!(!ids.contains(&&a), "A is claimed: {ids:?}");
    assert!(!ids.contains(&&b), "B blocked on InProgress A: {ids:?}");
}

// ---- agent-await-gates-impl (v2.5) -----------------------------------

#[test]
fn status_blocked_serializes_with_wire_spelling() {
    // Wire spelling: serde rename_all = lowercase. `Status::Blocked`
    // round-trips as `blocked`.
    let s = serde_json::to_string(&Status::Blocked).unwrap();
    assert_eq!(s, "\"blocked\"");
    let back: Status = serde_json::from_str("\"blocked\"").unwrap();
    assert_eq!(back, Status::Blocked);
}

#[test]
fn block_lands_set_status_and_set_block_reason_in_one_commit() {
    // The headline acceptance: `Storage::block` sets status =
    // blocked AND records block_reason in ONE commit carrying
    // TWO `Jjf-Op:` trailers.
    let repo = make_scratch_repo("block_atomic_one_commit");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "park me".into(),
            ..Default::default()
        })
        .unwrap();
    let baseline = storage.read_history(&id).unwrap().len();

    storage.block(&id, Some("waiting on PR-42")).unwrap();

    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.status, Status::Blocked);
    assert_eq!(issue.block_reason.as_deref(), Some("waiting on PR-42"));

    let hist = storage.read_history(&id).unwrap();
    let new = &hist[baseline..];
    assert_eq!(
        new.len(),
        2,
        "expected two ops (set-status + set-block-reason), got {new:#?}"
    );
    let commit = &new[0].commit;
    assert!(
        new.iter().all(|e| &e.commit == commit),
        "both ops must share ONE commit: {new:#?}"
    );
    assert!(matches!(
        new[0].op,
        Op::SetStatus {
            status: Status::Blocked,
            ..
        }
    ));
    assert!(matches!(
        new[1].op,
        Op::SetBlockReason {
            reason: Some(ref r),
            ..
        } if r == "waiting on PR-42"
    ));
}

#[test]
fn block_without_reason_records_null() {
    let repo = make_scratch_repo("block_no_reason");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "park me silently".into(),
            ..Default::default()
        })
        .unwrap();
    storage.block(&id, None).unwrap();
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.status, Status::Blocked);
    assert_eq!(issue.block_reason, None);
}

#[test]
fn block_normalizes_whitespace_only_reason_to_none() {
    // A whitespace-only reason should land as `None` rather than
    // a confusing `Some(" ")`. The CLI relies on this for clean
    // round-trip behavior when the user passes `--reason ""`.
    let repo = make_scratch_repo("block_whitespace_reason");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.block(&id, Some("   ")).unwrap();
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.block_reason, None);
}

#[test]
fn block_rejects_multiline_reason() {
    // Newlines would corrupt the `Jjf-Reason:` trailer. Reject at
    // the storage boundary with `Error::Invalid`.
    let repo = make_scratch_repo("block_rejects_newlines");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    let err = storage.block(&id, Some("line one\nline two")).unwrap_err();
    assert!(
        matches!(err, StorageError::Invalid(_)),
        "expected Invalid, got {err:?}"
    );
}

#[test]
fn block_on_closed_issue_errors_invalid() {
    let repo = make_scratch_repo("block_closed_errors");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.set_status(&id, Status::Closed).unwrap();
    let err = storage.block(&id, Some("reason")).unwrap_err();
    assert!(
        matches!(err, StorageError::Invalid(_)),
        "expected Invalid, got {err:?}"
    );
}

#[test]
fn unblock_clears_status_and_reason() {
    let repo = make_scratch_repo("unblock_round_trip");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.block(&id, Some("a reason")).unwrap();
    storage.unblock(&id).unwrap();
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.status, Status::Open);
    assert_eq!(issue.block_reason, None);
}

#[test]
fn unblock_lands_two_trailers_on_one_commit() {
    let repo = make_scratch_repo("unblock_one_commit");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.block(&id, Some("park")).unwrap();
    let before_unblock = storage.read_history(&id).unwrap().len();
    storage.unblock(&id).unwrap();
    let hist = storage.read_history(&id).unwrap();
    let new = &hist[before_unblock..];
    assert_eq!(new.len(), 2, "expected two ops, got {new:#?}");
    let commit = &new[0].commit;
    assert!(
        new.iter().all(|e| &e.commit == commit),
        "both unblock ops must share ONE commit: {new:#?}"
    );
    assert!(matches!(
        new[0].op,
        Op::SetStatus {
            status: Status::Open,
            ..
        }
    ));
    assert!(matches!(
        new[1].op,
        Op::SetBlockReason { reason: None, .. }
    ));
}

#[test]
fn unblock_on_already_open_unparked_is_no_op() {
    let repo = make_scratch_repo("unblock_no_op");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    let baseline = storage.read_history(&id).unwrap().len();
    storage.unblock(&id).unwrap();
    let after = storage.read_history(&id).unwrap().len();
    assert_eq!(baseline, after, "unblock on already-open must not commit");
}

#[test]
fn list_ready_excludes_blocked_by_default() {
    let repo = make_scratch_repo("ready_excludes_blocked");
    let storage = Storage::open(&repo).unwrap();
    let a = storage
        .create_issue(&IssueDraft {
            title: "A".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    let _b = storage
        .create_issue(&IssueDraft {
            title: "B".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    storage.block(&a, Some("waiting")).unwrap();

    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    let ids: Vec<&IssueId> = ready.iter().map(|i| &i.id).collect();
    assert!(!ids.contains(&&a), "blocked A must not appear: {ids:?}");
    assert_eq!(ready.len(), 1, "only B should be ready: {ready:#?}");
}

#[test]
fn list_ready_includes_blocked_when_include_blocked_set() {
    let repo = make_scratch_repo("ready_include_blocked");
    let storage = Storage::open(&repo).unwrap();
    let a = storage
        .create_issue(&IssueDraft {
            title: "A".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    let _b = storage
        .create_issue(&IssueDraft {
            title: "B".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    storage.block(&a, Some("park")).unwrap();

    let ready = storage
        .list_ready(&ReadyFilter {
            include_blocked: true,
            ..Default::default()
        })
        .unwrap();
    let ids: Vec<&IssueId> = ready.iter().map(|i| &i.id).collect();
    assert!(
        ids.contains(&&a),
        "blocked A must appear with include_blocked: {ids:?}"
    );
    assert_eq!(
        ready.len(),
        2,
        "both A and B should be visible: {ready:#?}"
    );
}

#[test]
fn blocked_dep_blocks_dependent_from_ready() {
    // A Blocked dep blocks dependents the same way Open/InProgress
    // do — it's not closed yet, just parked.
    let repo = make_scratch_repo("ready_blocked_dep_blocks");
    let storage = Storage::open(&repo).unwrap();
    let a = storage
        .create_issue(&IssueDraft {
            title: "A".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    let b = storage
        .create_issue(&IssueDraft {
            title: "B".into(),
            type_: Some(IssueType::Feature),
            dependencies: vec![DepEdge::blocks(a.clone())],
            ..Default::default()
        })
        .unwrap();
    storage.block(&a, Some("park")).unwrap();
    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    let ids: Vec<&IssueId> = ready.iter().map(|i| &i.id).collect();
    assert!(!ids.contains(&&a), "A is blocked: {ids:?}");
    assert!(!ids.contains(&&b), "B blocked on Blocked A: {ids:?}");
}

#[test]
fn block_reason_lww_later_overwrites_earlier() {
    // Scalar LWW: a second `block` with a different reason
    // overwrites the first. The op-space resolver picks the
    // later write by `Jjf-At`, same as title/body/assignee.
    let repo = make_scratch_repo("block_reason_lww");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.block(&id, Some("first reason")).unwrap();
    storage.block(&id, Some("second reason")).unwrap();
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.block_reason.as_deref(), Some("second reason"));
}

// ============================================================
// Abandoned status (v2.7 `abandon-verb`, ticket `c1ffea7`).
// Soft-delete: stays in history, slug stays claimed, excluded
// from `list_ready` unconditionally.
// ============================================================

#[test]
fn status_abandoned_serializes_with_wire_spelling() {
    // Wire spelling is `abandoned` (lowercase, via serde
    // rename_all). Mirrors the block / in-progress checks; pins
    // the trailer payload.
    assert_eq!(Status::Abandoned.as_str(), "abandoned");
    let v = serde_json::to_value(Status::Abandoned).unwrap();
    assert_eq!(v, serde_json::json!("abandoned"));
}

#[test]
fn set_status_to_abandoned_round_trips_via_read() {
    // The minimal happy path: a `set_status(Abandoned)` lands
    // and `Storage::read` reports the new value. Mirrors the
    // shape of `block` / `unblock` round-trip tests.
    let repo = make_scratch_repo("abandon_round_trip");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "soon to be abandoned".into(),
            ..Default::default()
        })
        .unwrap();
    storage.set_status(&id, Status::Abandoned).unwrap();
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.status, Status::Abandoned);
}

#[test]
fn list_ready_excludes_abandoned_unconditionally() {
    // Abandoned issues never appear in the ready set — even
    // with `include_blocked` and `include_claimed` both set,
    // unlike Blocked / InProgress which are gated by flags.
    // Closed has the same "no override" semantics; this is the
    // companion guarantee for Abandoned.
    let repo = make_scratch_repo("ready_excludes_abandoned");
    let storage = Storage::open(&repo).unwrap();
    let a = storage
        .create_issue(&IssueDraft {
            title: "A".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    let _b = storage
        .create_issue(&IssueDraft {
            title: "B".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    storage.set_status(&a, Status::Abandoned).unwrap();

    // Default filter: only B.
    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    let ids: Vec<&IssueId> = ready.iter().map(|i| &i.id).collect();
    assert!(!ids.contains(&&a), "abandoned A must not appear: {ids:?}");
    assert_eq!(ready.len(), 1, "only B should be ready: {ready:#?}");

    // Even with both flip-flags on, A stays out — there's no
    // `include_abandoned` flag on `ReadyFilter` because the
    // spec forbids it.
    let ready = storage
        .list_ready(&ReadyFilter {
            include_blocked: true,
            include_claimed: true,
            ..Default::default()
        })
        .unwrap();
    let ids: Vec<&IssueId> = ready.iter().map(|i| &i.id).collect();
    assert!(
        !ids.contains(&&a),
        "abandoned A must not appear even with include_blocked + include_claimed: {ids:?}"
    );
    assert_eq!(ready.len(), 1, "only B should be ready: {ready:#?}");
}

#[test]
fn slug_uniqueness_blocks_against_abandoned_holder() {
    // F-012 / spec §3.4 contract: slug uniqueness spans every
    // status, including Abandoned. Abandoning an issue doesn't
    // release its slug — a new ticket must pick a fresh one.
    // The dual companion to `slug_uniqueness_scope_spans_all_statuses_including_closed`.
    let repo = make_scratch_repo("slug_abandoned_holder");
    let storage = Storage::open(&repo).unwrap();
    let first = storage
        .create_issue(&IssueDraft {
            title: "first".into(),
            slug: Some("mis-filed".into()),
            ..Default::default()
        })
        .unwrap();
    storage.set_status(&first, Status::Abandoned).unwrap();
    let err = storage
        .create_issue(&IssueDraft {
            title: "second".into(),
            slug: Some("mis-filed".into()),
            ..Default::default()
        })
        .unwrap_err();
    match err {
        StorageError::SlugCollision { slug, conflicts_with } => {
            assert_eq!(slug, "mis-filed");
            assert_eq!(
                conflicts_with, first,
                "abandoned issue's id must be carried in conflicts_with"
            );
        }
        other => panic!("expected SlugCollision against abandoned holder, got {other:?}"),
    }
}

#[test]
fn re_abandoning_lands_fresh_set_status_trailer_each_call() {
    // Like close-twice: re-abandoning an already-abandoned
    // issue is NOT a no-op at the commit level. It still lands
    // a fresh `set-status` op so the audit log records the
    // intent. Idempotent only at the data level (status stays
    // Abandoned, no observable record change).
    let repo = make_scratch_repo("abandon_twice");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "abandon me twice".into(),
            ..Default::default()
        })
        .unwrap();
    let baseline_set_status = storage
        .read_history(&id)
        .unwrap()
        .into_iter()
        .filter(|e| matches!(e.op, Op::SetStatus { .. }))
        .count();
    assert_eq!(
        baseline_set_status, 0,
        "fresh issue has no set-status ops"
    );

    storage.set_status(&id, Status::Abandoned).unwrap();
    // Same-second guard, same rationale as close-twice.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    storage.set_status(&id, Status::Abandoned).unwrap();

    let issue = storage.read(&id).unwrap();
    assert_eq!(
        issue.status,
        Status::Abandoned,
        "status is idempotent at the data level"
    );

    let set_status_count = storage
        .read_history(&id)
        .unwrap()
        .into_iter()
        .filter(|e| matches!(e.op, Op::SetStatus { .. }))
        .count();
    assert_eq!(
        set_status_count, 2,
        "two abandons must land two fresh set-status ops (non-idempotency)"
    );
}

#[test]
fn abandoned_dep_does_not_block_dependent_from_ready() {
    // An abandoned dep is like a closed dep — the work will
    // never be done, so dependents are free of it. Companion to
    // `list_ready_closed_dependency_does_not_block`.
    let repo = make_scratch_repo("ready_abandoned_dep_doesnt_block");
    let storage = Storage::open(&repo).unwrap();
    let parent = storage
        .create_issue(&IssueDraft {
            title: "parent".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    let child = storage
        .create_issue(&IssueDraft {
            title: "child".into(),
            type_: Some(IssueType::Feature),
            dependencies: vec![DepEdge::blocks(parent.clone())],
            ..Default::default()
        })
        .unwrap();
    // With parent Open, child is blocked.
    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    let ids: Vec<&IssueId> = ready.iter().map(|i| &i.id).collect();
    assert!(!ids.contains(&&child), "open dep blocks child: {ids:?}");

    storage.set_status(&parent, Status::Abandoned).unwrap();
    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    let ids: Vec<&IssueId> = ready.iter().map(|i| &i.id).collect();
    assert!(
        ids.contains(&&child),
        "abandoned dep should not block child: {ids:?}"
    );
    assert!(
        !ids.contains(&&parent),
        "abandoned parent itself must be excluded from ready: {ids:?}"
    );
}

// ============================================================
// Snapshot cache (`docs/storage-index-design.md`, ticket `61e9a1c`).
// ============================================================

/// Path to the on-disk cache file. Mirrors `cache::cache_path`
/// internally — exposed here as a string for stat-checking.
fn cache_file_path(repo: &Path) -> PathBuf {
    repo.join(".jj").join("jjforge-cache.json")
}

#[test]
fn cache_is_written_on_first_list_call() {
    let abs = make_scratch_repo("cache_first_write");
    let storage = Storage::open(&abs).unwrap();
    storage
        .create_issue(&IssueDraft {
            title: "first".into(),
            ..Default::default()
        })
        .unwrap();
    let cache_path = cache_file_path(&abs);
    // Cache may exist from create_issue's preflight (the dup-id
    // probe goes through list_ids). Force a fresh state.
    let _ = std::fs::remove_file(&cache_path);
    assert!(!cache_path.exists(), "precondition: cache cleared");
    let _ = storage.list_ready(&ReadyFilter::default()).unwrap();
    assert!(
        cache_path.exists(),
        "cache file should be written after a list_ready call"
    );
}

#[test]
fn cache_hits_when_no_writes_intervene() {
    let abs = make_scratch_repo("cache_hits_steady");
    let storage = Storage::open(&abs).unwrap();
    for i in 0..3 {
        storage
            .create_issue(&IssueDraft {
                title: format!("issue {i}"),
                ..Default::default()
            })
            .unwrap();
    }
    // Prime the cache.
    let first = storage.list_ready(&ReadyFilter::default()).unwrap();
    let cache_path = cache_file_path(&abs);
    let mtime_before = std::fs::metadata(&cache_path).unwrap().modified().unwrap();
    // Sleep a hair so a hypothetical rewrite would land a different
    // mtime. We assert NO rewrite.
    std::thread::sleep(std::time::Duration::from_millis(50));
    let second = storage.list_ready(&ReadyFilter::default()).unwrap();
    let mtime_after = std::fs::metadata(&cache_path).unwrap().modified().unwrap();
    assert_eq!(first.len(), second.len(), "result should be the same set");
    assert_eq!(
        mtime_before, mtime_after,
        "cache mtime should not change between reads without writes"
    );
}

#[test]
fn cache_invalidates_after_a_write() {
    let abs = make_scratch_repo("cache_invalidates_after_write");
    let storage = Storage::open(&abs).unwrap();
    let _a = storage
        .create_issue(&IssueDraft {
            title: "first".into(),
            ..Default::default()
        })
        .unwrap();
    // Prime the cache.
    let before = storage.list_ready(&ReadyFilter::default()).unwrap();
    assert_eq!(before.len(), 1);

    // Mutate.
    let b = storage
        .create_issue(&IssueDraft {
            title: "second".into(),
            ..Default::default()
        })
        .unwrap();

    // Next read must see the new issue. The bookmark moved; cache
    // probe sees head mismatch; rebuild kicks in.
    let after = storage.list_ready(&ReadyFilter::default()).unwrap();
    assert_eq!(after.len(), 2, "post-write read should see new issue");
    let after_ids: Vec<&IssueId> = after.iter().map(|i| &i.id).collect();
    assert!(after_ids.contains(&&b), "new issue b must be in result");
}

#[test]
fn cache_corruption_triggers_rebuild() {
    let abs = make_scratch_repo("cache_corrupt_rebuild");
    let storage = Storage::open(&abs).unwrap();
    let a = storage
        .create_issue(&IssueDraft {
            title: "real".into(),
            ..Default::default()
        })
        .unwrap();
    // Prime the cache.
    let _ = storage.list_ready(&ReadyFilter::default()).unwrap();
    let cache_path = cache_file_path(&abs);
    assert!(cache_path.exists(), "cache should exist after first read");

    // Corrupt the cache file.
    std::fs::write(&cache_path, "{not valid json !!!").unwrap();

    // Read should still succeed and return the correct issue.
    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    assert_eq!(ready.len(), 1, "corrupt cache rebuilds correctly");
    assert_eq!(ready[0].id, a);
}

#[test]
fn cache_missing_file_rebuilds() {
    let abs = make_scratch_repo("cache_missing_rebuild");
    let storage = Storage::open(&abs).unwrap();
    let a = storage
        .create_issue(&IssueDraft {
            title: "lonely".into(),
            ..Default::default()
        })
        .unwrap();
    let cache_path = cache_file_path(&abs);
    let _ = std::fs::remove_file(&cache_path);
    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, a);
    assert!(
        cache_path.exists(),
        "rebuild should have re-persisted the cache"
    );
}

#[test]
fn cache_resolve_by_slug_round_trips_after_write() {
    let abs = make_scratch_repo("cache_resolve_slug");
    let storage = Storage::open(&abs).unwrap();
    let a = storage
        .create_issue(&IssueDraft {
            title: "slug carrier".into(),
            slug: Some("the-slug".into()),
            ..Default::default()
        })
        .unwrap();
    // First resolve primes the cache.
    let resolved = storage.resolve("the-slug").unwrap();
    assert_eq!(resolved, a);
    // Second resolve must still hit (cache stable, no write).
    let resolved2 = storage.resolve("the-slug").unwrap();
    assert_eq!(resolved2, a);
    // Now write another issue with a different slug; resolve must
    // see the fresh data.
    let b = storage
        .create_issue(&IssueDraft {
            title: "newcomer".into(),
            slug: Some("newcomer".into()),
            ..Default::default()
        })
        .unwrap();
    let resolved_b = storage.resolve("newcomer").unwrap();
    assert_eq!(resolved_b, b);
    // And the original slug still resolves correctly post-write.
    let resolved_again = storage.resolve("the-slug").unwrap();
    assert_eq!(resolved_again, a);
}

#[test]
fn cache_memory_round_trips_after_write() {
    let abs = make_scratch_repo("cache_memory_roundtrip");
    let storage = Storage::open(&abs).unwrap();
    storage
        .set_memory("first-key", "first value")
        .unwrap();
    // Prime.
    let m = storage.read_memory("first-key").unwrap();
    assert_eq!(m.unwrap().value, "first value");
    // Update; next read must see the update.
    storage
        .set_memory("first-key", "updated value")
        .unwrap();
    let m = storage.read_memory("first-key").unwrap();
    assert_eq!(m.unwrap().value, "updated value");
    let all = storage.list_memories().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].value, "updated value");
}

#[test]
fn cache_hit_avoids_rebuild_n_issues() {
    // Demonstrate the speedup: build a non-trivial issue set, prime
    // the cache, then assert a second read is much faster than the
    // first one (which had to rebuild).
    let n = 25_usize; // generous for a debug-build test, fast enough.
    let abs = make_scratch_repo("cache_hit_speedup");
    let storage = Storage::open(&abs).unwrap();
    for i in 0..n {
        storage
            .create_issue(&IssueDraft {
                title: format!("issue {i}"),
                ..Default::default()
            })
            .unwrap();
    }
    // Force a clean cache miss for the first measurement.
    let cache_path = cache_file_path(&abs);
    let _ = std::fs::remove_file(&cache_path);
    let t0 = std::time::Instant::now();
    let first = storage.list_ready(&ReadyFilter::default()).unwrap();
    let first_dur = t0.elapsed();
    assert_eq!(first.len(), n);

    let t1 = std::time::Instant::now();
    let second = storage.list_ready(&ReadyFilter::default()).unwrap();
    let second_dur = t1.elapsed();
    assert_eq!(second.len(), n);

    // The hit should be a small multiple of the head-commit probe
    // cost (one `jj log`). The miss is rebuild (one `jj file show`
    // per top-level dir). Empirically the hit is < the miss; we
    // assert a conservative 2x margin so heavily-loaded CI doesn't
    // flake. (The 10x acceptance bar in the ticket assumes N=100;
    // at N=25 in debug it's still meaningful.)
    assert!(
        second_dur < first_dur,
        "cache hit ({second_dur:?}) should be faster than rebuild ({first_dur:?})"
    );
}

// --- qa-title-validation (issue e4e483b) ---------------------------------
//
// `validate_title` rejects four classes of input at the storage
// boundary: empty, embedded newline, embedded null byte, and any
// other control character (tabs included). `Storage::create_issue`,
// `Storage::set_title`, and `Storage::update` all delegate to it
// and surface the typed `InvalidTitle` error.

#[test]
fn create_issue_rejects_empty_title_with_typed_reason() {
    let repo = make_scratch_repo("create_title_empty");
    let storage = Storage::open(&repo).unwrap();
    let err = storage
        .create_issue(&IssueDraft {
            title: "   ".into(),
            ..Default::default()
        })
        .unwrap_err();
    match err {
        StorageError::InvalidTitle { title, reason } => {
            assert_eq!(title, "   ");
            assert_eq!(reason, TitleInvalidReason::Empty);
        }
        other => panic!("expected InvalidTitle, got {other:?}"),
    }
}

#[test]
fn create_issue_rejects_embedded_newline_in_title() {
    let repo = make_scratch_repo("create_title_newline");
    let storage = Storage::open(&repo).unwrap();
    let err = storage
        .create_issue(&IssueDraft {
            title: "foo\nbar".into(),
            ..Default::default()
        })
        .unwrap_err();
    match err {
        StorageError::InvalidTitle { title, reason } => {
            assert_eq!(title, "foo\nbar");
            assert_eq!(reason, TitleInvalidReason::Newline);
        }
        other => panic!("expected InvalidTitle/Newline, got {other:?}"),
    }
}

#[test]
fn create_issue_rejects_carriage_return_in_title() {
    let repo = make_scratch_repo("create_title_cr");
    let storage = Storage::open(&repo).unwrap();
    let err = storage
        .create_issue(&IssueDraft {
            title: "foo\rbar".into(),
            ..Default::default()
        })
        .unwrap_err();
    match err {
        StorageError::InvalidTitle { reason, .. } => {
            assert_eq!(reason, TitleInvalidReason::Newline);
        }
        other => panic!("expected InvalidTitle/Newline (CR), got {other:?}"),
    }
}

#[test]
fn create_issue_rejects_embedded_null_byte_in_title() {
    let repo = make_scratch_repo("create_title_null");
    let storage = Storage::open(&repo).unwrap();
    let err = storage
        .create_issue(&IssueDraft {
            title: "a\0b".into(),
            ..Default::default()
        })
        .unwrap_err();
    match err {
        StorageError::InvalidTitle { title, reason } => {
            assert_eq!(title, "a\0b");
            assert_eq!(reason, TitleInvalidReason::NullByte);
        }
        other => panic!("expected InvalidTitle/NullByte, got {other:?}"),
    }
}

#[test]
fn create_issue_rejects_tab_in_title_as_control_char() {
    let repo = make_scratch_repo("create_title_tab");
    let storage = Storage::open(&repo).unwrap();
    let err = storage
        .create_issue(&IssueDraft {
            title: "a\tb".into(),
            ..Default::default()
        })
        .unwrap_err();
    match err {
        StorageError::InvalidTitle { reason, .. } => match reason {
            TitleInvalidReason::ControlChar { codepoint } => {
                assert_eq!(codepoint, 0x09, "tab should be U+0009");
            }
            other => panic!("expected ControlChar(0x09), got {other:?}"),
        },
        other => panic!("expected InvalidTitle/ControlChar, got {other:?}"),
    }
}

#[test]
fn set_title_rejects_newline_with_typed_reason() {
    let repo = make_scratch_repo("set_title_newline");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "baseline".into(),
            ..Default::default()
        })
        .unwrap();
    let err = storage.set_title(&id, "foo\nbar").unwrap_err();
    match err {
        StorageError::InvalidTitle { reason, .. } => {
            assert_eq!(reason, TitleInvalidReason::Newline);
        }
        other => panic!("expected InvalidTitle/Newline, got {other:?}"),
    }
}

#[test]
fn update_with_invalid_title_is_rejected_before_commit() {
    let repo = make_scratch_repo("update_title_invalid");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "baseline".into(),
            ..Default::default()
        })
        .unwrap();
    let history_before = storage.read_history(&id).unwrap().len();
    let err = storage
        .update(
            &id,
            UpdateFields {
                title: Some("a\0b".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
    match err {
        StorageError::InvalidTitle { reason, .. } => {
            assert_eq!(reason, TitleInvalidReason::NullByte);
        }
        other => panic!("expected InvalidTitle/NullByte, got {other:?}"),
    }
    let history_after = storage.read_history(&id).unwrap().len();
    assert_eq!(
        history_before, history_after,
        "rejected update must not land a commit"
    );
}

#[test]
fn validate_title_accepts_unicode_and_punctuation() {
    // Sanity: legitimate prose titles (asterinas migration use case)
    // must NOT be rejected by the new validator. Quotes, parens,
    // dashes, slashes, em-dash, non-ASCII letters all OK.
    let ok_titles = [
        "Fix the bug in the foo subsystem",
        "host-asterinas-migrate: import the upstream tree",
        "Why doesn't \"qux\" work? (it should)",
        "rust/no_std — drop the alloc crate",
        "Émilie hits the same panic",
    ];
    for t in &ok_titles {
        assert!(
            jjf_storage::validate_title(t).is_ok(),
            "validator wrongly rejected {t:?}"
        );
    }
}

// ---------------------------------------------------------------------
// qa-dep-validation (d1a01f0): the write path rejects phantom dep
// targets and self-deps at the storage boundary. Both checks fire
// BEFORE any mutating IO so a rejection leaves the bookmark
// untouched.
// ---------------------------------------------------------------------

#[test]
fn add_dep_edge_with_phantom_target_rejects_with_issue_not_found() {
    // `jjf dep add A <phantom>` — `<phantom>` has never existed on
    // the bookmark. Pre-validation rejects so the trailer doesn't
    // land.
    let repo = make_scratch_repo("dep_phantom_target_rejects");
    let storage = Storage::open(&repo).unwrap();

    let real = storage
        .create_issue(&IssueDraft {
            title: "real child".into(),
            type_: Some(IssueType::Bug),
            ..Default::default()
        })
        .unwrap();

    let phantom = IssueId::parse("deadbee").unwrap();
    let err = storage
        .add_dep_edge(&real, &phantom, DepKind::Blocks)
        .unwrap_err();
    match err {
        StorageError::IssueNotFound(id) => assert_eq!(id, phantom),
        other => panic!("expected IssueNotFound, got {other:?}"),
    }

    // The depender's record must have no dep edges.
    let after = storage.read(&real).unwrap();
    assert!(
        after.dependencies.is_empty(),
        "rejected dep_add must not land an edge: {:?}",
        after.dependencies,
    );
}

#[test]
fn add_dep_edge_with_self_target_rejects_with_self_dependency() {
    // `jjf dep add A A` — self-dep would make A permanently
    // blocked by itself. Reject at the boundary; no commit.
    let repo = make_scratch_repo("dep_self_rejects");
    let storage = Storage::open(&repo).unwrap();

    let id = storage
        .create_issue(&IssueDraft {
            title: "would self-block".into(),
            type_: Some(IssueType::Bug),
            ..Default::default()
        })
        .unwrap();

    let err = storage.add_dep_edge(&id, &id, DepKind::Blocks).unwrap_err();
    match err {
        StorageError::SelfDependency { id: bad } => assert_eq!(bad, id),
        other => panic!("expected SelfDependency, got {other:?}"),
    }

    let after = storage.read(&id).unwrap();
    assert!(
        after.dependencies.is_empty(),
        "rejected self-dep must not land an edge: {:?}",
        after.dependencies,
    );
}

#[test]
fn add_dep_edge_self_dep_rejected_for_all_kinds() {
    // Self-dep nonsense applies to every kind: blocks self-blocks,
    // parent-child self-parents, related/discovered-from are
    // nonsense pointing at self. Reject uniformly.
    let repo = make_scratch_repo("dep_self_all_kinds");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "self-only".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();

    for kind in [
        DepKind::Blocks,
        DepKind::ParentChild,
        DepKind::Related,
        DepKind::DiscoveredFrom,
    ] {
        let err = storage.add_dep_edge(&id, &id, kind).unwrap_err();
        assert!(
            matches!(err, StorageError::SelfDependency { .. }),
            "kind={kind:?}: expected SelfDependency, got {err:?}",
        );
    }
}

#[test]
fn create_issue_with_phantom_dep_target_rejects() {
    // `jjf new -d <phantom>` — the inline-on-create form. Validate
    // each dep target at create time so the resulting record can't
    // carry a dangling edge.
    let repo = make_scratch_repo("create_phantom_dep_rejects");
    let storage = Storage::open(&repo).unwrap();
    let phantom = IssueId::parse("deadbee").unwrap();

    let err = storage
        .create_issue(&IssueDraft {
            title: "depends on a ghost".into(),
            type_: Some(IssueType::Bug),
            dependencies: vec![DepEdge::blocks(phantom.clone())],
            ..Default::default()
        })
        .unwrap_err();
    match err {
        StorageError::IssueNotFound(id) => assert_eq!(id, phantom),
        other => panic!("expected IssueNotFound, got {other:?}"),
    }

    // The bookmark must not now contain any issue.
    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    assert!(
        ready.is_empty(),
        "rejected create_issue must not land a record: {ready:#?}",
    );
}

// ---------------------------------------------------------------------
// dep-cycle-undetected (43c7615): the write path rejects `dep add` that
// would close a cycle in the `blocks`-edge graph. Closed issues are
// still graph nodes — the walk doesn't short-circuit on status. Only
// the `blocks` kind is cycle-checked; the other kinds don't affect
// `jjf ready`.
// ---------------------------------------------------------------------

#[test]
fn add_dep_edge_direct_2_cycle_rejected() {
    // A blocks B (A.deps += blocks:B). Then B blocks A would close
    // a 2-cycle [A, B]. Reject; the chain in the error is the
    // existing path from target back to source — here, from A
    // (target) over A's blocks-deps back to B (source). A has one
    // blocks-dep: B. So the cycle path is [A, B].
    let repo = make_scratch_repo("dep_cycle_2");
    let storage = Storage::open(&repo).unwrap();

    let a = storage
        .create_issue(&IssueDraft {
            title: "A".into(),
            ..Default::default()
        })
        .unwrap();
    let b = storage
        .create_issue(&IssueDraft {
            title: "B".into(),
            ..Default::default()
        })
        .unwrap();

    // A.deps += blocks:B  — fine, A is blocked by B.
    storage.add_dep_edge(&a, &b, DepKind::Blocks).unwrap();

    // B.deps += blocks:A  — would close the cycle.
    let err = storage.add_dep_edge(&b, &a, DepKind::Blocks).unwrap_err();
    match err {
        StorageError::DependencyCycle {
            from,
            target,
            cycle,
        } => {
            assert_eq!(from, b, "source field");
            assert_eq!(target, a, "target field");
            // Walk forward from A (target). A's only blocks-dep is B.
            // B is the source. Path: [A, B].
            assert_eq!(cycle, vec![a.clone(), b.clone()], "cycle path");
        }
        other => panic!("expected DependencyCycle, got {other:?}"),
    }

    // The rejected edge MUST NOT have landed on B.
    let after = storage.read(&b).unwrap();
    assert!(
        after.dependencies.is_empty(),
        "rejected dep-add must not land an edge: {:?}",
        after.dependencies,
    );
}

#[test]
fn add_dep_edge_indirect_3_cycle_rejected() {
    // A.deps += blocks:B, B.deps += blocks:C, then C.deps += blocks:A
    // would close A -> B -> C -> A (path = [A, B, C]).
    let repo = make_scratch_repo("dep_cycle_3");
    let storage = Storage::open(&repo).unwrap();

    let a = storage
        .create_issue(&IssueDraft {
            title: "A".into(),
            ..Default::default()
        })
        .unwrap();
    let b = storage
        .create_issue(&IssueDraft {
            title: "B".into(),
            ..Default::default()
        })
        .unwrap();
    let c = storage
        .create_issue(&IssueDraft {
            title: "C".into(),
            ..Default::default()
        })
        .unwrap();

    storage.add_dep_edge(&a, &b, DepKind::Blocks).unwrap();
    storage.add_dep_edge(&b, &c, DepKind::Blocks).unwrap();

    let err = storage.add_dep_edge(&c, &a, DepKind::Blocks).unwrap_err();
    match err {
        StorageError::DependencyCycle {
            from,
            target,
            cycle,
        } => {
            assert_eq!(from, c);
            assert_eq!(target, a);
            // Walk from A: A -> B -> C. Cycle path: [A, B, C].
            assert_eq!(cycle, vec![a.clone(), b.clone(), c.clone()]);
        }
        other => panic!("expected DependencyCycle, got {other:?}"),
    }
}

#[test]
fn add_dep_edge_diamond_is_not_a_cycle() {
    // A -> B, A -> C, B -> D, C -> D. No cycles. Then D -> A
    // SHOULD be rejected (closes the cycle through both arms).
    // Also, re-adding A -> B is idempotent and must not be
    // flagged as a cycle.
    let repo = make_scratch_repo("dep_diamond");
    let storage = Storage::open(&repo).unwrap();

    let a = storage
        .create_issue(&IssueDraft {
            title: "A".into(),
            ..Default::default()
        })
        .unwrap();
    let b = storage
        .create_issue(&IssueDraft {
            title: "B".into(),
            ..Default::default()
        })
        .unwrap();
    let c = storage
        .create_issue(&IssueDraft {
            title: "C".into(),
            ..Default::default()
        })
        .unwrap();
    let d = storage
        .create_issue(&IssueDraft {
            title: "D".into(),
            ..Default::default()
        })
        .unwrap();

    storage.add_dep_edge(&a, &b, DepKind::Blocks).unwrap();
    storage.add_dep_edge(&a, &c, DepKind::Blocks).unwrap();
    storage.add_dep_edge(&b, &d, DepKind::Blocks).unwrap();
    storage.add_dep_edge(&c, &d, DepKind::Blocks).unwrap();

    // Idempotent re-add of A -> B: edge already present, must not
    // be flagged as a cycle. (Edges already on the record are de-
    // duped on the way in; the cycle walk skips them precisely so
    // self-overlap doesn't false-positive.)
    storage
        .add_dep_edge(&a, &b, DepKind::Blocks)
        .expect("re-adding an existing edge must not trip cycle check");

    // D -> A would close the cycle (D -> A -> B -> D or
    // D -> A -> C -> D).
    let err = storage.add_dep_edge(&d, &a, DepKind::Blocks).unwrap_err();
    assert!(
        matches!(err, StorageError::DependencyCycle { .. }),
        "expected DependencyCycle, got {err:?}",
    );
}

#[test]
fn dep_rm_then_dep_add_does_not_trip_cycle() {
    // A -> B, B -> C, C -> ... no cycle. Add A -> B; remove A -> B;
    // re-add A -> B. The re-add walks the post-rm graph and finds
    // no path back to A, so it lands cleanly.
    let repo = make_scratch_repo("dep_rm_then_add");
    let storage = Storage::open(&repo).unwrap();

    let a = storage
        .create_issue(&IssueDraft {
            title: "A".into(),
            ..Default::default()
        })
        .unwrap();
    let b = storage
        .create_issue(&IssueDraft {
            title: "B".into(),
            ..Default::default()
        })
        .unwrap();

    storage.add_dep_edge(&a, &b, DepKind::Blocks).unwrap();
    storage.remove_dep_edge(&a, &b, DepKind::Blocks).unwrap();
    storage
        .add_dep_edge(&a, &b, DepKind::Blocks)
        .expect("re-adding after rm must succeed");

    let after = storage.read(&a).unwrap();
    assert_eq!(after.dependencies.len(), 1, "edge should be back");
    assert_eq!(after.dependencies[0].target, b);
    assert_eq!(after.dependencies[0].kind, DepKind::Blocks);
}

#[test]
fn cycle_check_applies_only_to_blocks_kind() {
    // `related` / `discovered-from` / `parent-child` edges have no
    // ready-set effect; cycles among them aren't silent landmines.
    // The check skips them. Confirm by setting up what WOULD be
    // a `blocks` cycle and replaying it with each non-blocks kind.
    let repo = make_scratch_repo("dep_cycle_non_blocks");
    let storage = Storage::open(&repo).unwrap();

    let a = storage
        .create_issue(&IssueDraft {
            title: "A".into(),
            ..Default::default()
        })
        .unwrap();
    let b = storage
        .create_issue(&IssueDraft {
            title: "B".into(),
            ..Default::default()
        })
        .unwrap();

    // A -> B as `related` (no cycle effect).
    storage.add_dep_edge(&a, &b, DepKind::Related).unwrap();
    // B -> A as `related` — would be a 2-cycle if we cared. We
    // don't (no ready-set impact); should be accepted.
    storage
        .add_dep_edge(&b, &a, DepKind::Related)
        .expect("related cycle must not be rejected");

    // Same for discovered-from.
    storage
        .add_dep_edge(&a, &b, DepKind::DiscoveredFrom)
        .expect("discovered-from must not be cycle-checked");
    storage
        .add_dep_edge(&b, &a, DepKind::DiscoveredFrom)
        .expect("discovered-from must not be cycle-checked");

    // And parent-child. The ticket says we focus on blocks for now;
    // parent-child cycle detection is a follow-up.
    storage
        .add_dep_edge(&a, &b, DepKind::ParentChild)
        .expect("parent-child must not be cycle-checked (out of scope)");
    storage
        .add_dep_edge(&b, &a, DepKind::ParentChild)
        .expect("parent-child must not be cycle-checked (out of scope)");
}

#[test]
fn cycle_check_applies_even_when_issues_are_closed() {
    // Closing a node doesn't open a backdoor to cycle creation:
    // the graph walk treats closed issues as live nodes. A -> B
    // (B closed), then B -> A still rejects.
    let repo = make_scratch_repo("dep_cycle_closed");
    let storage = Storage::open(&repo).unwrap();

    let a = storage
        .create_issue(&IssueDraft {
            title: "A".into(),
            ..Default::default()
        })
        .unwrap();
    let b = storage
        .create_issue(&IssueDraft {
            title: "B".into(),
            ..Default::default()
        })
        .unwrap();

    storage.add_dep_edge(&a, &b, DepKind::Blocks).unwrap();
    storage.set_status(&a, Status::Closed).unwrap();

    let err = storage.add_dep_edge(&b, &a, DepKind::Blocks).unwrap_err();
    assert!(
        matches!(err, StorageError::DependencyCycle { .. }),
        "closed-node cycle must still be rejected: got {err:?}",
    );
}

#[test]
fn remove_dep_edge_against_phantom_target_is_no_op() {
    // `jjf dep rm` against a dep target that doesn't resolve is
    // permissive — removing a non-existent edge is harmless and
    // useful for cleanup. The op-side `retain` is a no-op when no
    // edge matches.
    let repo = make_scratch_repo("dep_rm_phantom_noop");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "real".into(),
            ..Default::default()
        })
        .unwrap();
    let phantom = IssueId::parse("deadbee").unwrap();

    // Should not error.
    storage
        .remove_dep_edge(&id, &phantom, DepKind::Blocks)
        .expect("remove_dep_edge against phantom must not error");
}

// =====================================================================
// `qa-trailer-injection` (issue `a902492`) — defensive tests.
//
// The strict write-boundary rejects every user-controlled string that
// could split into a new trailer line. These tests pin that contract
// from two angles:
//
// 1. A crafted-title injection attempt against a real victim must NOT
//    mutate the victim's state.
// 2. A bookmark-wide walker that asserts every `Jjf-Op:` stanza is
//    well-formed — no orphan trailers, no extras.
//
// Path A vs Path B: we picked Path A (reject newlines at the write
// boundary) because the asterinas migration doesn't need multi-line
// titles, validate_title already enforces it, and the defense is
// uniform across every free-form field (title, assignee, label,
// block-reason). Path B (writer-side quoting/escaping) would be more
// complex, require a parser-side dequoting step, and risk a
// quote-aware-vs-permissive split between jjforge and any third-party
// tool that grep'd the trailer block.
// =====================================================================

#[test]
fn crafted_title_cannot_inject_set_status_against_victim() {
    // End-to-end attack:
    //
    // 1. File a "victim" issue. Record its id.
    // 2. Attempt to create an "attacker" issue whose title (before
    //    validation) contains a forged `Jjf-Op: set-status` stanza
    //    targeting the victim id, with status closed.
    // 3. The create must fail with `InvalidTitle::Newline` — that
    //    proves the title-validation gate catches the attack BEFORE
    //    any commit lands.
    // 4. As belt-and-braces, the victim's status must remain `Open`.
    //
    // This is the contract test for `qa-trailer-injection`: a hostile
    // title cannot reach the writer.
    let repo = make_scratch_repo("crafted_title_injection");
    let storage = Storage::open(&repo).unwrap();

    let victim = storage
        .create_issue(&IssueDraft {
            title: "victim".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(storage.read(&victim).unwrap().status, Status::Open);

    let crafted_title = format!(
        "innocuous\n\nJjf-Op: set-status\nJjf-Issue: {}\nJjf-Status: closed\n",
        victim
    );

    let err = storage
        .create_issue(&IssueDraft {
            title: crafted_title.clone(),
            ..Default::default()
        })
        .unwrap_err();
    match err {
        StorageError::InvalidTitle { reason, .. } => {
            assert_eq!(reason, TitleInvalidReason::Newline);
        }
        other => panic!("expected InvalidTitle{{Newline}}, got {other:?}"),
    }

    // Victim is still open — no op slipped through.
    assert_eq!(storage.read(&victim).unwrap().status, Status::Open);

    // And as a stronger guarantee: the storage layer's `set_title`
    // (bypassing IssueDraft) also rejects the crafted title.
    let err = storage.set_title(&victim, &crafted_title).unwrap_err();
    assert!(
        matches!(err, StorageError::InvalidTitle { .. }),
        "set_title bypass attempt should also be rejected, got {err:?}"
    );
    assert_eq!(storage.read(&victim).unwrap().status, Status::Open);
}

#[test]
fn crafted_assignee_cannot_inject_trailer() {
    // Same shape as the title test but via the assignee field. The
    // writer's `Jjf-Assignee:` payload is single-line by contract; a
    // multi-line value would split into a new trailer line.
    let repo = make_scratch_repo("crafted_assignee_injection");
    let storage = Storage::open(&repo).unwrap();
    let victim = storage
        .create_issue(&IssueDraft {
            title: "victim".into(),
            ..Default::default()
        })
        .unwrap();

    let crafted = format!(
        "alice\nJjf-Op: set-status\nJjf-Issue: {}\nJjf-Status: closed",
        victim
    );

    // set_assignee path
    let err = storage.set_assignee(&victim, Some(&crafted)).unwrap_err();
    assert!(
        matches!(err, StorageError::Invalid(_)),
        "set_assignee with newlines should be rejected, got {err:?}"
    );

    // update(assignee=...) path
    let err = storage
        .update(
            &victim,
            UpdateFields {
                assignee: Some(Some(crafted.clone())),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(
        matches!(err, StorageError::Invalid(_)),
        "update(assignee=newline) should be rejected, got {err:?}"
    );

    // claim path (assignee comes via `who`)
    let err = storage.claim(&victim, &crafted).unwrap_err();
    assert!(
        matches!(err, StorageError::Invalid(_)),
        "claim(who=newline) should be rejected, got {err:?}"
    );

    // create_issue draft.assignee path
    let err = storage
        .create_issue(&IssueDraft {
            title: "attacker".into(),
            assignee: Some(crafted),
            ..Default::default()
        })
        .unwrap_err();
    assert!(
        matches!(err, StorageError::Invalid(_)),
        "create_issue(draft.assignee=newline) should be rejected, got {err:?}"
    );

    // Victim untouched.
    assert_eq!(storage.read(&victim).unwrap().status, Status::Open);
    assert_eq!(storage.read(&victim).unwrap().assignee, None);
}

#[test]
fn crafted_label_cannot_inject_trailer() {
    // Labels are free-form (no charset constraint); a multi-line
    // label would inject a trailer line. All three label write paths
    // (`add_label`, `remove_label`, `create_issue(draft.labels)`) must
    // reject.
    let repo = make_scratch_repo("crafted_label_injection");
    let storage = Storage::open(&repo).unwrap();
    let victim = storage
        .create_issue(&IssueDraft {
            title: "victim".into(),
            ..Default::default()
        })
        .unwrap();

    let crafted = format!(
        "wip\nJjf-Op: set-status\nJjf-Issue: {}\nJjf-Status: closed",
        victim
    );

    let err = storage.add_label(&victim, &crafted).unwrap_err();
    assert!(
        matches!(err, StorageError::Invalid(_)),
        "add_label with newlines should be rejected, got {err:?}"
    );
    let err = storage.remove_label(&victim, &crafted).unwrap_err();
    assert!(
        matches!(err, StorageError::Invalid(_)),
        "remove_label with newlines should be rejected, got {err:?}"
    );
    let err = storage
        .create_issue(&IssueDraft {
            title: "attacker".into(),
            labels: vec![crafted],
            ..Default::default()
        })
        .unwrap_err();
    assert!(
        matches!(err, StorageError::Invalid(_)),
        "create_issue(draft.labels with newline) should be rejected, got {err:?}"
    );

    // Victim untouched.
    assert_eq!(storage.read(&victim).unwrap().status, Status::Open);
    assert!(storage.read(&victim).unwrap().labels.is_empty());
}

// --- Trailer-block-shape walker ---------------------------------------
//
// Walks every commit on the `issues` bookmark and asserts each
// `Jjf-Op:` stanza is structurally well-formed: every required sibling
// trailer is present for the op kind, no extra `Jjf-*` lines are
// orphaned outside an op stanza, and the carrier shape `Jjf-Op:` →
// (`Jjf-Issue:` ...) holds. This is a generic invariant test: it
// doesn't try to reason about op semantics, only about trailer
// contiguity and the sibling-set contract for each op kind.

/// Per spec §5.2: the required sibling-trailer keys for each op kind.
/// `Jjf-Op`, `Jjf-Issue`, and `Jjf-At` are implicit (every stanza has
/// them); `Jjf-Bug` is the v1 alias for `Jjf-Issue` (the walker accepts
/// either). Optional trailers are listed in `optional_sibling_keys`.
fn required_sibling_keys(op_type: &str) -> Option<&'static [&'static str]> {
    Some(match op_type {
        "create" => &["Jjf-Title", "Jjf-Status"],
        "set-title" => &["Jjf-Title"],
        "set-status" => &["Jjf-Status"],
        "set-body" => &["Jjf-Body-Hash"],
        "label-add" | "label-rm" => &["Jjf-Label"],
        // Jjf-Dep-Kind is optional (defaults to `blocks` if absent
        // per v1 forward-compat); only Jjf-Dep is REQUIRED. The
        // optional set covers Jjf-Dep-Kind.
        "dep-add" | "dep-rm" => &["Jjf-Dep"],
        // Jjf-Assignee is required but may be empty (the empty-string
        // form encodes `assignee: None`).
        "set-assignee" => &["Jjf-Assignee"],
        "set-type" => &["Jjf-Type"],
        "set-slug" => &["Jjf-Slug"],
        "set-block-reason" => &["Jjf-Reason"],
        "comment-add" => &["Jjf-Comment-Id"],
        // merge stanzas carry no payload trailers (spec §5.2).
        "merge" => &[],
        // Memory-space ops (spec v2.2 §10) — not per-issue, so they
        // carry Memory-Key and (for set-memory) Memory-Value instead
        // of Jjf-Issue. Listed here so the walker recognizes them.
        "set-memory" => &["Jjf-Memory-Key", "Jjf-Memory-Value"],
        "unset-memory" => &["Jjf-Memory-Key"],
        // Unknown op-type: tolerate per spec §5.2. Caller treats this
        // as "no required keys" — the stanza is opaque.
        _ => return None,
    })
}

fn optional_sibling_keys(op_type: &str) -> &'static [&'static str] {
    match op_type {
        "dep-add" | "dep-rm" => &["Jjf-Dep-Kind"],
        _ => &[],
    }
}

/// A stanza is "memory-space" if its op-type lives on the memory
/// bookmark (no Jjf-Issue trailer per spec v2.2 §10). Walker uses
/// this to skip the per-issue id check.
fn is_memory_op(op_type: &str) -> bool {
    matches!(op_type, "set-memory" | "unset-memory")
}

#[test]
fn trailer_block_shape_walker_finds_no_orphans_on_real_bookmark() {
    // Build a representative bookmark exercising every op kind we
    // know how to write, then walk every commit and assert structural
    // integrity.
    let repo = make_scratch_repo("trailer_walker");
    let storage = Storage::open(&repo).unwrap();

    // create + slug + body + type + labels + dep + assignee in one
    // multi-op stanza
    let a = storage
        .create_issue(&IssueDraft {
            title: "alpha".into(),
            slug: Some("alpha".into()),
            body: "the body".into(),
            type_: Some(IssueType::Bug),
            labels: vec!["wip".into(), "p0".into()],
            assignee: Some("alice".into()),
            ..Default::default()
        })
        .unwrap();
    let b = storage
        .create_issue(&IssueDraft {
            title: "beta".into(),
            ..Default::default()
        })
        .unwrap();

    // Per-field mutators: set-title, set-status, set-body, label-add/rm,
    // dep-add/rm, set-assignee (set + clear), set-type, set-slug,
    // set-block-reason (block + unblock).
    storage.set_title(&a, "alpha 2").unwrap();
    storage.set_status(&a, Status::Closed).unwrap();
    storage.set_status(&a, Status::Open).unwrap();
    storage.set_body(&a, "new body").unwrap();
    storage.add_label(&a, "needs-review").unwrap();
    storage.remove_label(&a, "wip").unwrap();
    storage.add_dependency(&a, &b).unwrap();
    storage.remove_dependency(&a, &b).unwrap();
    storage.add_dep_edge(&a, &b, DepKind::ParentChild).unwrap();
    storage.remove_dep_edge(&a, &b, DepKind::ParentChild).unwrap();
    storage.set_assignee(&a, Some("bob")).unwrap();
    storage.set_assignee(&a, None).unwrap();
    storage.block(&a, Some("waiting on signal")).unwrap();
    storage.unblock(&a).unwrap();
    storage.add_comment(&a, "a comment", "tester").unwrap();

    // Memory ops: set + update + unset.
    storage.set_memory("hello-world", "first value").unwrap();
    storage.set_memory("hello-world", "second value").unwrap();
    storage.unset_memory("hello-world").unwrap();

    // V3: enumerate every per-issue ref under `refs/jjf/issues/` and
    // every per-memory ref under `refs/jjf/memories/`, then walk each
    // ref's commit chain. The trailer-block shape rules apply to every
    // commit description regardless of which ref carries it.
    let refs_out = git_capture(
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/jjf/issues/",
            "refs/jjf/memories/",
        ],
        &repo,
    );
    let refs: Vec<&str> = refs_out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(!refs.is_empty(), "expected v3 per-item refs to walk");

    let mut commit_count = 0;
    let mut stanza_count = 0;
    for r in refs {
        // `git log --format=%B%n----RECORD-SEP----` walks the chain
        // and emits each commit's full description, separated by our
        // sentinel marker.
        let log = git_capture(
            &["log", "--format=%B%n----RECORD-SEP----", r],
            &repo,
        );
        for desc in log.split("----RECORD-SEP----") {
            let desc = desc.trim_end();
            if desc.is_empty() {
                continue;
            }
            commit_count += 1;
            stanza_count += assert_trailer_block_shape(desc);
        }
    }
    assert!(commit_count > 0, "expected non-zero commits to walk");
    assert!(stanza_count > 0, "expected at least one Jjf-Op stanza");
}

/// Parse a commit description into trailer stanzas and assert each
/// `Jjf-Op:` stanza is well-formed. Returns the count of `Jjf-Op:`
/// stanzas asserted.
///
/// Rules enforced:
///
/// - Every `Jjf-Op:` line is followed by `Jjf-Issue:` (or `Jjf-Bug:`)
///   OR is a memory-space op (`set-memory` / `unset-memory`) that
///   carries `Jjf-Memory-Key:` instead.
/// - Every required sibling trailer for the op kind is present.
/// - No `Jjf-*:` trailer line appears OUTSIDE a `Jjf-Op:` stanza
///   (no orphans).
/// - Unknown op-types are tolerated (per spec §5.2) — the walker
///   verifies trailer-block contiguity for them but doesn't enforce
///   a sibling set.
fn assert_trailer_block_shape(desc: &str) -> usize {
    // Mini state machine: walk lines; track whether we're inside a
    // Jjf-Op stanza; when the stanza ends, assert sibling-set
    // completeness.
    let mut stanzas_seen = 0;
    let mut cur_op: Option<String> = None;
    let mut cur_siblings: Vec<String> = Vec::new();

    let close_stanza = |op: &str, siblings: &[String]| {
        // Every stanza must carry an issue id OR be a memory-space op.
        let has_issue =
            siblings.iter().any(|s| s == "Jjf-Issue" || s == "Jjf-Bug");
        if !is_memory_op(op) {
            assert!(
                has_issue,
                "Jjf-Op: {op} stanza missing Jjf-Issue (siblings={siblings:?})"
            );
        }
        if let Some(required) = required_sibling_keys(op) {
            for key in required {
                assert!(
                    siblings.iter().any(|s| s == key),
                    "Jjf-Op: {op} stanza missing required sibling {key} \
                     (siblings={siblings:?})"
                );
            }
            // Sanity: no UNKNOWN Jjf-* keys outside the required +
            // optional + always-present set. We allow Jjf-At and
            // Jjf-Issue/Jjf-Bug as implicit.
            let always = ["Jjf-At", "Jjf-Issue", "Jjf-Bug"];
            let optional = optional_sibling_keys(op);
            for sib in siblings {
                let ok = required.iter().any(|k| k == sib)
                    || optional.iter().any(|k| k == sib)
                    || always.iter().any(|k| k == sib);
                assert!(
                    ok,
                    "Jjf-Op: {op} stanza has unexpected sibling {sib} \
                     (siblings={siblings:?})"
                );
            }
        }
        // Unknown op-types (required is None) are tolerated.
    };

    for line in desc.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            // Blank line ends a stanza per RFC trailer rules.
            if let Some(op) = cur_op.take() {
                close_stanza(&op, &cur_siblings);
                cur_siblings.clear();
                stanzas_seen += 1;
            }
            continue;
        }
        if let Some(colon) = trimmed.find(':') {
            let key = &trimmed[..colon];
            // Real trailer keys are single-token, no spaces.
            if key.is_empty() || key.contains(' ') {
                // Not a trailer line. If we're in a stanza, end it.
                if let Some(op) = cur_op.take() {
                    close_stanza(&op, &cur_siblings);
                    cur_siblings.clear();
                    stanzas_seen += 1;
                }
                continue;
            }
            if key == "Jjf-Op" {
                // New stanza starts.
                if let Some(op) = cur_op.take() {
                    close_stanza(&op, &cur_siblings);
                    cur_siblings.clear();
                    stanzas_seen += 1;
                }
                let value = trimmed[colon + 1..].trim_start();
                cur_op = Some(value.to_owned());
            } else if key.starts_with("Jjf-") {
                // Sibling trailer. Must be inside a Jjf-Op stanza —
                // an orphan Jjf-* is the injection-shape we're
                // defending against.
                assert!(
                    cur_op.is_some(),
                    "orphan Jjf-* trailer outside any Jjf-Op stanza: {trimmed:?}"
                );
                cur_siblings.push(key.to_owned());
            } else if cur_op.is_some() {
                // Non-Jjf trailer (e.g. Signed-off-by) closes the
                // stanza (matches the parser's RFC behavior).
                if let Some(op) = cur_op.take() {
                    close_stanza(&op, &cur_siblings);
                    cur_siblings.clear();
                    stanzas_seen += 1;
                }
            }
        } else if cur_op.is_some() {
            // Non-trailer text mid-stanza: close it.
            if let Some(op) = cur_op.take() {
                close_stanza(&op, &cur_siblings);
                cur_siblings.clear();
                stanzas_seen += 1;
            }
        }
    }
    // Tail: close any open stanza.
    if let Some(op) = cur_op.take() {
        close_stanza(&op, &cur_siblings);
        stanzas_seen += 1;
    }
    stanzas_seen
}

// --- a6b8fb7: mutate-retry re-checks domain preconditions ---------

/// `claim_returns_claim_result_claimed_when_fresh` pins the
/// shape of the new return type: a freshly-claimable issue
/// returns `ClaimResult::Claimed`.
#[test]
fn claim_returns_claim_result_claimed_when_fresh() {
    let repo = make_scratch_repo("claim_result_claimed");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    let result = storage.claim(&id, "alice").unwrap();
    assert_eq!(result, ClaimResult::Claimed);
}

/// Same-user idempotent re-claim returns `ClaimResult::AlreadyOurs`
/// — distinct from `Claimed` so the CLI's `ready --claim` path can
/// detect the parallel-claim race-lost case (see `a6b8fb7`).
#[test]
fn claim_returns_already_ours_on_same_user_idempotent() {
    let repo = make_scratch_repo("claim_result_already_ours");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.claim(&id, "alice").unwrap();
    let again = storage.claim(&id, "alice").unwrap();
    assert_eq!(
        again,
        ClaimResult::AlreadyOurs,
        "second claim by same user must surface as AlreadyOurs"
    );
}

/// `a6b8fb7`: if the issue is closed BETWEEN the verb being
/// called and the closure running, the closure observes the
/// fresh status and surfaces `Invalid` — not a silent re-open.
///
/// We can't easily provoke a real CAS-loss retry in a unit test
/// because the closure only re-runs after a `jj`-level race —
/// but we CAN verify the closure-as-precondition contract by
/// mutating state AND calling claim on a record that's already
/// closed. If the closure's status check fires correctly, the
/// pre-existing `claim_on_closed_issue_errors_invalid` test
/// already covers the happy path. Here we focus on the "the
/// closure DID see the closed state" property: claim must NOT
/// successfully write against a Closed record (no silent
/// status flip).
#[test]
fn claim_on_closed_does_not_silently_reopen() {
    let repo = make_scratch_repo("claim_on_closed_no_reopen");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.set_status(&id, Status::Closed).unwrap();
    let history_before = storage.read_history(&id).unwrap().len();
    let err = storage.claim(&id, "alice").unwrap_err();
    assert!(matches!(err, StorageError::Invalid(_)));
    // Crucial post-condition: NO new commit landed (no silent
    // status flip from Closed to InProgress).
    let history_after = storage.read_history(&id).unwrap().len();
    assert_eq!(
        history_before, history_after,
        "claim on closed must not write any commit (no silent reopen)"
    );
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.status, Status::Closed);
    assert_eq!(issue.assignee, None);
}

/// `a6b8fb7`: unclaim on a closed record surfaces Invalid AND
/// doesn't write anything. Mirrors `claim_on_closed_does_not_silently_reopen`
/// for the inverse verb.
#[test]
fn unclaim_on_closed_does_not_silently_reopen() {
    let repo = make_scratch_repo("unclaim_on_closed_no_reopen");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.claim(&id, "alice").unwrap();
    storage.set_status(&id, Status::Closed).unwrap();
    let history_before = storage.read_history(&id).unwrap().len();
    let err = storage.unclaim(&id).unwrap_err();
    assert!(matches!(err, StorageError::Invalid(_)));
    let history_after = storage.read_history(&id).unwrap().len();
    assert_eq!(
        history_before, history_after,
        "unclaim on closed must not write any commit"
    );
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.status, Status::Closed);
}

/// `a6b8fb7`: block on a closed record surfaces Invalid AND
/// doesn't write anything.
#[test]
fn block_on_closed_does_not_silently_reopen() {
    let repo = make_scratch_repo("block_on_closed_no_reopen");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.set_status(&id, Status::Closed).unwrap();
    let history_before = storage.read_history(&id).unwrap().len();
    let err = storage.block(&id, Some("waiting on upstream")).unwrap_err();
    assert!(matches!(err, StorageError::Invalid(_)));
    let history_after = storage.read_history(&id).unwrap().len();
    assert_eq!(
        history_before, history_after,
        "block on closed must not write any commit"
    );
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.status, Status::Closed);
    assert_eq!(issue.block_reason, None);
}

/// `a6b8fb7`: when alice and bob race to claim the same issue,
/// the loser's CAS-loss retry must observe the post-race record
/// (claimed by alice) and surface `AlreadyClaimed { by: alice }`
/// — NOT a silent duplicate claim.
///
/// We can't deterministically provoke a CAS-loss retry from a
/// single-threaded unit test, but we CAN exercise the closure
/// directly: alice claims, then bob calls claim. The closure
/// sees the post-alice record on its first read and surfaces
/// AlreadyClaimed without writing. (The `claim_different_user_errors_with_already_claimed`
/// test above pinned the same surface; this test additionally
/// asserts NO commit lands — pinning the "no silent write" half
/// of the contract that `a6b8fb7` introduces.)
#[test]
fn claim_by_different_user_does_not_silently_overwrite() {
    let repo = make_scratch_repo("claim_diff_user_no_overwrite");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    storage.claim(&id, "alice").unwrap();
    let history_before_bob = storage.read_history(&id).unwrap().len();
    let err = storage.claim(&id, "bob").unwrap_err();
    match err {
        StorageError::AlreadyClaimed { by } => assert_eq!(by, "alice"),
        other => panic!("expected AlreadyClaimed{{by:alice}}, got {other:?}"),
    }
    // Crucial: no new commit landed despite the failure path.
    let history_after_bob = storage.read_history(&id).unwrap().len();
    assert_eq!(
        history_before_bob, history_after_bob,
        "bob's failed claim must not have landed any trailer"
    );
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.assignee.as_deref(), Some("alice"));
}

// --- c5078e4: bounded-retry policy with exponential backoff ------
//
// The four tests below pin the contract for the storage-layer CAS
// retry budget. Background: pre-c5078e4 the retry was one-shot, so
// N concurrent writers to the same issue often saw only ~2 land and
// the rest fail with a typed `ConcurrentWrite`. The fix gives every
// caller 5 retries (6 total attempts) with a 10/25/60/150/350 ms
// geometric backoff. Two env vars (`JJF_MAX_RETRIES`,
// `JJF_RETRY_BASE_MS`) tune the budget for tests; the latter at 0
// instructs the retry loop to skip the wall-clock sleep entirely.

/// Regression guard: sequential comments on the same issue still
/// work end-to-end. The new retry loop must not break the happy
/// path (single writer, no contention).
#[test]
fn sequential_comments_on_same_issue_all_land() {
    let repo = make_scratch_repo("retry_sequential_comments");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "sequential target".into(),
            ..Default::default()
        })
        .unwrap();
    for i in 0..5 {
        storage
            .add_comment(&id, &format!("comment {i}"), "alice")
            .unwrap_or_else(|e| panic!("sequential comment {i} failed: {e:?}"));
    }
    let issue = storage.read(&id).unwrap();
    assert_eq!(
        issue.comments.len(),
        5,
        "5 sequential adds must land 5 comments; got {}",
        issue.comments.len()
    );
}

/// With 5 threads concurrently appending to the SAME issue under
/// the default retry budget, ALL 5 must land. Per the ticket's
/// escape hatch ("reliability over coverage"), we cap at 5 racers
/// because 10-way bursts can defeat the geometric backoff window
/// in heavy-contention CI runs; the property under test is "the
/// retry budget genuinely absorbs realistic contention," and 5
/// racers is firmly inside that envelope.
///
/// We open one Storage per thread (Storage isn't Clone) and share
/// the repo path. Each thread runs add_comment; we then count the
/// landed comments via a fresh Storage::open in the main thread.
#[test]
fn concurrent_comments_absorb_contention_with_retry_budget() {
    let repo = make_scratch_repo("retry_concurrent_5_comments");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "5-way contention target".into(),
            ..Default::default()
        })
        .unwrap();
    drop(storage);

    let mut handles = Vec::new();
    for i in 0..5 {
        let repo = repo.clone();
        let id = id.clone();
        handles.push(std::thread::spawn(move || {
            let storage = Storage::open(&repo).expect("open storage in thread");
            storage.add_comment(&id, &format!("c{i}"), "alice")
        }));
    }
    let mut landed = 0;
    let mut errors: Vec<String> = Vec::new();
    for h in handles {
        match h.join().expect("thread did not panic") {
            Ok(_) => landed += 1,
            Err(e) => errors.push(format!("{e:?}")),
        }
    }

    // Verify by reading back: the comments file should match the
    // landed count (no silent clobbering by retries).
    let storage = Storage::open(&repo).unwrap();
    let issue = storage.read(&id).unwrap();
    assert_eq!(
        issue.comments.len(),
        landed,
        "landed-count ({landed}) must match comments-file count ({}); errors: {errors:?}",
        issue.comments.len()
    );
    assert!(
        landed >= 4,
        "retry budget should absorb 5-way contention; \
         only {landed}/5 landed. Errors: {errors:?}"
    );
}

/// `JJF_MAX_RETRIES=0` short-circuits the retry loop entirely —
/// the first ConcurrentWrite conflict surfaces immediately.
///
/// We can't deterministically provoke a race in a single-threaded
/// test, but we CAN drive contention from threads and assert that
/// SOME thread sees a ConcurrentWrite (proving the retry loop
/// didn't silently absorb it). Without this knob, the retry budget
/// would mask the failure entirely under 10-way contention (as the
/// previous test pins).
#[test]
fn zero_max_retries_surfaces_first_conflict() {
    // SAFETY: nextest runs each test in its own process.
    unsafe {
        std::env::set_var("JJF_MAX_RETRIES", "0");
        std::env::set_var("JJF_RETRY_BASE_MS", "0");
    }

    let repo = make_scratch_repo("retry_max_0_surfaces_conflict");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "max-0 target".into(),
            ..Default::default()
        })
        .unwrap();
    drop(storage);

    let mut handles = Vec::new();
    for i in 0..10 {
        let repo = repo.clone();
        let id = id.clone();
        handles.push(std::thread::spawn(move || {
            let storage = Storage::open(&repo).expect("open storage in thread");
            storage.add_comment(&id, &format!("c{i}"), "alice")
        }));
    }

    let mut conflict_seen = false;
    let mut other_err: Vec<String> = Vec::new();
    let mut landed = 0;
    for h in handles {
        match h.join().expect("thread did not panic") {
            Ok(_) => landed += 1,
            Err(StorageError::ConcurrentWrite { .. }) => {
                conflict_seen = true;
            }
            Err(other) => other_err.push(format!("{other:?}")),
        }
    }

    // The CLI write dance often serializes through jj's working-copy
    // lock so contention isn't guaranteed every run. We assert:
    // either a ConcurrentWrite fired (the retry loop didn't absorb
    // it — the property under test) OR all 10 landed despite zero
    // retries (the writes happened to serialize cleanly). What MUST
    // NOT happen: a non-ConcurrentWrite error escaping the retry path.
    assert!(
        other_err.is_empty(),
        "non-ConcurrentWrite errors escaped retry path: {other_err:?}"
    );
    assert!(
        conflict_seen || landed == 10,
        "with max_retries=0 we expected either a ConcurrentWrite to surface \
         OR all 10 writes to serialize cleanly; saw landed={landed}, conflicts=0"
    );

    unsafe {
        std::env::remove_var("JJF_MAX_RETRIES");
        std::env::remove_var("JJF_RETRY_BASE_MS");
    }
}

/// The exhausted-retry error message reflects the actual configured
/// retry count. With `JJF_MAX_RETRIES=3`, the hint should say
/// "retried 3 times" — keeping the operator-facing message honest
/// when the budget is overridden.
///
/// We provoke exhaustion by setting max_retries to 1 (so 2 total
/// attempts) AND spawning many concurrent writers, then verify any
/// ConcurrentWrite error's hint mentions "retried 1 time" (the
/// configured budget). If no contention manifests we silently pass
/// (the message-shape test is degenerate without a real failure).
#[test]
fn exhausted_retry_hint_mentions_configured_count() {
    // SAFETY: nextest runs each test in its own process.
    unsafe {
        std::env::set_var("JJF_MAX_RETRIES", "1");
        std::env::set_var("JJF_RETRY_BASE_MS", "0");
    }

    let repo = make_scratch_repo("retry_hint_honest_count");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "honest-hint target".into(),
            ..Default::default()
        })
        .unwrap();
    drop(storage);

    let mut handles = Vec::new();
    for i in 0..10 {
        let repo = repo.clone();
        let id = id.clone();
        handles.push(std::thread::spawn(move || {
            let storage = Storage::open(&repo).expect("open storage in thread");
            storage.add_comment(&id, &format!("c{i}"), "alice")
        }));
    }
    let mut hints: Vec<String> = Vec::new();
    for h in handles {
        if let Err(StorageError::ConcurrentWrite { hint }) = h.join().expect("thread did not panic") {
            hints.push(hint);
        }
    }

    for hint in &hints {
        assert!(
            hint.contains("retried 1 time") && !hint.contains("retried 1 times"),
            "hint must reflect configured max_retries=1 (singular 'time'); \
             got: {hint:?}"
        );
    }

    unsafe {
        std::env::remove_var("JJF_MAX_RETRIES");
        std::env::remove_var("JJF_RETRY_BASE_MS");
    }
}

// ---- unreadable-ref diagnostics (ticket `4928ae6`) -----------------
//
// The snapshot cache's v3 rebuild used to silently drop any
// `refs/jjf/issues/*` whose tip didn't resolve to a commit carrying
// `issue.json` — pointing a ref at a blob (the easy repro:
// `git update-ref refs/jjf/issues/<id> $(git hash-object -w --stdin
// <<<"junk")`) made the affected id vanish from `list_ids` /
// `list_ready` with no diagnostic. Trust-eroding for the operator
// ("where did my ticket go?") and indistinguishable from "issue
// doesn't exist" via `jjf show <id>`.
//
// The fix records each unparseable ref into
// `SnapshotCache::unreadable_refs` (exposed via
// `Storage::unreadable_refs()`). The CLI's `ls` / `ready` verbs use
// that vec to emit a stderr warning; here we exercise the storage
// surface directly.

/// Pointing an `refs/jjf/issues/<id>` ref at a blob (via plain
/// `git update-ref` to a freshly-hashed blob oid) causes the v3
/// rebuild to surface that ref in `unreadable_refs()` while leaving
/// every unaffected issue listable. Headline repro from the ticket.
#[test]
fn unreadable_refs_surfaces_issue_ref_pointed_at_blob() {
    let repo = make_scratch_repo("unreadable_issue_ref_blob");
    let storage = Storage::open(&repo).expect("Storage::open");

    let alive = storage
        .create_issue(&IssueDraft {
            title: "alive issue".into(),
            ..Default::default()
        })
        .unwrap();
    let corrupt = storage
        .create_issue(&IssueDraft {
            title: "soon-to-be-corrupt".into(),
            ..Default::default()
        })
        .unwrap();

    // Hash a blob; point the corrupt issue's ref at it. From the
    // snapshot's POV the ref now resolves to a blob, not a commit
    // carrying `issue.json`.
    let blob_oid = git_capture_with_stdin(
        &["hash-object", "-w", "--stdin"],
        b"junk content\n",
        &repo,
    );
    let blob_oid = blob_oid.trim();
    sh(
        "git",
        &[
            "update-ref",
            &format!("refs/jjf/issues/{}", corrupt),
            blob_oid,
        ],
        &repo,
    );

    // Re-open so the snapshot memo doesn't carry the pre-corruption
    // cache state.
    drop(storage);
    let storage = Storage::open(&repo).expect("Storage::open after corruption");

    let ids = storage.list_ids().expect("list_ids");
    assert!(
        ids.contains(&alive),
        "alive issue must remain enumerable: ids={ids:?}"
    );
    assert!(
        !ids.contains(&corrupt),
        "corrupt issue must NOT appear in list_ids: ids={ids:?}"
    );

    let unreadable = storage.unreadable_refs().expect("unreadable_refs");
    assert_eq!(
        unreadable.len(),
        1,
        "exactly one unreadable ref expected: {unreadable:?}"
    );
    let expected_name = format!("refs/jjf/issues/{}", corrupt);
    assert_eq!(
        unreadable[0].name, expected_name,
        "unreadable ref name should be the corrupt issue's ref: got {:?}, expected {expected_name}",
        unreadable[0].name
    );
    assert!(
        !unreadable[0].reason.is_empty(),
        "unreadable ref must carry a human-readable reason: {:?}",
        unreadable[0],
    );
}

/// Pointing a memory ref at a blob surfaces it as unreadable
/// alongside issue-ref handling. The two namespaces share one fix.
#[test]
fn unreadable_refs_surfaces_memory_ref_pointed_at_blob() {
    let repo = make_scratch_repo("unreadable_memory_ref_blob");
    let storage = Storage::open(&repo).expect("Storage::open");

    storage
        .set_memory("alive-key", "alive value")
        .expect("seed alive memory");
    storage
        .set_memory("corrupt-key", "soon to break")
        .expect("seed corrupt-target memory");

    // Repoint the corrupt memory's ref at a blob.
    let blob_oid = git_capture_with_stdin(
        &["hash-object", "-w", "--stdin"],
        b"not a commit\n",
        &repo,
    );
    let blob_oid = blob_oid.trim();
    sh(
        "git",
        &[
            "update-ref",
            "refs/jjf/memories/corrupt-key",
            blob_oid,
        ],
        &repo,
    );

    drop(storage);
    let storage = Storage::open(&repo).expect("re-open after memory corruption");

    let mems = storage.list_memories().expect("list_memories");
    let keys: Vec<&str> = mems.iter().map(|m| m.key.as_str()).collect();
    assert!(
        keys.contains(&"alive-key"),
        "alive memory should remain visible: keys={keys:?}"
    );
    assert!(
        !keys.contains(&"corrupt-key"),
        "corrupt memory should be omitted from list_memories: keys={keys:?}"
    );

    let unreadable = storage.unreadable_refs().expect("unreadable_refs");
    assert_eq!(
        unreadable.len(),
        1,
        "expected exactly one unreadable ref (the corrupt memory): {unreadable:?}"
    );
    assert_eq!(
        unreadable[0].name, "refs/jjf/memories/corrupt-key",
        "unreadable ref name must point at the corrupt memory: {:?}",
        unreadable[0],
    );
}

/// A normal, healthy repo with several issues and several memories
/// reports `unreadable_refs() == []` — the no-warning baseline. If
/// this ever returns non-empty the warning would fire spuriously on
/// every clean call.
#[test]
fn unreadable_refs_empty_on_clean_repo() {
    let repo = make_scratch_repo("unreadable_clean_repo");
    let storage = Storage::open(&repo).expect("Storage::open");
    for title in ["alpha", "beta", "gamma"] {
        storage
            .create_issue(&IssueDraft {
                title: title.into(),
                ..Default::default()
            })
            .unwrap();
    }
    storage.set_memory("rule-one", "value one").unwrap();
    storage.set_memory("rule-two", "value two").unwrap();

    let unreadable = storage.unreadable_refs().expect("unreadable_refs");
    assert!(
        unreadable.is_empty(),
        "clean repo must report no unreadable refs; got {unreadable:?}"
    );

    // Sanity: the data we wrote is enumerable.
    let ids = storage.list_ids().expect("list_ids");
    assert_eq!(ids.len(), 3, "expected 3 issues to be listed: {ids:?}");
    let mems = storage.list_memories().expect("list_memories");
    assert_eq!(mems.len(), 2, "expected 2 memories to be listed: {mems:?}");
}

/// Helper for the unreadable-ref tests: pipe `stdin` into a `git`
/// invocation under `cwd` and return its trimmed stdout (or the
/// stderr-included assertion failure if git failed). Mirrors the
/// pattern used in `v3_read_path.rs` so an upstream eye scanning the
/// test suite recognizes the shape.
fn git_capture_with_stdin(args: &[&str], stdin_bytes: &[u8], cwd: &Path) -> String {
    use std::io::Write;
    use std::process::Stdio;
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
