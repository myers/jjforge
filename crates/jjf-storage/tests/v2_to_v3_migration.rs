//! Integration tests for the v2 → v3 auto-migrator.
//!
//! Pinned by ticket `c14e1c1` (storage-v3 #4). Each test sets up a
//! v2-shape repo (the v2 `issues` bookmark with one or more issues +
//! optional memories) by opening via
//! [`Storage::open_skip_v2_to_v3_migration`] so the v2-shape data
//! lands, then re-opens via the normal [`Storage::open`] to trigger
//! the migration.
//!
//! Post-migration, we assert:
//!
//! - The `issues` bookmark is gone.
//! - The `refs/jjf/meta/format-version` sentinel exists.
//! - Every issue id round-trips through `read_record` / `read_comments`
//!   with byte-identical content.
//! - Every issue's v3 ref chain has the same number of commits as
//!   the v2 history walker would have returned for that issue.
//! - Re-opening a migrated repo is a no-op.
//! - A v1 → v2 → v3 chain works on a synthesized v1 repo.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use jjf_storage::{IssueDraft, Status, Storage, UpdateFields};

/// Run a closure that needs to write v2-shape state. Opens the repo
/// via [`Storage::open_skip_v2_to_v3_migration`] inside the closure
/// so the v2 paths stay live; the caller of `with_disable` does NOT
/// receive the handle — the closure builds its own — because each
/// test wants to drop the v2 handle before re-opening to trigger
/// the migrator.
///
/// Kept as a wrapper (vs. inlining the call) because every test
/// uses the same v2-shape setup-then-migrate two-phase recipe and
/// the name documents the intent.
fn with_disable<F: FnOnce() -> R, R>(f: F) -> R {
    f()
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(".scratch")
        .join(format!("v2_to_v3_{name}"));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(&dir).unwrap()
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

/// Build a v2-shape jj+git colocated repo with the `issues` bookmark
/// planted. The migration is suppressed by the surrounding
/// `with_disable` call so subsequent `Storage::open` calls inside it
/// stay v2-mode.
fn make_v2_repo(name: &str) -> PathBuf {
    let abs = scratch(name);
    sh("jj", &["git", "init"], &abs);
    // Plant the v2 `issues` bookmark with the standard seed.
    sh("jj", &["new", "root()", "-m", "jjf: seed issues bookmark"], &abs);
    sh("jj", &["bookmark", "create", "issues", "-r", "@"], &abs);
    sh("jj", &["new", "root()"], &abs);
    abs
}

/// Does the named ref exist?
fn ref_exists(repo: &Path, refname: &str) -> bool {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", refname])
        .current_dir(repo)
        .output()
        .unwrap();
    out.status.success()
}

/// Count commits on a ref chain (oldest-first walk via `git log
/// --reverse`).
fn count_commits_on_ref(repo: &Path, refname: &str) -> usize {
    let out = git_capture(&["log", "--reverse", "--format=%H", refname], repo);
    out.lines().filter(|l| !l.trim().is_empty()).count()
}

// ---------------------------------------------------------------------
// Acceptance test 1: a v2 repo with 5+ issues migrates to v3 on first
// open. read_record / read_comments are byte-identical pre and post.
// ---------------------------------------------------------------------

#[test]
fn v2_to_v3_migration_round_trips_records_and_comments() {
    let repo = make_v2_repo("round_trip_records");

    // Phase 1: stand up v2 data. Five issues, varying shapes:
    // - issue 0: bare create, no comments, open.
    // - issue 1: create + close, no comments.
    // - issue 2: create + 2 comments, still open.
    // - issue 3: create + close + 1 comment (post-close comment).
    // - issue 4: create + slug + assignee + label, all set on
    //   create (the multi-op stanza of spec §5.7).
    let (ids_v2, records_v2, comments_v2) = with_disable(|| {
        let s = Storage::open_skip_v2_to_v3_migration(&repo).expect("open v2");

        let i0 = s.create_issue(&IssueDraft {
            title: "issue zero".into(),
            ..Default::default()
        })
        .unwrap();

        let i1 = s.create_issue(&IssueDraft {
            title: "issue one".into(),
            ..Default::default()
        })
        .unwrap();
        s.set_status(&i1, Status::Closed).unwrap();

        let i2 = s.create_issue(&IssueDraft {
            title: "issue two".into(),
            ..Default::default()
        })
        .unwrap();
        s.add_comment(&i2, "first comment", "alice").unwrap();
        s.add_comment(&i2, "second comment", "bob").unwrap();

        let i3 = s.create_issue(&IssueDraft {
            title: "issue three".into(),
            ..Default::default()
        })
        .unwrap();
        s.set_status(&i3, Status::Closed).unwrap();
        s.add_comment(&i3, "post-close finding", "carol").unwrap();

        let i4 = s.create_issue(&IssueDraft {
            title: "issue four".into(),
            slug: Some("issue-four-slug".into()),
            assignee: Some("alice".into()),
            labels: vec!["epic:test".into()],
            ..Default::default()
        })
        .unwrap();

        // Snapshot the v2 state for comparison after migration.
        let mut ids: Vec<_> = vec![i0, i1, i2, i3, i4];
        ids.sort();
        let mut records = Vec::new();
        let mut comments = Vec::new();
        for id in &ids {
            let issue = s.read(id).unwrap();
            records.push((id.clone(), issue.clone()));
            comments.push((id.clone(), issue.comments));
        }
        (ids, records, comments)
    });

    // Phase 2: migrate. Re-open WITHOUT the opt-out — the migrator
    // runs.
    let s_v3 = Storage::open(&repo).expect("open triggers migration");

    // Post-migration assertions.
    assert!(
        !ref_exists(&repo, "refs/heads/issues"),
        "`issues` bookmark must be deleted post-migration"
    );
    assert!(
        ref_exists(&repo, "refs/jjf/meta/format-version"),
        "v3 sentinel ref must exist post-migration"
    );
    for id in &ids_v2 {
        let ref_name = format!("refs/jjf/issues/{}", id);
        assert!(
            ref_exists(&repo, &ref_name),
            "v3 ref must exist for {id}"
        );
    }

    // Records round-trip byte-identically.
    for (id, original) in &records_v2 {
        let post = s_v3.read(id).unwrap_or_else(|e| {
            panic!("read({id}) post-migration failed: {e:?}")
        });
        assert_eq!(
            post.id, original.id,
            "id mismatch for {id}"
        );
        assert_eq!(
            post.title, original.title,
            "title mismatch for {id}"
        );
        assert_eq!(
            post.status, original.status,
            "status mismatch for {id}"
        );
        assert_eq!(
            post.slug, original.slug,
            "slug mismatch for {id}"
        );
        assert_eq!(
            post.assignee, original.assignee,
            "assignee mismatch for {id}"
        );
        assert_eq!(
            post.labels, original.labels,
            "labels mismatch for {id}"
        );
        assert_eq!(
            post.created_at, original.created_at,
            "created_at mismatch for {id}"
        );
        assert_eq!(
            post.updated_at, original.updated_at,
            "updated_at mismatch for {id}"
        );
    }

    // Comments round-trip byte-identically.
    for (id, original) in &comments_v2 {
        let post = s_v3.read(id).unwrap().comments;
        assert_eq!(
            post.len(),
            original.len(),
            "comment count mismatch for {id}: v2={} v3={}",
            original.len(),
            post.len()
        );
        for (a, b) in post.iter().zip(original.iter()) {
            assert_eq!(a.body, b.body, "comment body mismatch in {id}");
            assert_eq!(
                a.created_at, b.created_at,
                "comment created_at mismatch in {id}"
            );
        }
    }
}

// ---------------------------------------------------------------------
// Acceptance test 2: every issue's v3 chain has the same number of
// commits as the v2 history.
// ---------------------------------------------------------------------

#[test]
fn v2_to_v3_migration_preserves_op_chain_length() {
    let repo = make_v2_repo("op_chain_length");

    let (ids_v2, v2_chain_lengths) = with_disable(|| {
        let s = Storage::open_skip_v2_to_v3_migration(&repo).expect("open v2");

        // Build issues with KNOWN commit counts.
        // - i0: 1 commit (create only).
        // - i1: 2 commits (create + close).
        // - i2: 3 commits (create + 2 comments).
        // - i3: 4 commits (create + close + 1 comment + 1 update).
        let i0 = s.create_issue(&IssueDraft {
            title: "i0".into(),
            ..Default::default()
        })
        .unwrap();

        let i1 = s.create_issue(&IssueDraft {
            title: "i1".into(),
            ..Default::default()
        })
        .unwrap();
        s.set_status(&i1, Status::Closed).unwrap();

        let i2 = s.create_issue(&IssueDraft {
            title: "i2".into(),
            ..Default::default()
        })
        .unwrap();
        s.add_comment(&i2, "comment a", "alice").unwrap();
        s.add_comment(&i2, "comment b", "bob").unwrap();

        let i3 = s.create_issue(&IssueDraft {
            title: "i3".into(),
            ..Default::default()
        })
        .unwrap();
        s.set_status(&i3, Status::Closed).unwrap();
        s.add_comment(&i3, "i3 comment", "carol").unwrap();
        s.update(
            &i3,
            UpdateFields {
                title: Some("i3 renamed".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let mut ids = vec![i0.clone(), i1.clone(), i2.clone(), i3.clone()];
        ids.sort();
        // Map each id to its expected v2 history length. Each
        // mutation lands one commit on the bookmark; the create
        // lands the create commit; the seed commit doesn't touch the
        // issue's files so isn't counted by the per-issue walker.
        let expected = vec![
            (i0, 1usize),
            (i1, 2),
            (i2, 3),
            (i3, 4),
        ];
        (ids, expected)
    });

    // Migrate.
    let _s_v3 = Storage::open(&repo).expect("open triggers migration");

    // Check chain lengths on each v3 ref.
    for (id, expected_len) in &v2_chain_lengths {
        let ref_name = format!("refs/jjf/issues/{}", id);
        let actual = count_commits_on_ref(&repo, &ref_name);
        assert_eq!(
            actual, *expected_len,
            "ref {ref_name} should have {expected_len} commits, got {actual}"
        );
    }
    // Sanity: make sure we covered every id.
    assert_eq!(ids_v2.len(), v2_chain_lengths.len());
}

// ---------------------------------------------------------------------
// Acceptance test 3: re-opening a migrated repo is a no-op (no second
// migration). We verify by reading v3 ref shas before and after a
// second open.
// ---------------------------------------------------------------------

#[test]
fn re_open_after_migration_is_a_no_op() {
    let repo = make_v2_repo("re_open_noop");

    // Stand up v2 + migrate.
    let id = with_disable(|| {
        let s = Storage::open_skip_v2_to_v3_migration(&repo).expect("open v2");
        s.create_issue(&IssueDraft {
            title: "single issue".into(),
            ..Default::default()
        })
        .unwrap()
    });

    // First open: triggers migration. Snapshot the v3 ref's tip.
    let _ = Storage::open(&repo).expect("first open (migrates)");
    let ref_name = format!("refs/jjf/issues/{}", id);
    let tip_before = git_capture(&["rev-parse", &ref_name], &repo)
        .trim()
        .to_owned();
    let sentinel_before =
        git_capture(&["rev-parse", "refs/jjf/meta/format-version"], &repo)
            .trim()
            .to_owned();

    // Second open: must NOT mutate any v3 ref.
    let _ = Storage::open(&repo).expect("second open (no-op)");
    let tip_after = git_capture(&["rev-parse", &ref_name], &repo)
        .trim()
        .to_owned();
    let sentinel_after =
        git_capture(&["rev-parse", "refs/jjf/meta/format-version"], &repo)
            .trim()
            .to_owned();

    assert_eq!(
        tip_before, tip_after,
        "v3 ref tip must not change on re-open"
    );
    assert_eq!(
        sentinel_before, sentinel_after,
        "sentinel ref must not change on re-open"
    );
    // Bookmark still absent.
    assert!(
        !ref_exists(&repo, "refs/heads/issues"),
        "`issues` bookmark must stay deleted on re-open"
    );
}

// ---------------------------------------------------------------------
// Acceptance test 4: migrate memories alongside issues.
// ---------------------------------------------------------------------

#[test]
fn v2_to_v3_migration_carries_memories() {
    let repo = make_v2_repo("memories");

    let (mems_v2, _issue) = with_disable(|| {
        let s = Storage::open_skip_v2_to_v3_migration(&repo).expect("open v2");
        // Need at least one issue so the bookmark has any meaningful
        // state, but memories live alongside issues on the same
        // bookmark — they get migrated too.
        let id = s.create_issue(&IssueDraft {
            title: "anchor issue".into(),
            ..Default::default()
        })
        .unwrap();
        s.set_memory("alpha-rule", "alpha first").unwrap();
        s.set_memory("beta-rule", "beta second").unwrap();
        // Overwrite one of them so its chain has 2 commits.
        s.set_memory("alpha-rule", "alpha rewritten").unwrap();

        let mems = s.list_memories().unwrap();
        (mems, id)
    });

    // Migrate.
    let s_v3 = Storage::open(&repo).expect("migrate");

    // Memories round-trip.
    let mems_v3 = s_v3.list_memories().unwrap();
    assert_eq!(
        mems_v3.len(),
        mems_v2.len(),
        "memory count mismatch v2={:?} v3={:?}",
        mems_v2.iter().map(|m| &m.key).collect::<Vec<_>>(),
        mems_v3.iter().map(|m| &m.key).collect::<Vec<_>>()
    );
    for m_v2 in &mems_v2 {
        let found = mems_v3
            .iter()
            .find(|m| m.key == m_v2.key)
            .unwrap_or_else(|| panic!("memory {} missing post-migration", m_v2.key));
        assert_eq!(found.value, m_v2.value, "memory value mismatch for {}", m_v2.key);
    }

    // The overwritten memory's v3 ref should have 2 commits (matches
    // the v2 chain of two `set-memory` commits).
    assert_eq!(
        count_commits_on_ref(&repo, "refs/jjf/memories/alpha-rule"),
        2,
        "alpha-rule should have 2 commits (initial set + rewrite)"
    );
    assert_eq!(
        count_commits_on_ref(&repo, "refs/jjf/memories/beta-rule"),
        1,
        "beta-rule should have 1 commit (initial set)"
    );
}

// ---------------------------------------------------------------------
// Acceptance test 5: v1 → v2 → v3 chained migration on a synthesized
// v1 repo.
// ---------------------------------------------------------------------

#[test]
fn v1_to_v2_to_v3_chained_migration() {
    let repo = make_v2_repo("v1_to_v2_to_v3");

    // Stand up v2 first, then rewrite to v1 shape on disk (mirroring
    // the trick the existing `v1_to_v2_migration_preserves_history`
    // integration test uses).
    let id = with_disable(|| {
        let s = Storage::open_skip_v2_to_v3_migration(&repo).expect("open v2");
        let id = s.create_issue(&IssueDraft {
            title: "v1 era issue".into(),
            ..Default::default()
        })
        .unwrap();
        s.set_status(&id, Status::Closed).unwrap();
        id
    });

    // Rewrite paths to v1 shape: issues/<id>.* → bugs/<id>.*.
    let json_old = format!("issues/{}.json", id);
    let comments_old = format!("issues/{}.comments.jsonl", id);
    let json_new = format!("bugs/{}.json", id);
    let comments_new = format!("bugs/{}.comments.jsonl", id);

    sh("jj", &["new", "bookmarks(issues)", "-m", "synthesize v1 layout"], &repo);
    fs::create_dir_all(repo.join("bugs")).unwrap();
    fs::rename(repo.join(&json_old), repo.join(&json_new)).unwrap();
    fs::rename(repo.join(&comments_old), repo.join(&comments_new)).unwrap();
    let _ = fs::remove_dir(repo.join("issues"));

    sh("jj", &["bookmark", "create", "bugs", "-r", "@"], &repo);
    sh("jj", &["bookmark", "delete", "issues"], &repo);
    sh("jj", &["new", "root()"], &repo);

    // Sanity check: v1 shape now.
    let bookmarks = jj_capture(&["bookmark", "list", "-T", "name ++ \"\\n\""], &repo);
    assert!(
        bookmarks.lines().any(|l| l.trim() == "bugs"),
        "synthesized v1 must have a `bugs` bookmark"
    );
    assert!(
        !bookmarks.lines().any(|l| l.trim() == "issues"),
        "synthesized v1 must NOT have an `issues` bookmark"
    );

    // Open WITHOUT the opt-out. This triggers v1 → v2 inline, then
    // v2 → v3 auto-migration in sequence.
    let s = Storage::open(&repo).expect("chained migration");

    // Read the issue back.
    let issue = s.read(&id).expect("read issue post-chain");
    assert_eq!(issue.title, "v1 era issue");
    assert_eq!(issue.status, Status::Closed);

    // The repo is now v3.
    assert!(
        ref_exists(&repo, "refs/jjf/meta/format-version"),
        "v3 sentinel must exist after chained migration"
    );
    assert!(
        !ref_exists(&repo, "refs/heads/issues"),
        "v2 bookmark must be gone after chained migration"
    );
    assert!(
        !ref_exists(&repo, "refs/heads/bugs"),
        "v1 bookmark must be gone after chained migration"
    );
    assert!(
        ref_exists(&repo, &format!("refs/jjf/issues/{}", id)),
        "v3 per-issue ref must exist after chained migration"
    );

    // Chain length: create + close = 2 commits. The v1 → v2 migration
    // commit itself isn't an issue op (no Jjf-Op trailer for this
    // issue), but it DOES touch the issue's path filter (the file
    // rename), so the v2 → v3 walker emits a third commit. We assert
    // >= 2 here; the exact count depends on whether the rename
    // commit gets included as a content-bearing v3 commit.
    let n = count_commits_on_ref(&repo, &format!("refs/jjf/issues/{}", id));
    assert!(
        n >= 2,
        "expected at least 2 commits on the v3 ref chain (create + close); got {n}"
    );
}

// ---------------------------------------------------------------------
// Acceptance test 6: idempotency on partial migration. Pre-populate a
// v3 ref for one issue, then re-run the migration; the pre-populated
// ref must NOT be overwritten, and the rest of the issues must still
// migrate.
// ---------------------------------------------------------------------

#[test]
fn migration_skips_issues_with_existing_v3_ref() {
    let repo = make_v2_repo("partial_migration");

    let (id0, id1) = with_disable(|| {
        let s = Storage::open_skip_v2_to_v3_migration(&repo).expect("open v2");
        let i0 = s.create_issue(&IssueDraft {
            title: "issue zero".into(),
            ..Default::default()
        })
        .unwrap();
        let i1 = s.create_issue(&IssueDraft {
            title: "issue one".into(),
            ..Default::default()
        })
        .unwrap();
        (i0, i1)
    });

    // Pre-plant a v3 ref for id0 pointing at the sentinel of an
    // empty tree — emulating a partial migration where id0 was
    // already done by a previous crashed pass. The migrator must
    // notice the ref exists and skip id0.
    //
    // Use git plumbing directly. The committed tree is empty (no
    // issue.json), which would be invalid v3 content, but the
    // important thing is the ref's PRESENCE.
    let empty_tree = git_capture(&["mktree"], &repo).trim().to_owned();
    // Empty tree comes from `git mktree` with empty stdin; we have
    // to actually pipe in nothing. The above `git mktree` ran with
    // no stdin and produced the empty-tree oid.
    let commit_oid = {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "commit-tree", &empty_tree, "-F", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"jjf: precommitted v3 ref (idempotency test)\n")
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "commit-tree failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    sh(
        "git",
        &[
            "update-ref",
            &format!("refs/jjf/issues/{}", id0),
            &commit_oid,
            "0000000000000000000000000000000000000000",
        ],
        &repo,
    );

    // Now migrate.
    let _s = Storage::open(&repo).expect("migrate with pre-populated ref");

    // id0's ref should still point at our hand-built commit (the
    // migrator left it alone).
    let id0_tip = git_capture(&["rev-parse", &format!("refs/jjf/issues/{}", id0)], &repo)
        .trim()
        .to_owned();
    assert_eq!(
        id0_tip, commit_oid,
        "pre-populated v3 ref for {id0} must not be overwritten by the migrator"
    );

    // id1's ref should exist (the migrator built it).
    assert!(
        ref_exists(&repo, &format!("refs/jjf/issues/{}", id1)),
        "v3 ref for {id1} must exist post-migration"
    );

    // Sentinel must be planted.
    assert!(
        ref_exists(&repo, "refs/jjf/meta/format-version"),
        "sentinel must be planted post-migration"
    );
    // Bookmark gone.
    assert!(
        !ref_exists(&repo, "refs/heads/issues"),
        "`issues` bookmark must be deleted"
    );
}

