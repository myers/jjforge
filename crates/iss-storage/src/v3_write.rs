//! v3 write path — git-only, ref-shaped commit chains.
//!
//! Design pinned by `docs/storage-out-of-tree.md` "Write path". Each
//! mutation runs as:
//!
//! 1. Read the current snapshot from `refs/jjf/<ns>/<id>:<file>`.
//! 2. Apply the op in memory (the caller did this and gave us the new
//!    bytes).
//! 3. `git hash-object -w` each blob.
//! 4. `git mktree` to assemble the new tree.
//! 5. `git commit-tree` with the previous-tip as parent (or no parent
//!    for create).
//! 6. `git update-ref <ref> <new> <old>` — CAS-protected.
//!
//! Zero `jj` subprocess calls. The jj working copy is never touched.
//! git HEAD is never moved. See [`refs::issue_ref`] for the ref
//! namespace.
//!
//! This module holds the v3 write path. Every mutator on
//! [`crate::Storage`] routes here unconditionally — the v1/v2 shapes
//! and the `jj`-based commit dance they used were removed.

use crate::git::{GitRepo, ZERO_OID};
use crate::id::IssueId;
use crate::record::{Comment, IssueRecord, Memory};
use crate::{Error, Result};

/// Ref-name helpers. Centralized so callers don't sprinkle string
/// builders.
pub(crate) mod refs {
    use crate::id::IssueId;

    /// `refs/jjf/issues/<id>` — the per-issue commit-chain.
    pub(crate) fn issue_ref(id: &IssueId) -> String {
        format!("refs/jjf/issues/{}", id)
    }

    /// `refs/jjf/memories/<key>` — the per-memory commit-chain.
    pub(crate) fn memory_ref(key: &str) -> String {
        format!("refs/jjf/memories/{}", key)
    }

    /// `refs/jjf/meta/format-version` — the v3 sentinel ref. Presence
    /// is the v3-vs-v2 discriminator at `Storage::open` time. The
    /// pointed-to commit's blob holds a human-readable `version: 3`
    /// line, but reads only care about presence.
    pub(crate) const FORMAT_VERSION_REF: &str = "refs/jjf/meta/format-version";

    /// Prefix for `for_each_ref` enumeration.
    pub(crate) const ISSUES_PREFIX: &str = "refs/jjf/issues/";

    /// Prefix for `for_each_ref` enumeration on memories. Used by the
    /// v3 read path (ticket `6e2c843`) to list memory keys.
    pub(crate) const MEMORIES_PREFIX: &str = "refs/jjf/memories/";
}

/// Filename inside an issue ref's tree carrying the canonical record
/// JSON. (Spec: `docs/storage-out-of-tree.md` "Op log shape".)
pub(crate) const ISSUE_JSON_FILE: &str = "issue.json";
/// Sibling file carrying the comments stream. Optional — absent if
/// the issue has no comments.
pub(crate) const COMMENTS_JSONL_FILE: &str = "comments.jsonl";
/// Memory ref tree holds a single file with the JSON record. The
/// shape matches the v2 `memories/<key>.json` byte-for-byte.
pub(crate) const MEMORY_JSON_FILE: &str = "memory.json";

/// File mode for a regular-file blob in a tree. `mktree` requires
/// the four-component shape; `100644` is the canonical "non-exec
/// regular file" mode (matches what `git add` would produce).
const BLOB_MODE: &str = "100644";

/// Translate raw [`crate::git::GitError`]s on the write path into the
/// typed storage-layer [`Error`], detecting CAS / concurrent-write
/// failures and surfacing them as [`Error::ConcurrentWrite`] so the
/// higher-level retry policy in `Storage::mutate` / `add_comment`
/// recognizes them.
fn translate(e: crate::git::GitError) -> Error {
    if e.is_concurrent_write() {
        Error::ConcurrentWrite {
            hint: "another writer landed first. Retry your command.".into(),
        }
    } else {
        Error::Git(e)
    }
}

/// Build the per-issue tree from the new record + (possibly empty)
/// comments. Hashes both blobs, assembles a sorted-name tree, returns
/// the tree's oid. Comments are omitted from the tree iff `comments`
/// is empty AND `include_empty_comments` is false — for issues that
/// never had a comment, we don't want a phantom empty file in the
/// tree. For issues that ever had a comment we still write the file
/// (the writer might be removing the only comment); the caller passes
/// `Some(vec![])` to force the file.
pub(crate) fn build_issue_tree(
    repo: &GitRepo,
    record: &IssueRecord,
    comments: Option<&[Comment]>,
) -> Result<String> {
    let record_bytes = serialize_record_pretty(record)?;
    let record_oid = repo
        .hash_object(record_bytes.as_bytes())
        .map_err(translate)?;

    // Tree entries must be sorted lexicographically by name per git's
    // tree-object contract. `git mktree` will refuse an unsorted
    // input. We have at most two entries; explicit ordering is
    // cheaper than a sort.
    let mut entries: Vec<(String, String, String)> = Vec::new();

    // Hash the comments blob (if any). The bytes vec doesn't need
    // to outlive this match — we own the hex oid string from
    // `hash_object` and that's all the tree builder needs.
    let comments_oid_holder: Option<String> = match comments {
        Some(cs) => {
            let bytes = serialize_comments_jsonl(cs)?;
            let oid = repo.hash_object(bytes.as_bytes()).map_err(translate)?;
            Some(oid)
        }
        None => None,
    };

    // `comments.jsonl` < `issue.json` lexicographically — git's tree
    // sort treats them as bytes. Push comments first if present.
    if let Some(oid) = &comments_oid_holder {
        entries.push((
            BLOB_MODE.to_owned(),
            COMMENTS_JSONL_FILE.to_owned(),
            oid.clone(),
        ));
    }
    entries.push((BLOB_MODE.to_owned(), ISSUE_JSON_FILE.to_owned(), record_oid));

    let entries_refs: Vec<(&str, &str, &str)> = entries
        .iter()
        .map(|(m, n, o)| (m.as_str(), n.as_str(), o.as_str()))
        .collect();
    let tree_oid = repo.mktree(&entries_refs).map_err(translate)?;
    Ok(tree_oid)
}

/// Land a v3 issue mutation: read the previous tip, build the new
/// tree + commit, atomically update the ref. Returns the new
/// commit's oid (kept for tests / debug logging; non-load-bearing).
///
/// `comments` semantics:
/// - `None` — the issue has no comments file in its tree at all.
///   Used for newly-created issues with zero comments.
/// - `Some(&[])` — the issue's tree carries an empty comments file.
///   Currently unused (v2 always writes a comments file at create,
///   but v3 starts cleaner — see `commit_record_v3`).
/// - `Some(cs)` — the issue's tree carries `cs` rendered to JSONL.
pub(crate) fn commit_record_v3(
    repo: &GitRepo,
    id: &IssueId,
    record: &IssueRecord,
    comments: Option<&[Comment]>,
    message: &str,
) -> Result<String> {
    let ref_name = refs::issue_ref(id);
    let prev_tip = repo.resolve_ref(&ref_name).map_err(translate)?;
    let tree_oid = build_issue_tree(repo, record, comments)?;
    let parents: Vec<&str> = match &prev_tip {
        Some(p) => vec![p.as_str()],
        None => vec![],
    };
    let new_commit = repo
        .commit_tree(&tree_oid, &parents, message)
        .map_err(translate)?;
    let expected_old = prev_tip.as_deref().unwrap_or(ZERO_OID);
    repo.update_ref(&ref_name, &new_commit, expected_old)
        .map_err(translate)?;
    Ok(new_commit)
}

/// Land a v3 memory mutation: read the previous tip (if any), build a
/// one-file tree carrying the rendered memory JSON, land a new commit
/// with the v2 trailer shape (`Jjf-Op: set-memory` …), CAS-update the
/// ref.
///
/// For an `unset` op the caller passes `None` for `memory` — the new
/// commit's tree is empty (no files), preserving the audit chain.
pub(crate) fn commit_memory_v3(
    repo: &GitRepo,
    key: &str,
    memory: Option<&Memory>,
    message: &str,
) -> Result<String> {
    let ref_name = refs::memory_ref(key);
    let prev_tip = repo.resolve_ref(&ref_name).map_err(translate)?;
    let tree_oid = match memory {
        Some(mem) => {
            let bytes = serialize_memory_pretty(mem)?;
            let blob_oid = repo
                .hash_object(bytes.as_bytes())
                .map_err(translate)?;
            repo.mktree(&[(BLOB_MODE, MEMORY_JSON_FILE, &blob_oid)])
                .map_err(translate)?
        }
        None => {
            // Empty-tree oid via mktree with no entries. git's
            // canonical empty-tree oid is
            // 4b825dc642cb6eb9a060e54bf8d69288fbee4904 but `mktree`
            // with empty input is the same; we let git tell us.
            repo.mktree(&[]).map_err(translate)?
        }
    };
    let parents: Vec<&str> = match &prev_tip {
        Some(p) => vec![p.as_str()],
        None => vec![],
    };
    let new_commit = repo
        .commit_tree(&tree_oid, &parents, message)
        .map_err(translate)?;
    let expected_old = prev_tip.as_deref().unwrap_or(ZERO_OID);
    repo.update_ref(&ref_name, &new_commit, expected_old)
        .map_err(translate)?;
    Ok(new_commit)
}

/// Read the current `issue.json` blob from a v3 issue ref. Returns
/// `Err(Error::IssueNotFound)` if the ref doesn't exist (no issue
/// with this id) or the blob is absent (corrupt — every v3 issue ref
/// MUST carry `issue.json` in its tip's tree).
pub(crate) fn read_record_v3(
    repo: &GitRepo,
    id: &IssueId,
) -> Result<IssueRecord> {
    let ref_name = refs::issue_ref(id);
    let blob = repo
        .cat_blob(&ref_name, ISSUE_JSON_FILE)
        .map_err(translate)?
        .ok_or_else(|| Error::IssueNotFound(id.clone()))?;
    let text = String::from_utf8(blob).map_err(|e| {
        Error::Invalid(format!(
            "issue.json on {} was not valid UTF-8: {e}",
            ref_name
        ))
    })?;
    Ok(serde_json::from_str(&text)?)
}

/// Read `issue.json` from an arbitrary commit's tree (typically one of
/// the two parents of a pending merge). Returns `Ok(None)` if the file
/// is absent at that commit. Used by the pull-merge driver in
/// [`crate::sync_v3`] to fetch each parent's snapshot before reducing.
pub(crate) fn read_record_at_oid_v3(
    repo: &GitRepo,
    oid: &str,
) -> Result<Option<IssueRecord>> {
    let blob = repo
        .cat_blob(oid, ISSUE_JSON_FILE)
        .map_err(translate)?;
    let Some(b) = blob else { return Ok(None) };
    let text = String::from_utf8(b).map_err(|e| {
        Error::Invalid(format!(
            "issue.json on {} was not valid UTF-8: {e}",
            oid
        ))
    })?;
    Ok(Some(serde_json::from_str(&text)?))
}

/// Read `comments.jsonl` from an arbitrary commit's tree. Returns an
/// empty vec if the file is absent (an issue with no comments at that
/// revision). Companion to [`read_record_at_oid_v3`].
pub(crate) fn read_comments_at_oid_v3(
    repo: &GitRepo,
    oid: &str,
) -> Result<Vec<Comment>> {
    let blob = match repo.cat_blob(oid, COMMENTS_JSONL_FILE).map_err(translate)? {
        Some(b) => b,
        None => return Ok(Vec::new()),
    };
    let text = String::from_utf8(blob).map_err(|e| {
        Error::Invalid(format!(
            "comments.jsonl on {} was not valid UTF-8: {e}",
            oid
        ))
    })?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line)?);
    }
    Ok(out)
}

/// Build a multi-parent merge commit on a v3 issue ref carrying the
/// LWW-resolved record + comments in its tree and a `Jjf-Op: merge`
/// trailer in its message. CAS-updates `refs/jjf/issues/<id>` to point
/// at the new commit. Used by the pull-merge driver in
/// [`crate::sync_v3`] for the "diverged" scenario.
///
/// `parents` is the list of parent oids (typically `[local_tip,
/// remote_tip]` — two-parent — but the helper supports n-parent in case
/// a future caller wants multi-way merges). `record` and `comments`
/// MUST be the post-reduce LWW state, not either parent's individual
/// state — the read path's `cat_blob(<ref>, "issue.json")` reads the
/// tip's tree, so the merge commit's tree IS the resolved snapshot.
///
/// `expected_old` is the CAS sentinel — the current local-tip oid.
/// Concurrent writers landing between our read and our update-ref get
/// translated to [`Error::ConcurrentWrite`] by the standard `translate`
/// helper.
pub(crate) fn commit_merge_v3(
    repo: &GitRepo,
    id: &IssueId,
    record: &IssueRecord,
    comments: &[Comment],
    parents: &[&str],
    message: &str,
    expected_old: &str,
) -> Result<String> {
    let ref_name = refs::issue_ref(id);
    // For a merge the tree is the resolved snapshot. We always carry a
    // comments file (even if empty) so the merge commit looks
    // consistent on a `cat-file blob` of `comments.jsonl`: the file
    // either contains the unioned set or is empty. Passing `Some(&[])`
    // would force an empty file in the tree; we prefer the more
    // permissive "omit when empty" rule the writer already uses (no
    // phantom blob), so we project `Some(cs)` when non-empty and
    // `None` otherwise.
    let comments_arg: Option<&[Comment]> = if comments.is_empty() {
        None
    } else {
        Some(comments)
    };
    let tree_oid = build_issue_tree(repo, record, comments_arg)?;
    let new_commit = repo
        .commit_tree(&tree_oid, parents, message)
        .map_err(translate)?;
    repo.update_ref(&ref_name, &new_commit, expected_old)
        .map_err(translate)?;
    Ok(new_commit)
}

/// Read the current `comments.jsonl` blob from a v3 issue ref.
/// Returns an empty vec if the file is absent (the issue has no
/// comments) or empty.
pub(crate) fn read_comments_v3(
    repo: &GitRepo,
    id: &IssueId,
) -> Result<Vec<Comment>> {
    let ref_name = refs::issue_ref(id);
    let blob = match repo
        .cat_blob(&ref_name, COMMENTS_JSONL_FILE)
        .map_err(translate)?
    {
        Some(b) => b,
        None => return Ok(Vec::new()),
    };
    let text = String::from_utf8(blob).map_err(|e| {
        Error::Invalid(format!(
            "comments.jsonl on {} was not valid UTF-8: {e}",
            ref_name
        ))
    })?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line)?);
    }
    Ok(out)
}

/// Read the current `memory.json` blob from a v3 memory ref. Returns
/// `Ok(None)` if the ref is absent OR the most recent commit's tree
/// is empty (an `unset` op landed). The latter case happens when an
/// operator deletes a memory; the ref persists with the audit
/// history but the current state is "no value".
pub(crate) fn read_memory_v3(
    repo: &GitRepo,
    key: &str,
) -> Result<Option<Memory>> {
    let ref_name = refs::memory_ref(key);
    let blob = match repo
        .cat_blob(&ref_name, MEMORY_JSON_FILE)
        .map_err(translate)?
    {
        Some(b) => b,
        None => return Ok(None),
    };
    let text = String::from_utf8(blob).map_err(|e| {
        Error::Invalid(format!(
            "memory.json on {} was not valid UTF-8: {e}",
            ref_name
        ))
    })?;
    Ok(Some(serde_json::from_str(&text)?))
}

/// Does a v3 issue ref exist for this id? Used by the create path to
/// detect id collisions and pick a new random id.
pub(crate) fn issue_exists_v3(repo: &GitRepo, id: &IssueId) -> Result<bool> {
    Ok(repo
        .resolve_ref(&refs::issue_ref(id))
        .map_err(translate)?
        .is_some())
}

/// Enumerate every issue id present under `refs/jjf/issues/`. The
/// returned ids are sorted ascending by hex. Used to be the v3 read
/// path's enumeration primitive; ticket `4928ae6` replaced the
/// caller with `for_each_ref_with_type` so corrupt (non-commit)
/// refs surface as `UnreadableRef` instead of being silently
/// dropped at parse time. Kept for symmetry with `list_memory_keys_v3`
/// and as a potential building block for `iss doctor` (the heavier
/// follow-up to the lighter ls/ready warning).
#[allow(dead_code)]
pub(crate) fn list_issue_ids_v3(repo: &GitRepo) -> Result<Vec<IssueId>> {
    let refs = repo
        .for_each_ref(refs::ISSUES_PREFIX)
        .map_err(translate)?;
    let mut ids: Vec<IssueId> = Vec::with_capacity(refs.len());
    for r in refs {
        if let Some(stem) = r.strip_prefix(refs::ISSUES_PREFIX) {
            if let Ok(id) = IssueId::parse(stem) {
                ids.push(id);
            }
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Enumerate every memory key present under `refs/jjf/memories/`. The
/// returned keys are sorted ascending. Same symmetric story as
/// `list_issue_ids_v3`: superseded by the rebuild's direct
/// `for_each_ref_with_type` probe (ticket `4928ae6`), kept available
/// for future doctor-style verbs.
#[allow(dead_code)]
pub(crate) fn list_memory_keys_v3(repo: &GitRepo) -> Result<Vec<String>> {
    let refs = repo
        .for_each_ref(refs::MEMORIES_PREFIX)
        .map_err(translate)?;
    let mut keys: Vec<String> = Vec::with_capacity(refs.len());
    for r in refs {
        if let Some(stem) = r.strip_prefix(refs::MEMORIES_PREFIX) {
            // A memory key, like a slug, is restricted to `[a-z0-9-]+`
            // (see `crate::memory::slugify`); the for-each-ref output
            // shouldn't contain anything else, but skip anything with
            // a `/` defensively (subdirectories under memories aren't
            // a thing in v3, but a future schema change could add them
            // and we don't want to crash enumeration).
            if !stem.is_empty() && !stem.contains('/') {
                keys.push(stem.to_owned());
            }
        }
    }
    keys.sort();
    keys.dedup();
    Ok(keys)
}

/// Initialize the v3 sentinel ref. Idempotent — if the ref already
/// exists this is a no-op. The ref points at a parentless commit
/// whose tree carries a single `version` blob with the literal text
/// `version: 3\n`; only the ref's presence matters to the dispatch
/// logic, but the blob makes the ref self-describing.
///
/// Called by [`crate::Storage::init`] on a fresh repo (post-ticket
/// `add0646`) and (eventually) by the v2 → v3 migrator (ticket
/// `c14e1c1`).
pub(crate) fn write_format_version_sentinel(repo: &GitRepo) -> Result<()> {
    if repo
        .resolve_ref(refs::FORMAT_VERSION_REF)
        .map_err(translate)?
        .is_some()
    {
        return Ok(());
    }
    let blob_oid = repo
        .hash_object(b"version: 3\n")
        .map_err(translate)?;
    let tree_oid = repo
        .mktree(&[(BLOB_MODE, "version", &blob_oid)])
        .map_err(translate)?;
    let commit_oid = repo
        .commit_tree(&tree_oid, &[], "iss: storage format v3 sentinel\n")
        .map_err(translate)?;
    repo.update_ref(refs::FORMAT_VERSION_REF, &commit_oid, ZERO_OID)
        .map_err(translate)?;
    Ok(())
}

// ---- serialization helpers ------------------------------------------
//
// These mirror the v2 `write_record_json` / `write_comments_jsonl` /
// `write_memory_json` in `lib.rs` but produce in-memory bytes (the v3
// path hashes them via `git hash-object -w --stdin` rather than
// writing them to the working copy first). The byte shape is
// identical so the v2-vs-v3 cross-check in ticket `c14e1c1` (the
// migrator) can diff blobs byte-for-byte.

fn serialize_record_pretty(record: &IssueRecord) -> Result<String> {
    let mut s = serde_json::to_string_pretty(record)?;
    s.push('\n');
    Ok(s)
}

fn serialize_comments_jsonl(comments: &[Comment]) -> Result<String> {
    let mut s = String::new();
    for c in comments {
        s.push_str(&serde_json::to_string(c)?);
        s.push('\n');
    }
    Ok(s)
}

fn serialize_memory_pretty(mem: &Memory) -> Result<String> {
    let mut s = serde_json::to_string_pretty(mem)?;
    s.push('\n');
    Ok(s)
}
