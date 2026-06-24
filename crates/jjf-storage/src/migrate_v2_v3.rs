//! v2 → v3 auto-migrator. Lands at first `Storage::open` on a v2-shape
//! repo (post v1 → v2 migration if needed).
//!
//! See `docs/storage-out-of-tree.md` §"v2 → v3 migration" for the spec.
//!
//! For each issue id reachable from the `issues` bookmark tip, we walk
//! the v2 op-chain (every commit that touched that issue's paths,
//! oldest-first) and re-land each commit on the v3 ref
//! `refs/jjf/issues/<id>`:
//!
//! - **Tree**: contains `issue.json` and (if present) `comments.jsonl`
//!   AT THAT POINT IN V2 HISTORY — i.e. the literal bytes of the v2
//!   file at that commit, byte-identical to the v3 read path's
//!   expected blob shape.
//! - **Message**: the v2 commit's message verbatim (summary line +
//!   blank line + trailer block). The trailer parser is shared across
//!   v2 and v3, so trailer semantics carry forward unchanged.
//! - **Parent**: the previous v3 commit on the same ref, or none for
//!   the first commit in the issue's chain (a v3 root commit).
//!
//! Memories follow the same shape on `refs/jjf/memories/<key>`.
//!
//! ## Idempotency
//!
//! The migration is NOT atomic across all issues. If it crashes mid-
//! migration, some refs are populated and some aren't, and the v3
//! sentinel ref hasn't been written. Recovery: next `Storage::open`
//! re-detects v2 (no sentinel) and re-runs. We make this safe per-
//! issue: if `refs/jjf/issues/<id>` ALREADY exists at the start of
//! a migration pass, that issue is treated as already-done and the
//! pass moves on. The sentinel ref planted at the end of the pass
//! is the idempotency marker for the whole repo.

use crate::git::GitRepo;
use crate::id::IssueId;
use crate::jj::JjRepo;
use crate::v3_write;
use crate::{
    issue_comments_relpath, issue_json_relpath, memory::memory_json_relpath,
    v1_issue_comments_relpath, v1_issue_json_relpath, Error, ISSUES_BOOKMARK,
    ISSUES_BOOKMARK_REVSET, Result,
};

/// One v2 commit on an issue's per-issue path filter — captured with
/// enough information to re-land as a v3 commit (snapshot bytes for
/// the tree, full message for the commit, commit id for traceability
/// / debugging).
#[derive(Debug)]
struct V2CommitOnIssue {
    /// v2 commit id — informational; used for debug logs / test
    /// assertions, not for chaining (the v3 ref chains by its own
    /// freshly-built commit oids).
    #[allow(dead_code)]
    commit: String,
    /// The full message text (summary + blank line + trailer block).
    /// Passed verbatim to `git commit-tree -F -` so the trailer
    /// stanza round-trips byte-identically.
    message: String,
    /// JSON record bytes at this commit, if the relevant `.json`
    /// file exists in the commit's tree. None means neither the v2
    /// path nor the v1 path is present at this commit — the commit
    /// touched the issue's `.comments.jsonl` sibling only (one of
    /// the same-second comment-append cases from spec §5.6).
    record_bytes: Option<Vec<u8>>,
    /// Comments-jsonl bytes at this commit, if any. `None` means no
    /// comments file in the tree at this point; `Some(empty)`
    /// distinguishes "explicit empty file" from "no file at all".
    /// In practice v2 always writes a `.comments.jsonl` at create
    /// time so by the second commit forward this is `Some(...)`,
    /// but pre-create ancestors (e.g. the seed) and v1 paths can
    /// legitimately be `None`.
    comments_bytes: Option<Vec<u8>>,
}

/// One v2 commit on a memory key's path — same shape as
/// [`V2CommitOnIssue`] but only carries the single `memory.json`
/// file (memories have no comments stream).
#[derive(Debug)]
struct V2CommitOnMemory {
    #[allow(dead_code)]
    commit: String,
    message: String,
    memory_bytes: Option<Vec<u8>>,
}

/// Public entry: run the v2 → v3 migration if the repo is in v2 shape.
///
/// Pre-conditions (caller's contract):
/// - The v3 sentinel ref is ABSENT (otherwise we'd be re-migrating).
///   The caller (`Storage::open`) only invokes us when detection said
///   `StorageMode::V2`.
/// - Any v1 → v2 step has already run; the `issues` bookmark either
///   exists (we migrate it) or doesn't (we no-op).
///
/// Post-conditions on success:
/// - Every issue id reachable from the v2 bookmark tip has a
///   `refs/jjf/issues/<id>` chain mirroring its v2 op log.
/// - Every memory key has a `refs/jjf/memories/<key>` chain.
/// - The `issues` bookmark is deleted.
/// - The `refs/jjf/meta/format-version` sentinel exists.
///
/// On error mid-migration: some refs may be populated; the sentinel
/// is NOT planted; the `issues` bookmark is NOT deleted. The next
/// `Storage::open` re-runs.
pub(crate) fn maybe_migrate_v2_to_v3(jj: &JjRepo, git: &GitRepo) -> Result<()> {
    // The sentinel-vs-bookmark dispatch happens in `Storage::open`;
    // by the time we're called, mode == V2. Still defensive: if the
    // bookmark is absent (fresh repo without v2 data, no v1 either),
    // this is a no-op — there's nothing to migrate.
    if !bookmark_exists(jj, ISSUES_BOOKMARK)? {
        return Ok(());
    }

    // Enumerate issue ids at the bookmark tip.
    let ids = list_issue_ids_v2(jj)?;
    for id in &ids {
        migrate_one_issue(jj, git, id)?;
    }

    // Enumerate memory keys at the bookmark tip.
    let keys = list_memory_keys_v2(jj)?;
    for key in &keys {
        migrate_one_memory(jj, git, key)?;
    }

    // Delete the v2 bookmark. We use `jj bookmark delete` so the jj
    // op-log records the deletion symmetrically with the v1 → v2
    // migrator's bookmark rename. The underlying git ref
    // `refs/heads/issues` goes with it.
    jj.run(&["bookmark", "delete", ISSUES_BOOKMARK])
        .map_err(Error::Jj)?;

    // Plant the v3 sentinel ref LAST. This is the idempotency marker:
    // a subsequent `Storage::open` sees the sentinel, returns
    // `StorageMode::V3`, and the migrator never re-runs on this repo.
    v3_write::write_format_version_sentinel(git)?;

    Ok(())
}

/// Does the named bookmark exist? Same check as the v1 → v2
/// detector's `Storage::bookmark_exists` but standalone — we don't
/// have a `Storage` handle here, just the raw wrappers.
fn bookmark_exists(jj: &JjRepo, name: &str) -> Result<bool> {
    let stdout = jj
        .run(&["bookmark", "list", "-T", "name ++ \"\\n\"", name])
        .map_err(Error::Jj)?;
    Ok(stdout.lines().any(|line| line.trim() == name))
}

/// Enumerate every issue id on the v2 `issues` bookmark by listing
/// `issues/<id>.json` files at the bookmark tip. Mirrors the
/// v1 → v2 walker's file-listing pattern.
fn list_issue_ids_v2(jj: &JjRepo) -> Result<Vec<IssueId>> {
    let listing = jj
        .run(&[
            "file",
            "list",
            "-r",
            ISSUES_BOOKMARK_REVSET,
            "-T",
            "path ++ \"\\n\"",
            "root:issues/",
        ])
        .map_err(Error::Jj)?;
    let mut ids: Vec<IssueId> = Vec::new();
    for line in listing.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("issues/") else {
            continue;
        };
        let Some(stem) = rest.strip_suffix(".json") else {
            continue;
        };
        if let Ok(id) = IssueId::parse(stem) {
            ids.push(id);
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Enumerate every memory key on the v2 `issues` bookmark by listing
/// `memories/<key>.json` files. Returns the keys (the `.json` stem)
/// sorted ascending.
fn list_memory_keys_v2(jj: &JjRepo) -> Result<Vec<String>> {
    let listing = match jj.run(&[
        "file",
        "list",
        "-r",
        ISSUES_BOOKMARK_REVSET,
        "-T",
        "path ++ \"\\n\"",
        "root:memories/",
    ]) {
        Ok(s) => s,
        Err(e) => {
            // `No such path: memories/` on a v2 repo with zero
            // memories. Treat as empty rather than failure.
            if let crate::jj::JjError::Cli { stderr, .. } = &e {
                if stderr.contains("No such path") {
                    return Ok(Vec::new());
                }
            }
            return Err(Error::Jj(e));
        }
    };
    let mut keys: Vec<String> = Vec::new();
    for line in listing.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("memories/") else {
            continue;
        };
        if let Some(stem) = rest.strip_suffix(".json") {
            if !stem.is_empty() && !stem.contains('/') {
                keys.push(stem.to_owned());
            }
        }
    }
    keys.sort();
    keys.dedup();
    Ok(keys)
}

/// Migrate one issue's op chain into a v3 ref. Idempotent: if the v3
/// ref already exists for this id, we treat the issue as already-done
/// and return immediately.
fn migrate_one_issue(jj: &JjRepo, git: &GitRepo, id: &IssueId) -> Result<()> {
    // Per-issue idempotency. If a prior migration pass crashed AFTER
    // landing this issue's ref but BEFORE planting the sentinel, the
    // next `Storage::open` re-detects v2 and re-enters here. We don't
    // want to double-write the chain.
    let v3_ref = v3_write::refs::issue_ref(id);
    if git
        .resolve_ref(&v3_ref)
        .map_err(Error::Git)?
        .is_some()
    {
        return Ok(());
    }

    let commits = walk_v2_issue_commits(jj, id)?;
    // Empty chain is theoretically impossible — the bookmark file
    // listing returned this id, so SOMETHING on the chain touched
    // `issues/<id>.json`. But defensively: skip if zero.
    if commits.is_empty() {
        return Ok(());
    }

    let mut parent: Option<String> = None;
    for c in &commits {
        // Build the per-commit tree. Each tree entry is hashed
        // independently, then assembled into a tree object. Empty
        // records / comments are skipped from the tree (a tree with
        // just `comments.jsonl` would happen e.g. on a comment-only
        // commit prior to the create — which doesn't really happen,
        // but defensively skip).
        let mut entries: Vec<(String, String, String)> = Vec::new();
        // `comments.jsonl` sorts BEFORE `issue.json` lexicographically;
        // git's `mktree` requires entries sorted by name.
        if let Some(bytes) = &c.comments_bytes {
            // Only include the comments blob if non-empty. Mirrors
            // the v3 write path's `commit_record_v3` which omits the
            // file when there are no comments. An empty `Vec<u8>`
            // (genuinely empty file) is still included — the byte
            // shape is what matters for tree-oid identity.
            //
            // Pragmatic choice: if a v2 commit's comments file
            // exists but is empty (the create-time stub), we DO
            // include it — that's the byte-identical migration
            // contract and we want post-migration `git cat-file blob
            // refs/jjf/issues/<id>:comments.jsonl` to return the
            // same bytes as v2's `jj file show issues/<id>.comments.jsonl`
            // returned pre-migration.
            let oid = git.hash_object(bytes).map_err(Error::Git)?;
            entries.push((
                "100644".to_owned(),
                v3_write::COMMENTS_JSONL_FILE.to_owned(),
                oid,
            ));
        }
        if let Some(bytes) = &c.record_bytes {
            let oid = git.hash_object(bytes).map_err(Error::Git)?;
            entries.push((
                "100644".to_owned(),
                v3_write::ISSUE_JSON_FILE.to_owned(),
                oid,
            ));
        }
        // Skip commits that produced an empty tree — they happen at
        // v1-path-rename commits where neither file is present under
        // the v2 path. The replay model: only build v3 commits that
        // carry actual content. (Empty trees would still be valid
        // git, but they introduce ghost commits in `git log
        // refs/jjf/issues/<id>` that don't correspond to any data
        // change.)
        //
        // However, the trailer block on such a commit COULD carry
        // an op for this issue (the create-time multi-op stanza in
        // particular). To avoid silently dropping ops, we still emit
        // the v3 commit BUT with an empty tree.  The trailer parser
        // round-trips the op regardless of the tree contents.
        let tree_entries_refs: Vec<(&str, &str, &str)> = entries
            .iter()
            .map(|(m, n, o)| (m.as_str(), n.as_str(), o.as_str()))
            .collect();
        let tree_oid = git.mktree(&tree_entries_refs).map_err(Error::Git)?;

        let parents: Vec<&str> = match &parent {
            Some(p) => vec![p.as_str()],
            None => vec![],
        };
        let new_commit = git
            .commit_tree(&tree_oid, &parents, &c.message)
            .map_err(Error::Git)?;
        parent = Some(new_commit);
    }

    // Atomically plant the v3 ref at the chain tip. We use
    // `update-ref <ref> <new> 0000...` for create-only semantics so a
    // concurrent migrator pass doesn't double-write. (We already
    // bailed on `resolve_ref` above; this is belt-and-braces.)
    if let Some(tip) = parent {
        git.update_ref(&v3_ref, &tip, crate::git::ZERO_OID)
            .map_err(Error::Git)?;
    }

    Ok(())
}

/// Walk the v2 op-chain for one issue, oldest-first. Returns one
/// entry per commit that touched any of:
///
/// - `issues/<id>.json`            (v2 path)
/// - `issues/<id>.comments.jsonl`  (v2 path)
/// - `bugs/<id>.json`              (v1 path; pre-migration ancestors)
/// - `bugs/<id>.comments.jsonl`    (v1 path)
///
/// Each entry carries the commit id, the full commit message, and
/// the snapshot bytes for the issue's `.json` and `.comments.jsonl`
/// files (from whichever path is present at that commit).
fn walk_v2_issue_commits(
    jj: &JjRepo,
    id: &IssueId,
) -> Result<Vec<V2CommitOnIssue>> {
    let json_relpath = issue_json_relpath(id);
    let comments_relpath = issue_comments_relpath(id);
    let v1_json_relpath = v1_issue_json_relpath(id);
    let v1_comments_relpath = v1_issue_comments_relpath(id);

    // Sentinel-separated template, mirroring `history::read_history_at`.
    // We need: commit_id and full description per commit, sorted
    // oldest-first.
    let field_sep = "----JJF-MIGRATE-V2V3-FIELD-c0ffee----";
    let record_sep = "\n----JJF-MIGRATE-V2V3-REC-c0ffee----\n";

    let template = format!(
        "commit_id ++ \"{f}\" ++ description ++ \"{r}\"",
        f = field_sep,
        r = record_sep.replace('\n', "\\n"),
    );

    let ancestors_rev = format!("ancestors({})", ISSUES_BOOKMARK_REVSET);
    let raw = jj
        .run(&[
            "log",
            "--no-graph",
            "-r",
            &ancestors_rev,
            "-T",
            &template,
            &format!("root:{}", json_relpath.display()),
            &format!("root:{}", comments_relpath.display()),
            &format!("root:{}", v1_json_relpath.display()),
            &format!("root:{}", v1_comments_relpath.display()),
        ])
        .map_err(Error::Jj)?;

    // jj emits newest-first; oldest-first is the chain order.
    let mut records: Vec<&str> = raw
        .split(record_sep)
        .filter(|s| !s.trim().is_empty())
        .collect();
    records.reverse();

    let mut out = Vec::new();
    for record in records {
        let parts: Vec<&str> = record.splitn(2, field_sep).collect();
        if parts.len() != 2 {
            return Err(Error::Invalid(format!(
                "v2→v3 migrate walker: bad record (got {} parts): {:?}",
                parts.len(),
                record
            )));
        }
        let commit = parts[0].trim_start_matches('\n').to_owned();
        let message = parts[1].trim_start_matches('\n').to_owned();
        // Trim the trailing newline that the template's `++ "{r}"`
        // leaves on `description`. The trailer-block contract is that
        // the final stanza ends in `\n`; we want that single trailing
        // newline preserved (it's part of the on-disk shape) but no
        // extras introduced by the template.
        let message = message.trim_end_matches('\n').to_owned() + "\n";

        // Read snapshot bytes at this commit. We try the v2 path
        // first; fall back to v1.
        let record_bytes = read_blob_bytes_at(jj, &commit, &json_relpath.display().to_string())?
            .or(read_blob_bytes_at(
                jj,
                &commit,
                &v1_json_relpath.display().to_string(),
            )?);
        let comments_bytes = read_blob_bytes_at(jj, &commit, &comments_relpath.display().to_string())?
            .or(read_blob_bytes_at(
                jj,
                &commit,
                &v1_comments_relpath.display().to_string(),
            )?);

        out.push(V2CommitOnIssue {
            commit,
            message,
            record_bytes,
            comments_bytes,
        });
    }

    Ok(out)
}

/// Migrate one memory key's op chain into a v3 ref. Idempotent on
/// per-key collision (see [`migrate_one_issue`] for the rationale).
fn migrate_one_memory(jj: &JjRepo, git: &GitRepo, key: &str) -> Result<()> {
    let v3_ref = v3_write::refs::memory_ref(key);
    if git
        .resolve_ref(&v3_ref)
        .map_err(Error::Git)?
        .is_some()
    {
        return Ok(());
    }

    let commits = walk_v2_memory_commits(jj, key)?;
    if commits.is_empty() {
        return Ok(());
    }

    let mut parent: Option<String> = None;
    for c in &commits {
        let mut entries: Vec<(String, String, String)> = Vec::new();
        if let Some(bytes) = &c.memory_bytes {
            let oid = git.hash_object(bytes).map_err(Error::Git)?;
            entries.push((
                "100644".to_owned(),
                v3_write::MEMORY_JSON_FILE.to_owned(),
                oid,
            ));
        }
        let tree_entries_refs: Vec<(&str, &str, &str)> = entries
            .iter()
            .map(|(m, n, o)| (m.as_str(), n.as_str(), o.as_str()))
            .collect();
        let tree_oid = git.mktree(&tree_entries_refs).map_err(Error::Git)?;
        let parents: Vec<&str> = match &parent {
            Some(p) => vec![p.as_str()],
            None => vec![],
        };
        let new_commit = git
            .commit_tree(&tree_oid, &parents, &c.message)
            .map_err(Error::Git)?;
        parent = Some(new_commit);
    }

    if let Some(tip) = parent {
        git.update_ref(&v3_ref, &tip, crate::git::ZERO_OID)
            .map_err(Error::Git)?;
    }

    Ok(())
}

/// Walk the v2 op-chain for one memory key, oldest-first.
fn walk_v2_memory_commits(
    jj: &JjRepo,
    key: &str,
) -> Result<Vec<V2CommitOnMemory>> {
    let json_relpath = memory_json_relpath(key);

    let field_sep = "----JJF-MIGRATE-V2V3-MEM-FIELD-c0ffee----";
    let record_sep = "\n----JJF-MIGRATE-V2V3-MEM-REC-c0ffee----\n";

    let template = format!(
        "commit_id ++ \"{f}\" ++ description ++ \"{r}\"",
        f = field_sep,
        r = record_sep.replace('\n', "\\n"),
    );

    let ancestors_rev = format!("ancestors({})", ISSUES_BOOKMARK_REVSET);
    let raw = jj
        .run(&[
            "log",
            "--no-graph",
            "-r",
            &ancestors_rev,
            "-T",
            &template,
            &format!("root:{}", json_relpath.display()),
        ])
        .map_err(Error::Jj)?;

    let mut records: Vec<&str> = raw
        .split(record_sep)
        .filter(|s| !s.trim().is_empty())
        .collect();
    records.reverse();

    let mut out = Vec::new();
    for record in records {
        let parts: Vec<&str> = record.splitn(2, field_sep).collect();
        if parts.len() != 2 {
            return Err(Error::Invalid(format!(
                "v2→v3 migrate memory walker: bad record (got {} parts): {:?}",
                parts.len(),
                record
            )));
        }
        let commit = parts[0].trim_start_matches('\n').to_owned();
        let message = parts[1].trim_start_matches('\n').to_owned();
        let message = message.trim_end_matches('\n').to_owned() + "\n";

        let memory_bytes = read_blob_bytes_at(jj, &commit, &json_relpath.display().to_string())?;

        out.push(V2CommitOnMemory {
            commit,
            message,
            memory_bytes,
        });
    }

    Ok(out)
}

/// Read the bytes of `path` at a specific jj commit. Returns
/// `Ok(None)` if the path is absent at that commit; bubbles up other
/// jj errors. We invoke `jj file show -r <commit> root:<path>` and
/// distinguish absence by the stderr signature.
fn read_blob_bytes_at(
    jj: &JjRepo,
    commit: &str,
    path: &str,
) -> Result<Option<Vec<u8>>> {
    match jj.run(&[
        "file",
        "show",
        "-r",
        commit,
        &format!("root:{}", path),
    ]) {
        Ok(s) => Ok(Some(s.into_bytes())),
        Err(e) => {
            if let crate::jj::JjError::Cli { stderr, .. } = &e {
                // jj's stable absence phrase across recent versions.
                // We also accept the lowercase variant defensively.
                let lc = stderr.to_ascii_lowercase();
                if lc.contains("no such path")
                    || lc.contains("does not exist")
                    || lc.contains("no file")
                {
                    return Ok(None);
                }
            }
            Err(Error::Jj(e))
        }
    }
}
