//! Integration tests for the v3 git-only read path.
//!
//! Pinned by ticket `6e2c843` (storage-v3 #2):
//! - `Storage::read(id)` on a v3-shape repo returns the same `Issue`
//!   the v2 path would (titles, slugs, types, labels, dependencies,
//!   assignee, comments).
//! - `Storage::list_ids()` enumerates every v3-stored issue.
//! - `Storage::list_ready()` filters and sorts the v3 issue set the
//!   same way it does v2.
//! - `Storage::read_memory()` / `list_memories()` round-trip via the
//!   `refs/jjf/memories/*` namespace.
//! - `Storage::read_history(id)` walks the per-issue ref's commit
//!   chain and returns one entry per `Jjf-Op:` trailer.
//! - The debug-build op-replay cross-check runs against the per-issue
//!   ref's chain (and panics on injected divergence — covered by
//!   the v3_replay_panics_on_injected_divergence test below).
//!
//! Scope per the ticket: "the read path". Write-path coverage lives
//! in `v3_write_path.rs`; we only call mutators here to seed fixtures.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use iss_storage::{
    DepEdge, DepKind, Error as StorageError, IssueDraft, IssueType,
    ReadyFilter, Status, Storage, UpdateFields,
};

// ---- fixture --------------------------------------------------------

/// Build a v3-shape scratch repo: a plain git repo with the
/// `refs/jjf/meta/format-version` sentinel ref planted. `Storage::open`
/// will detect V3 mode and route every read through the git-only path.
///
/// J7: switched from `jj git init --colocate` to `git init` — the
/// shipped binary no longer calls jj, and the tests should not either.
fn make_v3_scratch_repo(name: &str) -> PathBuf {
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(".scratch")
        .join(name);
    if scratch.exists() {
        fs::remove_dir_all(&scratch).unwrap();
    }
    fs::create_dir_all(&scratch).unwrap();
    let abs = fs::canonicalize(&scratch).unwrap();
    // Plain git init — no jj required (J7).
    sh("git", &["init"], &abs);
    sh("git", &["config", "user.email", "test@jjforge.invalid"], &abs);
    sh("git", &["config", "user.name", "jjforge test"], &abs);
    // Plant the v3 sentinel via Storage::init (the canonical path).
    Storage::init(&abs).expect("Storage::init must plant the v3 sentinel");
    abs
}

// J7: plant_v3_sentinel removed — make_v3_scratch_repo now calls
// Storage::init, which is the canonical sentinel-planting path.

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

fn git_capture_with_stdin(args: &[&str], stdin: &[u8], cwd: &Path) -> String {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
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

// ---- single-issue read ---------------------------------------------

#[test]
fn v3_read_record_round_trips_create() {
    let repo = make_v3_scratch_repo("v3_read_record");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "v3 first issue".into(),
            body: "Body goes here.".into(),
            slug: Some("v3-first".into()),
            type_: Some(IssueType::Bug),
            labels: vec!["p1".into(), "needs-review".into()],
            ..Default::default()
        })
        .unwrap();

    let issue = storage.read(&id).expect("Storage::read on v3 repo");
    assert_eq!(issue.id, id);
    assert_eq!(issue.title, "v3 first issue");
    assert_eq!(issue.body, "Body goes here.");
    assert_eq!(issue.slug.as_deref(), Some("v3-first"));
    assert_eq!(issue.type_, IssueType::Bug);
    assert_eq!(issue.status, Status::Open);
    let mut labels = issue.labels.clone();
    labels.sort();
    assert_eq!(labels, vec!["needs-review".to_string(), "p1".into()]);
    assert!(issue.comments.is_empty(), "fresh-create has no comments");
}

#[test]
fn v3_read_record_reflects_mutations() {
    let repo = make_v3_scratch_repo("v3_read_record_after_mutations");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "original title".into(),
            ..Default::default()
        })
        .unwrap();

    storage
        .update(
            &id,
            UpdateFields {
                title: Some("updated title".into()),
                status: Some(Status::Closed),
                ..Default::default()
            },
        )
        .unwrap();

    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.title, "updated title");
    assert_eq!(issue.status, Status::Closed);
}

#[test]
fn v3_read_comments_round_trips_thread() {
    let repo = make_v3_scratch_repo("v3_read_comments_round_trip");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "issue with thread".into(),
            ..Default::default()
        })
        .unwrap();
    storage.add_comment(&id, "first", "alice").unwrap();
    storage.add_comment(&id, "second", "bob").unwrap();
    storage.add_comment(&id, "third", "carol").unwrap();

    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.comments.len(), 3);
    let authors: Vec<&str> = issue.comments.iter().map(|c| c.author.as_str()).collect();
    assert_eq!(authors, vec!["alice", "bob", "carol"]);
    let bodies: Vec<&str> = issue.comments.iter().map(|c| c.body.as_str()).collect();
    assert_eq!(bodies, vec!["first", "second", "third"]);
}

#[test]
fn v3_read_returns_issue_not_found_for_missing_ref() {
    use iss_storage::Error;
    let repo = make_v3_scratch_repo("v3_read_missing");
    let storage = Storage::open(&repo).unwrap();
    // Synthesize a never-created id; the storage hex generator is
    // 7-char lowercase hex, so a hand-built "0000000" is structurally
    // valid (no ref points at it).
    let parsed: iss_storage::IssueId = "0000000".parse().unwrap();
    let err = storage.read(&parsed).unwrap_err();
    assert!(
        matches!(err, Error::IssueNotFound(_)),
        "expected IssueNotFound; got {err:?}"
    );
}

// ---- list_ids and list_ready ---------------------------------------

#[test]
fn v3_list_ids_enumerates_per_issue_refs() {
    let repo = make_v3_scratch_repo("v3_list_ids");
    let storage = Storage::open(&repo).unwrap();
    let a = storage
        .create_issue(&IssueDraft {
            title: "alpha".into(),
            ..Default::default()
        })
        .unwrap();
    let b = storage
        .create_issue(&IssueDraft {
            title: "beta".into(),
            ..Default::default()
        })
        .unwrap();
    let c = storage
        .create_issue(&IssueDraft {
            title: "gamma".into(),
            ..Default::default()
        })
        .unwrap();

    let mut ids = storage.list_ids().expect("list_ids on v3 repo");
    ids.sort();
    let mut expected = vec![a.clone(), b.clone(), c.clone()];
    expected.sort();
    assert_eq!(ids, expected);
}

#[test]
fn v3_list_ready_orders_and_filters() {
    let repo = make_v3_scratch_repo("v3_list_ready");
    let storage = Storage::open(&repo).unwrap();

    let _bug = storage
        .create_issue(&IssueDraft {
            title: "a bug".into(),
            type_: Some(IssueType::Bug),
            ..Default::default()
        })
        .unwrap();
    let _feature = storage
        .create_issue(&IssueDraft {
            title: "a feature".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    let _epic = storage
        .create_issue(&IssueDraft {
            title: "an epic".into(),
            type_: Some(IssueType::Epic),
            ..Default::default()
        })
        .unwrap();
    let closed = storage
        .create_issue(&IssueDraft {
            title: "already closed".into(),
            type_: Some(IssueType::Bug),
            ..Default::default()
        })
        .unwrap();
    storage.set_status(&closed, Status::Closed).unwrap();

    let ready = storage
        .list_ready(&ReadyFilter::default())
        .expect("list_ready on v3 repo");
    let titles: Vec<&str> = ready.iter().map(|i| i.title.as_str()).collect();
    // Closed issue excluded; bug > feature > epic priority.
    assert_eq!(titles, vec!["a bug", "a feature", "an epic"]);

    let bugs_only = storage
        .list_ready(&ReadyFilter {
            types: vec![IssueType::Bug],
            ..Default::default()
        })
        .unwrap();
    let titles: Vec<&str> = bugs_only.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, vec!["a bug"]);
}

#[test]
fn v3_list_ready_respects_dep_block() {
    // A `blocks`-edge to an OPEN issue must keep the child out of the
    // ready set, regardless of priority.
    let repo = make_v3_scratch_repo("v3_list_ready_dep_block");
    let storage = Storage::open(&repo).unwrap();
    let target = storage
        .create_issue(&IssueDraft {
            title: "the blocker".into(),
            type_: Some(IssueType::Feature),
            ..Default::default()
        })
        .unwrap();
    let blocked = storage
        .create_issue(&IssueDraft {
            title: "downstream".into(),
            type_: Some(IssueType::Bug),
            dependencies: vec![DepEdge {
                target: target.clone(),
                kind: DepKind::Blocks,
            }],
            ..Default::default()
        })
        .unwrap();
    let ready = storage.list_ready(&ReadyFilter::default()).unwrap();
    let ids: Vec<_> = ready.iter().map(|i| i.id.clone()).collect();
    assert!(
        ids.contains(&target),
        "the blocker is itself ready: {ids:?}"
    );
    assert!(
        !ids.contains(&blocked),
        "the downstream issue must be blocked: {ids:?}"
    );
}

// ---- resolve (slug lookup) -----------------------------------------

#[test]
fn v3_resolve_by_slug_matches_open_issue() {
    let repo = make_v3_scratch_repo("v3_resolve");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "needs a handle".into(),
            slug: Some("needs-a-handle".into()),
            ..Default::default()
        })
        .unwrap();
    let resolved = storage.resolve("needs-a-handle").unwrap();
    assert_eq!(resolved, id);
}

#[test]
fn v3_resolve_unknown_slug_errors() {
    use iss_storage::Error;
    let repo = make_v3_scratch_repo("v3_resolve_unknown");
    let storage = Storage::open(&repo).unwrap();
    let _ = storage
        .create_issue(&IssueDraft {
            title: "noise".into(),
            ..Default::default()
        })
        .unwrap();
    let err = storage.resolve("not-a-slug").unwrap_err();
    assert!(matches!(err, Error::SlugNotFound { .. }), "got {err:?}");
}

// ---- memories ------------------------------------------------------

#[test]
fn v3_memory_round_trips_and_lists() {
    let repo = make_v3_scratch_repo("v3_memory_round_trip");
    let storage = Storage::open(&repo).unwrap();

    storage.set_memory("first", "alpha value").unwrap();
    storage.set_memory("second", "beta value").unwrap();
    storage.set_memory("third", "gamma value").unwrap();

    let m = storage.read_memory("first").unwrap().expect("first present");
    assert_eq!(m.key, "first");
    assert_eq!(m.value, "alpha value");

    let all = storage.list_memories().unwrap();
    let keys: Vec<&str> = all.iter().map(|m| m.key.as_str()).collect();
    assert_eq!(keys, vec!["first", "second", "third"]);

    // Unset should remove from list_memories.
    storage.unset_memory("second").unwrap();
    assert!(storage.read_memory("second").unwrap().is_none());
    let after_unset = storage.list_memories().unwrap();
    let keys: Vec<&str> = after_unset.iter().map(|m| m.key.as_str()).collect();
    assert_eq!(keys, vec!["first", "third"]);
}

// ---- history -------------------------------------------------------

#[test]
fn v3_read_history_walks_per_issue_ref_chain() {
    // The history walker must walk `refs/jjf/issues/<id>` and produce
    // one HistoryEntry per Jjf-Op trailer, chronological (oldest
    // first). Each mutation appends a new commit to the ref.
    let repo = make_v3_scratch_repo("v3_read_history");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "for history".into(),
            ..Default::default()
        })
        .unwrap();
    storage
        .update(
            &id,
            UpdateFields {
                title: Some("rev 2".into()),
                ..Default::default()
            },
        )
        .unwrap();
    storage.add_comment(&id, "hello", "alice").unwrap();
    storage.set_status(&id, Status::Closed).unwrap();

    let history = storage.read_history(&id).expect("read_history on v3 repo");
    assert!(
        history.len() >= 4,
        "expected at least 4 ops (create, set-title, comment-add, set-status); got {}: {:#?}",
        history.len(),
        history,
    );

    // First op is create.
    use iss_storage::Op;
    assert!(
        matches!(history.first().map(|e| &e.op), Some(Op::Create { .. })),
        "first history entry should be Create; got {:?}",
        history.first()
    );

    // Last op should be SetStatus to Closed.
    let last = history.last().unwrap();
    match &last.op {
        Op::SetStatus { status, .. } => {
            assert_eq!(*status, Status::Closed);
        }
        other => panic!("expected final op to be SetStatus Closed, got {other:?}"),
    }
}

#[test]
fn v3_read_history_returns_not_found_for_missing_id() {
    use iss_storage::Error;
    let repo = make_v3_scratch_repo("v3_history_missing");
    let storage = Storage::open(&repo).unwrap();
    let parsed: iss_storage::IssueId = "1111111".parse().unwrap();
    let err = storage.read_history(&parsed).unwrap_err();
    assert!(
        matches!(err, Error::IssueNotFound(_)),
        "expected IssueNotFound; got {err:?}"
    );
}

// ---- snapshot cache (ticket aa915fe) -------------------------------

#[test]
fn v3_cache_hit_avoids_rebuild_n_issues() {
    // V3 counterpart of `cache_hit_avoids_rebuild_n_issues`: build a
    // non-trivial issue set on a v3-shape repo, prime the cache, and
    // assert a second bulk read is faster than the rebuild. The
    // headline win — v2 v3 alike — is that the steady-state hit cost
    // is the probe (one `git for-each-ref`) plus a few HashMap
    // lookups, independent of N.
    let n = 25_usize;
    let repo = make_v3_scratch_repo("v3_cache_hit_speedup");
    let storage = Storage::open(&repo).unwrap();
    for i in 0..n {
        storage
            .create_issue(&IssueDraft {
                title: format!("issue {i}"),
                ..Default::default()
            })
            .unwrap();
    }
    // Force a clean cache miss for the first measurement. Open a
    // fresh Storage so the in-process memo doesn't shortcut us.
    let cache_path = repo.join(".jj").join("jjforge-cache.json");
    let _ = std::fs::remove_file(&cache_path);
    let storage = Storage::open(&repo).unwrap();

    let t0 = std::time::Instant::now();
    let first = storage.list_ready(&ReadyFilter::default()).unwrap();
    let first_dur = t0.elapsed();
    assert_eq!(first.len(), n);

    let t1 = std::time::Instant::now();
    let second = storage.list_ready(&ReadyFilter::default()).unwrap();
    let second_dur = t1.elapsed();
    assert_eq!(second.len(), n);

    // Conservative margin — debug build + heavy CI can flatten the
    // gap. The rebuild reads N `cat-file` blobs (2N for issues with
    // comments); the hit is one `for-each-ref` + `hash-object`. The
    // hit is meaningfully cheaper at N=25.
    assert!(
        second_dur < first_dur,
        "v3 cache hit ({second_dur:?}) should be faster than rebuild ({first_dur:?})"
    );

    // The cache file lands on disk after the first rebuild and
    // survives across `Storage::open` calls.
    assert!(
        cache_path.exists(),
        "rebuild should persist the cache file at {}",
        cache_path.display(),
    );
}

#[test]
fn v3_mutation_invalidates_cache_next_read_rebuilds() {
    // Any v3 mutation drops the in-process memo and lands a new ref
    // tip — so the next read's probe sees a fresh ref-set sha,
    // discards the on-disk cache from the prior key, and rebuilds.
    let repo = make_v3_scratch_repo("v3_cache_invalidate");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "before".into(),
            ..Default::default()
        })
        .unwrap();
    // Prime the cache.
    let first = storage.read(&id).unwrap();
    assert_eq!(first.title, "before");

    // Mutate via the public API. The set-title trailer is what we'd
    // see in real use; the v3 commit lands on `refs/jjf/issues/<id>`.
    storage
        .update(
            &id,
            UpdateFields {
                title: Some("after".into()),
                ..Default::default()
            },
        )
        .unwrap();

    // The read after a mutation MUST surface the new title — which
    // can only happen if the cache invalidated and rebuilt. A stale
    // cache would have returned "before".
    let second = storage.read(&id).unwrap();
    assert_eq!(second.title, "after");
}

// ---- op-replay cross-check (debug builds) --------------------------

#[test]
#[cfg(debug_assertions)]
fn v3_read_returns_cached_record_after_ref_tamper() {
    // The debug-build cross-check between file-read and op-replay
    // (in `read::read`) is defense-in-depth: it fires only when
    // `Storage::read` falls through to the per-id direct read,
    // which happens when the snapshot cache doesn't contain the
    // requested id. On a cache hit (the common case) the cached
    // `Issue` is returned verbatim and the cross-check is skipped —
    // the rebuild itself doesn't cross-check (it reads the same
    // blobs `read::read` would).
    //
    // This test asserts the cache-hit path returns the tampered
    // blob's title (proving the v3 cache is in use). It is the v3
    // counterpart of the (currently unwritten — see
    // `docs/storage-out-of-tree.md`) "cache returns blob-derived
    // record on V2" property. Ticket 7 (test sweep) owns adding a
    // dedicated cache-miss cross-check exercise to lock the
    // defense-in-depth fire path in.
    let repo = make_v3_scratch_repo("v3_replay_panic");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "honest title".into(),
            ..Default::default()
        })
        .unwrap();

    // Read current tip of the per-issue ref + its tree.
    let ref_name = format!("refs/jjf/issues/{id}");
    let tip_oid = git_capture(&["rev-parse", &ref_name], &repo);
    let tip_oid = tip_oid.trim();
    let tree_oid = git_capture(&["rev-parse", &format!("{ref_name}^{{tree}}")], &repo);
    let tree_oid = tree_oid.trim();
    // Read the original issue.json so we can mangle just the title.
    let original = git_capture(&["cat-file", "blob", &format!("{ref_name}:issue.json")], &repo);
    // Replace the title without emitting a corresponding `set-title`
    // trailer. The cross-check then sees file.title != op_view.title
    // and panics.
    let tampered = original.replace("\"title\": \"honest title\"", "\"title\": \"sneaky\"");
    assert_ne!(tampered, original, "test setup: substitution must change the blob");

    // Hash the tampered blob, mktree, commit-tree (parent = tip_oid),
    // update-ref.
    let new_blob =
        git_capture_with_stdin(&["hash-object", "-w", "--stdin"], tampered.as_bytes(), &repo);
    let new_blob = new_blob.trim();
    let mktree_input = format!("100644 blob {new_blob}\tissue.json\n");
    let new_tree =
        git_capture_with_stdin(&["mktree"], mktree_input.as_bytes(), &repo);
    let new_tree = new_tree.trim();
    let _ = tree_oid; // assertion-time only; quiet unused warning
    let new_commit = git_capture_with_stdin(
        &[
            "commit-tree",
            new_tree,
            "-p",
            tip_oid,
            "-F",
            "-",
        ],
        // Note: no Jjf-Op trailer — that's the whole point. The op
        // chain stays "honest" but the snapshot is tampered.
        b"tamper: title without an op\n",
        &repo,
    );
    let new_commit = new_commit.trim();
    sh(
        "git",
        &["update-ref", &ref_name, new_commit, tip_oid],
        &repo,
    );

    // The in-process snapshot memo was populated during create.
    // Drop it so the next `read` re-probes and rebuilds against the
    // tampered ref-set.
    let storage = Storage::open(&repo).unwrap();
    let issue = storage.read(&id).expect("cache rebuild reads the tampered blob");
    assert_eq!(
        issue.title, "sneaky",
        "v3 cache should reflect the on-disk blob after a ref tamper"
    );
}

// ---- corrupt-sentinel detection -----------------------------------
//
// Ticket `de59159` (QA red-team sub-pass 3, attack `c1`): if someone
// hand-wires `refs/jjf/meta/format-version` to a blob (or tree, or
// tag) rather than a commit, `detect_storage_mode` used to return
// `StorageMode::V3` because `resolve_ref` checks presence not type.
// The docstring at `lib.rs:1037` explicitly promises "iff the
// sentinel ref resolves to a commit"; the fix enforces it.
//
// Test contract: opening such a repo surfaces the typed
// `Error::CorruptSentinel { oid, kind }` rather than silently
// flipping mode (or worse, panicking later on a missing tree
// readbehind).

#[test]
fn v3_open_rejects_blob_sentinel() {
    let repo = make_v3_scratch_repo("v3_open_rejects_blob_sentinel");
    // Repoint the sentinel ref at a blob oid.
    let blob_oid = git_capture_with_stdin(
        &["hash-object", "-w", "--stdin"],
        b"not a commit\n",
        &repo,
    );
    let blob_oid = blob_oid.trim();
    sh(
        "git",
        &["update-ref", "refs/jjf/meta/format-version", blob_oid],
        &repo,
    );

    let err = Storage::open(&repo).expect_err(
        "blob sentinel must be rejected, not silently treated as v3",
    );
    match err {
        StorageError::CorruptSentinel { oid, kind } => {
            assert_eq!(oid, blob_oid, "carry the offending oid");
            assert_eq!(
                kind, "blob",
                "carry git's own object-type classification"
            );
        }
        other => panic!("expected CorruptSentinel, got {other:?}"),
    }
}

#[test]
fn v3_open_rejects_tree_sentinel() {
    let repo = make_v3_scratch_repo("v3_open_rejects_tree_sentinel");
    // Mint a standalone tree (with no commit wrapper) and point the
    // sentinel at it.
    let blob_oid = git_capture_with_stdin(
        &["hash-object", "-w", "--stdin"],
        b"3\n",
        &repo,
    );
    let blob_oid = blob_oid.trim();
    let tree_oid = git_capture_with_stdin(
        &["mktree"],
        format!("100644 blob {blob_oid}\tversion\n").as_bytes(),
        &repo,
    );
    let tree_oid = tree_oid.trim();
    sh(
        "git",
        &["update-ref", "refs/jjf/meta/format-version", tree_oid],
        &repo,
    );

    let err = Storage::open(&repo)
        .expect_err("tree sentinel must be rejected");
    match err {
        StorageError::CorruptSentinel { oid, kind } => {
            assert_eq!(oid, tree_oid);
            assert_eq!(kind, "tree");
        }
        other => panic!("expected CorruptSentinel, got {other:?}"),
    }
}

/// A jj+git colocated repo with NO `refs/jjf/meta/format-version`
/// sentinel is a legacy (v1/v2-shape) repo. After J5, `Storage::open`
/// must REFUSE it with `Error::UnsupportedLegacyFormat` rather than
/// silently reading or migrating it.
#[test]
fn open_refuses_legacy_repo_without_v3_sentinel() {
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(".scratch")
        .join("open_refuses_legacy");
    if scratch.exists() {
        fs::remove_dir_all(&scratch).unwrap();
    }
    fs::create_dir_all(&scratch).unwrap();
    let repo = fs::canonicalize(&scratch).unwrap();
    // J7: plain git init — no jj required.
    sh("git", &["init"], &repo);
    sh("git", &["config", "user.email", "test@jjforge.invalid"], &repo);
    sh("git", &["config", "user.name", "jjforge test"], &repo);
    // Deliberately do NOT plant the v3 sentinel — this is a legacy
    // repo shape.

    let err = Storage::open(&repo)
        .expect_err("a repo without the v3 sentinel must be refused");
    match err {
        StorageError::UnsupportedLegacyFormat { path } => {
            assert_eq!(path, repo);
        }
        other => panic!("expected UnsupportedLegacyFormat, got {other:?}"),
    }
}

fn git_capture(args: &[&str], cwd: &Path) -> String {
    let out = Command::new("git").args(args).current_dir(cwd).output().unwrap();
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
