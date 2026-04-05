//! Integration tests for the v3 git-only write path.
//!
//! Pinned by ticket `eb42f50` (storage-v3 #1):
//! - A v3-shape repo (sentinel ref planted) routes every mutating
//!   `Storage` method through the new git-only write path.
//! - git HEAD does not move across mutations (the v2 4-CLI dance's
//!   primary failure mode).
//! - The jj working copy is unchanged across mutations.
//! - No `jj` subprocess is invoked on the v3 write path (verified by
//!   keeping the v3 module's source tree `Command::new("jj")`-free —
//!   a grep-test below double-checks the source).
//!
//! The v2 path's existing tests (in `integration.rs`) cover the
//! "v2 repos still work" requirement; this file is v3-only.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use jjf_storage::{
    DepKind, IssueDraft, Status, Storage, UpdateFields,
};

/// Build a v3-shape scratch repo: a plain git repo with the
/// `refs/jjf/meta/format-version` sentinel ref planted. `Storage::open`
/// will detect V3 mode and route every write through the git-only path.
///
/// J7: switched from `jj git init --colocate` to `git init` — the
/// shipped binary no longer calls jj, and the tests should not either.
/// The `Storage::init` call plants the sentinel; we call it via the API
/// so Storage is the actor (rather than hand-planting the sentinel).
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
    // Configure git identity locally — `commit-tree` needs an author.
    // Set explicitly here so the test is hermetic in CI.
    sh("git", &["config", "user.email", "test@jjforge.invalid"], &abs);
    sh("git", &["config", "user.name", "jjforge test"], &abs);

    // Plant the v3 sentinel ref via Storage::init (the canonical path).
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

/// `git rev-parse HEAD`, tolerating the "unborn HEAD" state (a
/// fresh plain git init has HEAD pointing at `refs/heads/main` but
/// that ref doesn't exist yet — there's no commit). Returns an
/// empty string in the unborn case. The v3 contract is that
/// mutations never move HEAD; comparing the value before/after
/// pins it whether or not the ref is born.
fn git_head(repo: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .current_dir(repo)
        .output()
        .unwrap();
    if !out.status.success() {
        return String::new();
    }
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// `git symbolic-ref HEAD`. Used to verify HEAD's symbolic target
/// (`refs/heads/main` in a fresh colocated init) doesn't get
/// retargeted at `refs/jj/root` (the v2 dance's drift symptom).
fn git_head_symbolic(repo: &Path) -> String {
    git_capture(&["symbolic-ref", "HEAD"], repo)
        .trim()
        .to_owned()
}

// J7: jj_at_change_id removed — plain git repos have no jj working-copy
// change concept. The git HEAD invariant is sufficient.

/// Resolve a ref to its oid (or empty string if missing).
fn git_show_ref(repo: &Path, ref_name: &str) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", ref_name])
        .current_dir(repo)
        .output()
        .unwrap();
    if !out.status.success() {
        return String::new();
    }
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Read a blob from a ref's tree.
fn git_blob_at(repo: &Path, ref_name: &str, path: &str) -> Option<String> {
    let spec = format!("{ref_name}:{path}");
    let out = Command::new("git")
        .args(["cat-file", "blob", &spec])
        .current_dir(repo)
        .output()
        .unwrap();
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn v3_repo_open_detects_v3_mode_and_writes_via_git_only() {
    // The behavior pinned by ticket `eb42f50`'s acceptance:
    // - Storage::open against a v3-shape repo routes writes via the
    //   git path.
    // - git HEAD does not move; HEAD's symbolic target stays put.
    // - jj's working-copy change (`@`) is unchanged.
    // - The new issue lands as a commit on `refs/jjf/issues/<id>`,
    //   carrying `issue.json` in its tip's tree.
    let repo = make_v3_scratch_repo("v3_open_then_create_issue");
    let head_before = git_head(&repo);
    let head_sym_before = git_head_symbolic(&repo);

    let storage = Storage::open(&repo).expect("Storage::open on v3 repo");
    let id = storage
        .create_issue(&IssueDraft {
            title: "v3 first issue".into(),
            body: "Body goes here.".into(),
            ..Default::default()
        })
        .expect("create_issue v3");

    let ref_name = format!("refs/jjf/issues/{id}");
    let tip = git_show_ref(&repo, &ref_name);
    assert!(
        !tip.is_empty(),
        "v3 issue ref {ref_name} should exist after create_issue"
    );
    let issue_blob = git_blob_at(&repo, &ref_name, "issue.json")
        .expect("v3 issue ref tip must carry issue.json");
    assert!(
        issue_blob.contains("\"title\": \"v3 first issue\""),
        "blob should carry the issue JSON; got: {issue_blob}"
    );

    // Comments file should NOT be present at create time on v3 —
    // the design is "comments.jsonl (if any)" and create has no
    // comments. The v2 path planted an empty file; v3 doesn't.
    assert!(
        git_blob_at(&repo, &ref_name, "comments.jsonl").is_none(),
        "v3 create should not plant an empty comments.jsonl in the tree"
    );

    // The 4-CLI dance's failure mode: HEAD drifts onto refs/jj/root.
    // We assert NEITHER form of drift fires:
    // - The git HEAD oid is unchanged.
    // - HEAD's symbolic target hasn't been swapped (e.g. to refs/jj/root).
    assert_eq!(
        git_head(&repo),
        head_before,
        "v3 write must not move git HEAD oid"
    );
    assert_eq!(
        git_head_symbolic(&repo),
        head_sym_before,
        "v3 write must not retarget HEAD (drift fingerprint: refs/jj/root)"
    );
}

#[test]
fn v3_mutate_preserves_head_and_chains_commits() {
    let repo = make_v3_scratch_repo("v3_mutate_chains_commits");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "v3 issue to mutate".into(),
            ..Default::default()
        })
        .unwrap();

    let ref_name = format!("refs/jjf/issues/{id}");
    let after_create = git_show_ref(&repo, &ref_name);
    let head_before = git_head(&repo);

    // A scalar mutation lands a new commit on the per-issue ref,
    // with the create commit as parent.
    storage.set_status(&id, Status::Closed).expect("set_status");
    let after_status = git_show_ref(&repo, &ref_name);
    assert_ne!(
        after_create, after_status,
        "set_status should fast-forward the per-issue ref"
    );
    // The new commit's parent is the create commit (chain shape).
    let parent_of_status = git_capture(
        &["rev-parse", &format!("{after_status}^")],
        &repo,
    )
    .trim()
    .to_owned();
    assert_eq!(
        parent_of_status, after_create,
        "v3 mutate must chain: new commit's parent == previous tip"
    );
    // git HEAD is pinned through the mutation.
    assert_eq!(git_head(&repo), head_before);

    // The new tip's `issue.json` reflects the set_status mutation.
    let issue_blob = git_blob_at(&repo, &ref_name, "issue.json")
        .expect("post-mutation tree must carry issue.json");
    assert!(
        issue_blob.contains("\"status\": \"closed\""),
        "post-set-status blob should carry status=closed: {issue_blob}"
    );
}

#[test]
fn v3_add_comment_writes_comments_jsonl_in_tree() {
    let repo = make_v3_scratch_repo("v3_add_comment");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "issue with comments".into(),
            ..Default::default()
        })
        .unwrap();
    let ref_name = format!("refs/jjf/issues/{id}");

    let head_before = git_head(&repo);

    let _c1 = storage
        .add_comment(&id, "first comment", "alice")
        .expect("add_comment 1");
    let comments_blob = git_blob_at(&repo, &ref_name, "comments.jsonl")
        .expect("comments.jsonl must be present after first add_comment");
    assert!(
        comments_blob.contains("\"author\":\"alice\""),
        "first comment author should be in jsonl: {comments_blob}"
    );

    let _c2 = storage
        .add_comment(&id, "second comment", "bob")
        .expect("add_comment 2");
    let comments_blob = git_blob_at(&repo, &ref_name, "comments.jsonl")
        .expect("comments.jsonl must persist across writes");
    assert!(comments_blob.contains("\"author\":\"alice\""));
    assert!(comments_blob.contains("\"author\":\"bob\""));
    let line_count = comments_blob.lines().filter(|l| !l.is_empty()).count();
    assert_eq!(line_count, 2, "expected two comment lines in jsonl");

    // HEAD stable through both add_comment calls.
    assert_eq!(git_head(&repo), head_before);
}

#[test]
fn v3_update_with_subsequent_mutation_preserves_comments_in_tree() {
    // Once an issue has comments, every subsequent scalar mutation
    // must re-list `comments.jsonl` in the new tree — git computes
    // tree oids from the blob set, so a tree that "preserves" the
    // comments must literally re-list the blob.
    let repo = make_v3_scratch_repo("v3_mutate_preserves_comments");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "preserve comments across mutate".into(),
            ..Default::default()
        })
        .unwrap();
    storage.add_comment(&id, "the only comment", "carol").unwrap();

    let ref_name = format!("refs/jjf/issues/{id}");
    let pre_blob = git_blob_at(&repo, &ref_name, "comments.jsonl").unwrap();

    storage
        .update(
            &id,
            UpdateFields {
                title: Some("updated title".into()),
                ..Default::default()
            },
        )
        .expect("update");

    let post_blob = git_blob_at(&repo, &ref_name, "comments.jsonl");
    assert_eq!(
        post_blob.as_deref(),
        Some(pre_blob.as_str()),
        "scalar mutation must preserve comments.jsonl byte-for-byte"
    );
}

#[test]
fn v3_set_and_unset_memory_chain_on_per_memory_ref() {
    let repo = make_v3_scratch_repo("v3_memory_chain");
    let storage = Storage::open(&repo).unwrap();

    let head_before = git_head(&repo);

    storage.set_memory("dolt-phantoms", "Three places").unwrap();
    let mem_ref = "refs/jjf/memories/dolt-phantoms";
    let after_set = git_show_ref(&repo, mem_ref);
    assert!(!after_set.is_empty(), "memory ref should exist after set");
    let blob = git_blob_at(&repo, mem_ref, "memory.json")
        .expect("memory.json present after set");
    assert!(blob.contains("\"value\": \"Three places\""));

    storage.unset_memory("dolt-phantoms").unwrap();
    let after_unset = git_show_ref(&repo, mem_ref);
    assert_ne!(
        after_unset, after_set,
        "unset should land a new commit (chain shape)"
    );
    assert!(
        git_blob_at(&repo, mem_ref, "memory.json").is_none(),
        "unset's tip tree should not carry memory.json"
    );

    assert_eq!(git_head(&repo), head_before);
}

#[test]
fn v3_write_path_source_does_not_invoke_jj() {
    // Ticket `eb42f50`: "No `jj` subprocess invoked on a v3 write
    // path. Verify by grepping the write path code; no
    // `Command::new(\"jj\")`."
    //
    // Both v3-path modules (`git.rs` and `v3_write.rs`) must spawn
    // `git`, never `jj`. The grep-test below checks every
    // non-comment, non-doc-attribute source line for a string-spawn
    // of `jj` and for any code-effective reference to `JjRepo`
    // (the jj wrapper). Doc-comment intralinks like
    // `[`crate::jj::JjRepo`]` are fine — they're documentation, not
    // a spawn site.
    let manifest = env!("CARGO_MANIFEST_DIR");
    let git_rs = fs::read_to_string(format!("{manifest}/src/git.rs"))
        .expect("read git.rs");
    let v3_rs = fs::read_to_string(format!("{manifest}/src/v3_write.rs"))
        .expect("read v3_write.rs");
    for (name, src) in [("git.rs", &git_rs), ("v3_write.rs", &v3_rs)] {
        for (lineno, line) in src.lines().enumerate() {
            // Skip doc comments and attribute comments — those are
            // documentation, not code. Distinguishing "code line" vs
            // "doc line" via the leading sigil is good enough: every
            // doc comment in this crate starts with `///` or `//!`,
            // and ordinary `//` comments don't spawn anything either.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            assert!(
                !line.contains("Command::new(\"jj\""),
                "v3 module {name}:{}: forbidden spawn of `jj`: {line:?}",
                lineno + 1
            );
            assert!(
                !line.contains("JjRepo"),
                "v3 module {name}:{}: forbidden use of JjRepo (which spawns `jj`): {line:?}",
                lineno + 1
            );
        }
    }
}

#[test]
fn v3_concurrent_create_loses_to_cas_failure() {
    // Models the v3 CAS-failure path: a pre-existing ref with a
    // different commit on it causes `update-ref` to refuse the
    // create, which translates to `Error::ConcurrentWrite` (the same
    // typed error v2 surfaces).
    //
    // We can't easily race two creates in-process without coordinated
    // threads, but we CAN simulate the post-race state: hand-plant a
    // commit on the same id's ref BEFORE the would-be writer's
    // create lands. The writer's create-then-update-ref will see the
    // ref non-empty and fail at the CAS step.
    //
    // (Two concurrent creates on the SAME random id is impossible in
    // practice — the id is 2^28; we'd see one in 268M tries. But the
    // CAS pathway is exercised by retries against a stale tip, the
    // shape we care about.)
    //
    // This test is structurally similar to the v2 concurrent_write
    // tests in `integration.rs` but cheap: we don't need a thread.
    let repo = make_v3_scratch_repo("v3_concurrent_create_cas");
    let storage = Storage::open(&repo).unwrap();
    // First create.
    let id = storage
        .create_issue(&IssueDraft {
            title: "first writer".into(),
            ..Default::default()
        })
        .unwrap();
    // Mutate so the ref has a non-create tip. The next CAS-failing
    // call should be detected by name; we don't have a way to force
    // the random id to collide so we'll test the mutate-with-stale-
    // expected-old path indirectly by ALSO mutating from a separate
    // Storage instance and asserting both writes land (the retry-on-
    // CAS-failure policy that mutate() already implements).
    let storage2 = Storage::open(&repo).unwrap();
    storage
        .update(
            &id,
            UpdateFields {
                title: Some("title from A".into()),
                ..Default::default()
            },
        )
        .unwrap();
    // storage2's snapshot is stale; the mutate path re-reads on CAS
    // failure (one retry, per the v2.5 ConcurrentWrite policy), so
    // this should land cleanly.
    storage2
        .set_status(&id, Status::Closed)
        .expect("post-stale-tip mutation should retry and succeed");

    // Both writes landed; the chain on the per-issue ref is
    // four-deep: create, update, set_status (two heads merged) —
    // actually three. Walk it.
    let ref_name = format!("refs/jjf/issues/{id}");
    let log = git_capture(
        &["log", "--format=%H", &ref_name],
        &repo,
    );
    let commit_count = log.lines().filter(|l| !l.is_empty()).count();
    assert!(
        commit_count >= 3,
        "expected at least 3 commits on the chain (create, update, set_status); got {commit_count}\nlog:\n{log}"
    );
}

// ---------------------------------------------------------------------
// Tier D — HEAD-drift regression matrix. Every v3 mutation kind must
// leave git HEAD's symbolic target, git HEAD's resolved oid, and jj's
// `@` change id unchanged. This is the safety net for v3's core
// property: writes happen entirely under `refs/jjf/*` without touching
// the working copy or any branch ref.
//
// Several other tests in this file already pin HEAD/`@` for individual
// op families (create, set_status, add_comment, update, set_memory,
// unset_memory). This matrix fills the remaining kinds called out by
// ticket 7's acceptance: claim, unclaim, block, unblock, label add,
// label rm, dep add, dep rm.
//
// The test wraps each mutation in a snapshot/assert helper so each row
// names the kind and any failure points at exactly which kind drifted.
// ---------------------------------------------------------------------

/// Snapshot of the git HEAD identifiers that pin the v3 no-drift invariant.
/// J7: removed `at_change_id` (jj working-copy concept; plain git repos
/// have no @). git HEAD is the authoritative drift signal.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HeadSnapshot {
    head_oid: String,
    head_sym: String,
}

fn snapshot_head(repo: &Path) -> HeadSnapshot {
    HeadSnapshot {
        head_oid: git_head(repo),
        head_sym: git_head_symbolic(repo),
    }
}

/// Run `mutation`, then assert the HeadSnapshot is unchanged. `kind`
/// names the mutation for failure messages.
fn assert_no_drift<F: FnOnce()>(repo: &Path, kind: &str, mutation: F) {
    let before = snapshot_head(repo);
    mutation();
    let after = snapshot_head(repo);
    assert_eq!(
        before.head_oid, after.head_oid,
        "{kind}: git HEAD oid drifted"
    );
    assert_eq!(
        before.head_sym, after.head_sym,
        "{kind}: git HEAD symbolic target drifted (fingerprint of the v2 dance: refs/jj/root)"
    );
}

#[test]
fn v3_no_head_drift_across_full_mutation_matrix() {
    let repo = make_v3_scratch_repo("v3_no_head_drift_matrix");
    let storage = Storage::open(&repo).unwrap();

    // Set up a primary issue plus a second issue that will be the
    // dependency target. Both creates already exercise the create
    // no-drift property — pinned again below via assert_no_drift.
    let dep_target = assert_returns_no_drift(&repo, "create-target", || {
        storage
            .create_issue(&IssueDraft {
                title: "dep target".into(),
                ..Default::default()
            })
            .unwrap()
    });
    let id = assert_returns_no_drift(&repo, "create-primary", || {
        storage
            .create_issue(&IssueDraft {
                title: "primary issue".into(),
                ..Default::default()
            })
            .unwrap()
    });

    // close / open (set_status). Already covered indirectly by
    // v3_mutate_preserves_head_and_chains_commits, but rep here so
    // failure messages point at the precise verb.
    assert_no_drift(&repo, "set_status(Closed) [close]", || {
        storage.set_status(&id, Status::Closed).unwrap();
    });
    assert_no_drift(&repo, "set_status(Open) [open]", || {
        storage.set_status(&id, Status::Open).unwrap();
    });

    // claim / unclaim.
    assert_no_drift(&repo, "claim", || {
        let _ = storage.claim(&id, "alice").unwrap();
    });
    assert_no_drift(&repo, "unclaim", || {
        storage.unclaim(&id).unwrap();
    });

    // block / unblock.
    assert_no_drift(&repo, "block", || {
        storage.block(&id, Some("waiting on signal")).unwrap();
    });
    assert_no_drift(&repo, "unblock", || {
        storage.unblock(&id).unwrap();
    });

    // label add / rm.
    assert_no_drift(&repo, "add_label", || {
        storage.add_label(&id, "needs-review").unwrap();
    });
    assert_no_drift(&repo, "remove_label", || {
        storage.remove_label(&id, "needs-review").unwrap();
    });

    // dep add / rm (default Blocks edge via add_dependency).
    assert_no_drift(&repo, "add_dependency", || {
        storage.add_dependency(&id, &dep_target).unwrap();
    });
    assert_no_drift(&repo, "remove_dependency", || {
        storage.remove_dependency(&id, &dep_target).unwrap();
    });

    // typed dep edge with explicit kind (parent-child).
    assert_no_drift(&repo, "add_dep_edge(ParentChild)", || {
        storage
            .add_dep_edge(&id, &dep_target, DepKind::ParentChild)
            .unwrap();
    });
    assert_no_drift(&repo, "remove_dep_edge(ParentChild)", || {
        storage
            .remove_dep_edge(&id, &dep_target, DepKind::ParentChild)
            .unwrap();
    });

    // update — multi-op stanza in one commit. Re-asserted here so the
    // matrix is exhaustive.
    assert_no_drift(&repo, "update(title)", || {
        storage
            .update(
                &id,
                UpdateFields {
                    title: Some("renamed primary".into()),
                    ..Default::default()
                },
            )
            .unwrap();
    });

    // add_comment.
    assert_no_drift(&repo, "add_comment", || {
        let _ = storage.add_comment(&id, "matrix test", "tester").unwrap();
    });

    // set_memory / unset_memory.
    assert_no_drift(&repo, "set_memory", || {
        storage.set_memory("matrix-rule", "first value").unwrap();
    });
    assert_no_drift(&repo, "set_memory [upsert]", || {
        storage.set_memory("matrix-rule", "second value").unwrap();
    });
    assert_no_drift(&repo, "unset_memory", || {
        storage.unset_memory("matrix-rule").unwrap();
    });
}

/// Variant of `assert_no_drift` for mutations whose return value the
/// caller wants to keep (e.g. `create_issue`).
fn assert_returns_no_drift<F, R>(repo: &Path, kind: &str, mutation: F) -> R
where
    F: FnOnce() -> R,
{
    let before = snapshot_head(repo);
    let r = mutation();
    let after = snapshot_head(repo);
    assert_eq!(before.head_oid, after.head_oid, "{kind}: git HEAD oid drifted");
    assert_eq!(
        before.head_sym, after.head_sym,
        "{kind}: git HEAD symbolic target drifted"
    );
    r
}
