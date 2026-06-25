//! Read-path snapshot cache.
//!
//! See `docs/storage-index-design.md` for the full rationale. In short:
//! every list-shaped read path (`Storage::list_ids`, `list_ready`,
//! `resolve`, `list_memories`, `dep_tree`) used to spawn one `jj file
//! show` per issue — ~15ms each. At N=1000 that's ~22 seconds per
//! `jjf ready`. The cache flips the read path to:
//!
//! 1. **Probe** the bookmark head with one `jj log` (~15ms).
//! 2. If `.jj/jjforge-cache.json` exists and its `head_commit` matches
//!    the live head, deserialize and return.
//! 3. Otherwise **rebuild**: one batched `jj file show` invocation with
//!    a sentinel-separated template that interleaves path and content
//!    for every file under `issues/` and `memories/`, parse in
//!    process, persist to disk.
//!
//! Cache hit cost is ~15ms regardless of N. Rebuild cost is one process
//! spawn plus parse time — sub-second at N=1000.
//!
//! ## What lives in the cache
//!
//! - Every `Issue` on the bookmark, keyed by `IssueId`.
//! - Every `Memory` on the bookmark, keyed by string.
//! - A `slug → id` index for cheap `Storage::resolve(slug)`.
//!
//! ## What does NOT live in the cache
//!
//! - The pre-migration v1 `bugs/` path. The v1 → v2 migration runs on
//!   `Storage::open` / `init` and the bookmark's tip thereafter only
//!   carries v2 paths. The history walker (`history.rs`) still scans
//!   v1 paths because per-issue history spans pre-migration commits;
//!   the snapshot cache only cares about the latest tree.
//!
//! ## Failure modes
//!
//! - Cache file missing → rebuild from scratch.
//! - Cache file corrupt or unparseable → log info on stderr, rebuild.
//! - Schema-version mismatch → rebuild.
//! - `.jj/` directory unwritable → cache is built in memory but not
//!   persisted; subsequent reads pay the rebuild cost again. We log
//!   one info-level line on stderr and keep going.
//!
//! ## Writers
//!
//! Writers do nothing special. Every mutation moves the `issues`
//! bookmark; the next read probes, sees the mismatch, and rebuilds.
//! No invalidation messages, no locks. The cache is pure derived
//! state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::git::GitRepo;
use crate::id::IssueId;
use crate::jj::JjRepo;
use crate::record::{Comment, Issue, IssueRecord, Memory};
use crate::v3_write;
use crate::{Error, Result, ISSUES_BOOKMARK_REVSET};

/// On-disk schema version. Bump when the [`SnapshotCache`] shape
/// changes in a way that pre-existing cache files can't reliably
/// deserialize. A mismatch triggers a rebuild from scratch — pure
/// derived state, no migration required.
pub(crate) const CACHE_SCHEMA_VERSION: u32 = 1;

/// Filename relative to `.jj/`. The `.jj/` directory is gitignored
/// by jj itself, so the cache is invisible to git by construction.
pub(crate) const CACHE_FILENAME: &str = "jjforge-cache.json";

/// Atomic-write temp suffix. We write to `.tmp` then rename so a
/// crashing process never leaves a half-written cache.
const CACHE_TEMP_SUFFIX: &str = ".tmp";

/// Sentinel that delimits the per-file blocks emitted by the batched
/// `jj file show` rebuild template. Deliberately verbose so no
/// legitimate JSON / JSONL line accidentally matches it.
const REBUILD_SENTINEL: &str = "--JJF-CACHE-SEP--";

/// On-disk discriminator for the storage layout a given cache file
/// was built against. Persisted as a string for forward-compat —
/// adding a future shape ("v4") doesn't have to invalidate existing
/// v2/v3 caches via a schema bump.
///
/// A cache file built on a v2-shape repo carries `"v2"`; a cache file
/// built on a v3-shape repo carries `"v3"`. On read, if the
/// `format_kind` doesn't match the current repo's storage shape the
/// cache is discarded and rebuilt — the two key spaces are not
/// comparable (a bookmark-tip sha and a ref-set sha are different
/// strings even for the same logical content).
pub(crate) const FORMAT_KIND_V2: &str = "v2";
pub(crate) const FORMAT_KIND_V3: &str = "v3";

/// `serde(default = ...)` requires a function reference, not a const,
/// so name the default explicitly. Old cache files (written before this
/// field existed) load as v2-shape — which matches their actual
/// provenance, since v3 caches didn't exist pre-cutover.
fn default_format_kind() -> String {
    FORMAT_KIND_V2.to_owned()
}

/// Diagnostic record for a single ref under `refs/jjf/issues/*` or
/// `refs/jjf/memories/*` that the snapshot-cache rebuild could not
/// turn into an [`Issue`] or [`Memory`].
///
/// Surfaces the silent-drop bug from ticket `4928ae6`: before this
/// type, a corrupt ref (one that didn't resolve to a commit whose
/// tree carried the expected blob) was simply skipped by `rebuild_v3`
/// and the read paths would emit a result set missing the affected
/// id — indistinguishable from the issue genuinely not existing.
///
/// The field shape is deliberately small: just the full ref name and
/// a one-line human-readable reason. Callers (the `jjf ls` and `jjf
/// ready` CLI verbs) format the list into a stderr warning so the
/// operator can see which refs went missing without poring over `git
/// for-each-ref` output.
///
/// `reason` is best-effort — it carries whatever the read path saw
/// when the blob fetch failed (a `git cat-file` stderr, a
/// `serde_json` parse error, a UTF-8 violation). It's diagnostic,
/// not machine-parseable; treat it as a string for the human.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnreadableRef {
    /// Full ref name (e.g. `refs/jjf/issues/eed62d7`).
    pub name: String,
    /// One-line human-readable reason. Pulled from the underlying
    /// `git cat-file` / parse / UTF-8 error, trimmed to the first
    /// line so multi-line stderr doesn't break the CLI's warning
    /// layout.
    pub reason: String,
}

/// Full snapshot of the `issues` bookmark tip.
///
/// Read-path callers materialize one of these per call (or per
/// process invocation if a higher-level layer chooses to memoize).
/// The struct deliberately uses `HashMap` rather than `BTreeMap`:
/// list-paths re-sort their projection anyway, and HashMap lookup
/// (`Storage::read`, `Storage::resolve`) is the common case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SnapshotCache {
    pub schema_version: u32,
    /// Which storage layout this cache was built against —
    /// [`FORMAT_KIND_V2`] or [`FORMAT_KIND_V3`]. `#[serde(default)]`
    /// makes pre-v3 cache files (which have no field) load as v2,
    /// preserving the operator's warm cache through the v3 upgrade.
    #[serde(default = "default_format_kind")]
    pub format_kind: String,
    /// Invalidation key for this cache.
    ///
    /// **V2 (`format_kind == "v2"`):** the jj commit-id (long form) of
    /// the `issues` bookmark tip at the time this snapshot was built.
    /// Probed via `jj log -r bookmarks(issues) -T commit_id --limit 1`.
    ///
    /// **V3 (`format_kind == "v3"`):** the SHA-1 hex of the concatenated
    /// `<refname> <objectname>\n` lines (sorted ascending by refname)
    /// for every ref under `refs/jjf/issues/` and `refs/jjf/memories/`.
    /// Probed via `git hash-object --stdin` over the
    /// `git for-each-ref --sort=refname --format='%(refname) %(objectname)' …`
    /// output. The sentinel ref `refs/jjf/meta/format-version` is
    /// excluded because it's planted once and never moves — including
    /// it would buy us nothing and complicate the rebuild story.
    ///
    /// The field name stays `head_commit` for backwards-compat with
    /// existing serialized cache files (renaming would invalidate
    /// every warm cache). The doc-string is the source of truth for
    /// its dual meaning.
    pub head_commit: String,
    /// Every `Issue` on the bookmark, keyed by id. Each Issue carries
    /// its comments inline (the same shape `Storage::read` returns).
    pub issues: HashMap<IssueId, Issue>,
    /// Every `Memory` on the bookmark, keyed by string key.
    pub memories: HashMap<String, Memory>,
    /// `slug → id` index so `Storage::resolve(slug)` is a HashMap
    /// lookup. Built from the same Issue records — redundant data
    /// but cheap; the slug is a short string.
    pub slug_index: HashMap<String, IssueId>,
    /// Per-ref diagnostics for refs under `refs/jjf/issues/*` /
    /// `refs/jjf/memories/*` that the rebuild encountered but could
    /// not parse into an [`Issue`] / [`Memory`]. Used by the CLI to
    /// emit a stderr warning when one or more refs got dropped from
    /// the result set (ticket `4928ae6`).
    ///
    /// `#[serde(default)]` and `skip_serializing_if = "Vec::is_empty"`
    /// keep the on-disk cache file shape backwards-compatible: an
    /// existing cache file with no `unreadable_refs` field loads as
    /// empty, and a current rebuild that found no corrupt refs
    /// doesn't write the field at all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable_refs: Vec<UnreadableRef>,
}

impl SnapshotCache {
    /// In-memory build for a V2-shape cache. Thin wrapper over
    /// [`SnapshotCache::from_parts_with_kind`] that pins the
    /// `format_kind` to [`FORMAT_KIND_V2`].
    pub(crate) fn from_parts(
        head_commit: String,
        issues: Vec<Issue>,
        memories: Vec<Memory>,
    ) -> Self {
        Self::from_parts_with_kind(
            FORMAT_KIND_V2.to_owned(),
            head_commit,
            issues,
            memories,
            Vec::new(),
        )
    }

    /// In-memory build from a format_kind + head + parsed issues +
    /// parsed memories. `format_kind` carries [`FORMAT_KIND_V2`] for
    /// caches built off the v2 `issues` bookmark or [`FORMAT_KIND_V3`]
    /// for caches built off the v3 `refs/jjf/*` set.
    ///
    /// `unreadable_refs` carries any per-ref diagnostics the rebuild
    /// accumulated for refs that couldn't be parsed into an
    /// [`Issue`] / [`Memory`]. Pass `Vec::new()` for a clean cache;
    /// the v3 rebuild populates this when it sees corrupt refs
    /// (ticket `4928ae6`).
    ///
    /// Used by both the rebuild paths and the tests.
    pub(crate) fn from_parts_with_kind(
        format_kind: String,
        head_commit: String,
        issues: Vec<Issue>,
        memories: Vec<Memory>,
        unreadable_refs: Vec<UnreadableRef>,
    ) -> Self {
        // Slug index. We populate from ACTIVE issues (Open or
        // InProgress) first; closed issues only fill empty slots.
        // Spec v2.1: closed issues release their slug, so an open
        // collision must win over a stale closed one. The
        // `find_open_slug_collision` probe relies on this — it
        // must see the OPEN holder, not whichever insertion order
        // a HashMap happened to pick.
        let mut slug_index: HashMap<String, IssueId> =
            HashMap::with_capacity(issues.len());
        use crate::record::Status;
        for issue in &issues {
            if !matches!(issue.status, Status::Open | Status::Blocked | Status::InProgress) {
                continue;
            }
            if let Some(slug) = &issue.slug {
                slug_index.insert(slug.clone(), issue.id.clone());
            }
        }
        for issue in &issues {
            if matches!(issue.status, Status::Open | Status::Blocked | Status::InProgress) {
                continue;
            }
            if let Some(slug) = &issue.slug {
                slug_index.entry(slug.clone()).or_insert_with(|| issue.id.clone());
            }
        }
        let issues_map: HashMap<IssueId, Issue> =
            issues.into_iter().map(|i| (i.id.clone(), i)).collect();
        let memories_map: HashMap<String, Memory> =
            memories.into_iter().map(|m| (m.key.clone(), m)).collect();
        SnapshotCache {
            schema_version: CACHE_SCHEMA_VERSION,
            format_kind,
            head_commit,
            issues: issues_map,
            memories: memories_map,
            slug_index,
            unreadable_refs,
        }
    }
}

/// Probe the current `issues` bookmark head commit id.
///
/// One `jj log` invocation, the same shape the rebuild detection uses.
/// Returns the trimmed `commit_id` string. Errors bubble up as
/// `Error::Jj` — the cache layer treats them as "fall back to file-read"
/// at the call site.
pub(crate) fn probe_head_commit(repo: &JjRepo) -> Result<String> {
    let out = repo.run(&[
        "log",
        "-r",
        ISSUES_BOOKMARK_REVSET,
        "-T",
        "commit_id",
        "--no-graph",
        "--limit",
        "1",
    ])?;
    Ok(out.trim().to_owned())
}

/// Path to the on-disk cache file: `<repo_root>/.jj/jjforge-cache.json`.
pub(crate) fn cache_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".jj").join(CACHE_FILENAME)
}

/// Load the cache from disk if it exists, is parseable, and is on
/// the current schema version. Returns `None` for any "missing or
/// unusable" case; the rebuild path treats `None` and a stale
/// `head_commit` identically.
///
/// Corrupt / unparseable / schema-mismatch cases emit one info-level
/// line on stderr — operators almost never want to debug a cache
/// rebuild, but if something's wrong it should leave a trace.
fn try_load_from_disk(repo_root: &Path) -> Option<SnapshotCache> {
    let path = cache_path(repo_root);
    let text = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                // Permissions or io error reading. Treat as a miss;
                // rebuild will try to write and produce its own
                // warning if persistence fails.
                eprintln!(
                    "jjforge: snapshot cache read failed ({}), rebuilding",
                    e
                );
            }
            return None;
        }
    };
    match serde_json::from_str::<SnapshotCache>(&text) {
        Ok(c) if c.schema_version == CACHE_SCHEMA_VERSION => Some(c),
        Ok(c) => {
            eprintln!(
                "jjforge: snapshot cache schema version {} != {}, rebuilding",
                c.schema_version, CACHE_SCHEMA_VERSION,
            );
            None
        }
        Err(e) => {
            eprintln!(
                "jjforge: snapshot cache corrupt ({}), rebuilding",
                e
            );
            None
        }
    }
}

/// Write the cache to disk atomically: write to a `.tmp` sibling
/// then `rename` over the real path. A crashing process or a
/// concurrent writer can never leave a half-written file.
///
/// Errors are logged at info level on stderr and otherwise ignored
/// — the cache is pure derived state and a write failure just
/// means the next read pays a rebuild cost. Notably we DON'T
/// surface the error to the caller; persistence is best-effort.
fn try_persist_to_disk(repo_root: &Path, cache: &SnapshotCache) {
    let final_path = cache_path(repo_root);
    let parent = match final_path.parent() {
        Some(p) => p,
        None => {
            eprintln!(
                "jjforge: snapshot cache path has no parent, not persisting"
            );
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(parent) {
        eprintln!(
            "jjforge: snapshot cache parent dir not creatable ({}), not persisting",
            e
        );
        return;
    }
    // Build `<final_path>.tmp` by appending — `with_extension` would
    // replace `.json` with `.tmp` and clash with concurrent cache
    // writes / readers if the schema ever grew a second cache file.
    let mut tmp_os = final_path.clone().into_os_string();
    tmp_os.push(CACHE_TEMP_SUFFIX);
    let tmp_path: PathBuf = tmp_os.into();
    let serialized = match serde_json::to_string(cache) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("jjforge: snapshot cache serialization failed ({})", e);
            return;
        }
    };
    if let Err(e) = std::fs::write(&tmp_path, serialized) {
        eprintln!(
            "jjforge: snapshot cache temp write failed ({}), not persisting",
            e
        );
        return;
    }
    if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
        eprintln!(
            "jjforge: snapshot cache rename failed ({}), not persisting",
            e
        );
        // Best-effort cleanup of the temp file.
        let _ = std::fs::remove_file(&tmp_path);
    }
}

/// Probe + load + rebuild as needed. Returns a fully-populated
/// SnapshotCache that matches the current bookmark tip.
///
/// Three paths:
///
/// 1. Cache file exists, schema matches, `head_commit == live_head`
///    → return loaded cache.
/// 2. Cache file missing / corrupt / schema mismatch / head mismatch
///    → rebuild via one batched `jj file show` invocation, persist
///    on success, return.
/// 3. Rebuild succeeds but persistence fails (e.g. `.jj/` non-writable)
///    → return the in-memory cache; next call pays the rebuild cost
///    again. We log to stderr.
pub(crate) fn load_or_rebuild(
    repo: &JjRepo,
    repo_root: &Path,
) -> Result<SnapshotCache> {
    let head = probe_head_commit(repo)?;
    if let Some(cache) = try_load_from_disk(repo_root) {
        if cache.format_kind == FORMAT_KIND_V2 && cache.head_commit == head {
            return Ok(cache);
        }
    }
    let cache = rebuild(repo, &head)?;
    try_persist_to_disk(repo_root, &cache);
    Ok(cache)
}

/// V3 counterpart to [`load_or_rebuild`]: probe the ref-set
/// fingerprint, load `.jj/jjforge-cache.json` on a key-match (and a
/// matching `format_kind`), rebuild via direct `git cat-file` reads
/// otherwise.
///
/// **Key.** SHA-1 hex of the sorted `<refname> <objectname>\n` lines
/// for every `refs/jjf/issues/*` and `refs/jjf/memories/*`. See
/// [`probe_ref_set_key_v3`]. The sentinel ref
/// `refs/jjf/meta/format-version` is deliberately excluded — it's
/// planted once and never moves, so including it would buy us
/// nothing.
///
/// **Cache file path.** Same `.jj/jjforge-cache.json` as v2 — the
/// `format_kind` field in the JSON discriminates the two key spaces.
/// A v2-shape cache file lying around on disk will be discarded
/// (key shape doesn't match v3's) and replaced atomically by the
/// rebuild's write.
///
/// **Persistence failure** is non-fatal: the in-memory cache is
/// returned, the next read pays the rebuild cost again, and stderr
/// carries the diagnostic line. Same shape as v2.
pub(crate) fn load_or_rebuild_v3(
    git: &GitRepo,
    repo_root: &Path,
) -> Result<SnapshotCache> {
    let key = probe_ref_set_key_v3(git)?;
    if let Some(cache) = try_load_from_disk(repo_root) {
        if cache.format_kind == FORMAT_KIND_V3 && cache.head_commit == key {
            return Ok(cache);
        }
    }
    let cache = rebuild_v3(git, &key)?;
    try_persist_to_disk(repo_root, &cache);
    Ok(cache)
}

/// Compute the v3 cache's invalidation key: SHA-1 hex of the
/// concatenated `<refname> <objectname>\n` lines (sorted ascending by
/// refname) for every `refs/jjf/issues/*` and `refs/jjf/memories/*`.
///
/// Uses `git hash-object --stdin` (no `-w`) so the key derivation
/// doesn't pollute the object database with cache-key blobs. The hash
/// is whatever git's content-addressed function is on this repo
/// (SHA-1 today, SHA-256 on `extensions.objectFormat = sha256` repos);
/// we treat it as an opaque fingerprint and only ever compare strings
/// for equality.
///
/// On a fresh repo with no issue refs and no memory refs the
/// for-each-ref output is empty. We still hash the empty input — git
/// hashes the empty blob to a deterministic oid — so the "no refs"
/// state has a stable cache key (`e69de29bb…` for SHA-1).
pub(crate) fn probe_ref_set_key_v3(git: &GitRepo) -> Result<String> {
    let pairs = git
        .for_each_ref_with_oid(&[v3_write::refs::ISSUES_PREFIX, v3_write::refs::MEMORIES_PREFIX])
        .map_err(Error::Git)?;
    let mut buf = String::with_capacity(pairs.len() * 90);
    for (name, oid) in &pairs {
        buf.push_str(name);
        buf.push(' ');
        buf.push_str(oid);
        buf.push('\n');
    }
    git.hash_object_no_write(buf.as_bytes()).map_err(Error::Git)
}

/// Rebuild the v3 cache by enumerating `refs/jjf/issues/*` +
/// `refs/jjf/memories/*` and reading each ref's tree blobs directly
/// via `git cat-file`.
///
/// Cost: one `for-each-ref` enumeration + N `cat-file` calls per
/// issue ref (one for `issue.json`, one for `comments.jsonl`) + M
/// `cat-file` calls per memory ref. For the ~30-issue planner this is
/// ~60 git spawns and lands under 200ms; for a 1000-issue corpus the
/// spawn cost dominates at ~1–2s. The follow-up optimization (Option
/// 2 in the ticket body) would replace the N `cat-file` calls with
/// one `cat-file --batch` pipeline; not yet — file as a future
/// ticket if the rebuild cost matters.
///
/// **Corrupt-ref diagnostics (ticket `4928ae6`).** A v3 issue ref
/// MUST resolve to a commit whose tree carries `issue.json`. If a ref
/// instead points at a blob, a commit with an empty tree, an
/// out-of-spec record, or otherwise fails to parse, we capture an
/// [`UnreadableRef`] on the cache rather than silently dropping the
/// id from the result set. The CLI's `ls` / `ready` verbs surface
/// these to stderr so an operator can see which refs went missing.
/// Same handling for memory refs.
///
/// Note: a hard CLI error (e.g. `git cat-file` exits with an
/// unrecognized stderr — neither "absent" nor a known parse failure)
/// still bubbles up via `?` and aborts the rebuild — those indicate
/// the repo itself is in trouble, not "one ref is bad".
pub(crate) fn rebuild_v3(git: &GitRepo, key: &str) -> Result<SnapshotCache> {
    let mut unreadable_refs: Vec<UnreadableRef> = Vec::new();

    // Issues: one ref per id, blob `issue.json` is required, blob
    // `comments.jsonl` is optional (absent ⇒ no comments yet).
    //
    // We enumerate with `%(objecttype)` so refs that point at a blob
    // / tree (the "corrupt ref" repro from ticket `4928ae6`) get
    // surfaced as unreadable up front, before we try to `cat-file`
    // their tree. The `cat_blob` helper translates a missing-path
    // result to `Ok(None)` for the legitimate "no comments yet" /
    // "memory tombstone" cases, which makes it impossible to tell a
    // blob-pointed ref from a healthy commit-pointed ref with an
    // absent path — so we do the object-type discrimination here.
    let issue_refs = git
        .for_each_ref_with_type(v3_write::refs::ISSUES_PREFIX)
        .map_err(Error::Git)?;
    let mut issue_ids: Vec<IssueId> = Vec::with_capacity(issue_refs.len());
    for (refname, objecttype) in issue_refs {
        let Some(stem) = refname.strip_prefix(v3_write::refs::ISSUES_PREFIX) else {
            continue;
        };
        let Ok(id) = IssueId::parse(stem) else {
            // Refname doesn't parse as a v3 issue id (e.g. someone
            // landed a stray ref under the prefix). Skip silently;
            // mirror `list_issue_ids_v3`'s tolerance.
            continue;
        };
        if objecttype != "commit" {
            unreadable_refs.push(UnreadableRef {
                name: refname.clone(),
                reason: format!(
                    "ref points at a {objecttype}, not a commit (expected a commit carrying issue.json)"
                ),
            });
            continue;
        }
        issue_ids.push(id);
    }
    issue_ids.sort();
    issue_ids.dedup();
    let mut issues: Vec<Issue> = Vec::with_capacity(issue_ids.len());
    for id in issue_ids {
        // `read_record_v3` returns `IssueNotFound` when the blob is
        // missing — most commonly because the ref doesn't resolve to
        // a commit at all (e.g. points at a blob) or its tree lacks
        // `issue.json`. Either way it's a corrupt ref from the
        // snapshot's POV: surface it via `unreadable_refs` so the
        // CLI can warn instead of silently dropping the id.
        let record: IssueRecord = match v3_write::read_record_v3(git, &id) {
            Ok(r) => r,
            Err(Error::IssueNotFound(_)) => {
                unreadable_refs.push(UnreadableRef {
                    name: format!("{}{}", v3_write::refs::ISSUES_PREFIX, id),
                    reason: "ref does not resolve to a commit carrying issue.json"
                        .to_owned(),
                });
                continue;
            }
            Err(Error::Json(e)) => {
                unreadable_refs.push(UnreadableRef {
                    name: format!("{}{}", v3_write::refs::ISSUES_PREFIX, id),
                    reason: format!("issue.json failed to parse: {}", first_line(&e.to_string())),
                });
                continue;
            }
            Err(Error::Invalid(msg)) => {
                unreadable_refs.push(UnreadableRef {
                    name: format!("{}{}", v3_write::refs::ISSUES_PREFIX, id),
                    reason: format!("issue.json invalid: {}", first_line(&msg)),
                });
                continue;
            }
            Err(Error::Git(e)) => {
                // Underlying `git cat-file` failure that the helper
                // didn't translate to "absent". Most plausible cause
                // here is a future ref-shape we don't expect; surface
                // as unreadable so the rebuild doesn't crash on one
                // odd ref.
                unreadable_refs.push(UnreadableRef {
                    name: format!("{}{}", v3_write::refs::ISSUES_PREFIX, id),
                    reason: format!("git read failed: {}", first_line(&e.to_string())),
                });
                continue;
            }
            Err(e) => return Err(e),
        };
        // Comments are optional; tolerate parse / utf-8 failure the
        // same way — record the ref as unreadable but keep the
        // issue itself (we already have the record).
        let mut comments = match v3_write::read_comments_v3(git, &id) {
            Ok(cs) => cs,
            Err(Error::Json(e)) => {
                unreadable_refs.push(UnreadableRef {
                    name: format!("{}{}", v3_write::refs::ISSUES_PREFIX, id),
                    reason: format!(
                        "comments.jsonl failed to parse: {}",
                        first_line(&e.to_string())
                    ),
                });
                Vec::new()
            }
            Err(Error::Invalid(msg)) => {
                unreadable_refs.push(UnreadableRef {
                    name: format!("{}{}", v3_write::refs::ISSUES_PREFIX, id),
                    reason: format!("comments.jsonl invalid: {}", first_line(&msg)),
                });
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let mut labels = record.labels;
        labels.sort();
        labels.dedup();
        let mut dependencies = record.dependencies;
        dependencies.sort();
        dependencies.dedup();
        issues.push(Issue {
            id: record.id,
            title: record.title,
            slug: record.slug,
            body: record.body,
            status: record.status,
            block_reason: record.block_reason,
            type_: record.type_,
            labels,
            dependencies,
            assignee: record.assignee,
            comments,
            created_at: record.created_at,
            updated_at: record.updated_at,
        });
    }

    // Memories: one ref per key, blob `memory.json` may be absent
    // (post-unset tombstone) — that's NOT a corrupt-ref signal, it's
    // the documented "memory has been cleared" state. Parse failures
    // and invalid-UTF8 on a present blob DO surface as unreadable.
    //
    // Same object-type up-front filter as the issue path: a memory
    // ref pointing at a blob looks identical to a tombstone via
    // `cat_blob` alone, so we discriminate here.
    let memory_refs = git
        .for_each_ref_with_type(v3_write::refs::MEMORIES_PREFIX)
        .map_err(Error::Git)?;
    let mut memory_keys: Vec<String> = Vec::with_capacity(memory_refs.len());
    for (refname, objecttype) in memory_refs {
        let Some(stem) = refname.strip_prefix(v3_write::refs::MEMORIES_PREFIX) else {
            continue;
        };
        // Match `list_memory_keys_v3`'s shape filter — defensively
        // skip anything with a `/` even though v3 keys don't carry them.
        if stem.is_empty() || stem.contains('/') {
            continue;
        }
        if objecttype != "commit" {
            unreadable_refs.push(UnreadableRef {
                name: refname.clone(),
                reason: format!(
                    "ref points at a {objecttype}, not a commit (expected a commit carrying memory.json)"
                ),
            });
            continue;
        }
        memory_keys.push(stem.to_owned());
    }
    memory_keys.sort();
    memory_keys.dedup();
    let mut memories: Vec<Memory> = Vec::with_capacity(memory_keys.len());
    for key in memory_keys {
        match v3_write::read_memory_v3(git, &key) {
            Ok(Some(mem)) => memories.push(mem),
            Ok(None) => { /* unset tombstone, not corrupt */ }
            Err(Error::Json(e)) => {
                unreadable_refs.push(UnreadableRef {
                    name: format!("{}{}", v3_write::refs::MEMORIES_PREFIX, key),
                    reason: format!(
                        "memory.json failed to parse: {}",
                        first_line(&e.to_string())
                    ),
                });
            }
            Err(Error::Invalid(msg)) => {
                unreadable_refs.push(UnreadableRef {
                    name: format!("{}{}", v3_write::refs::MEMORIES_PREFIX, key),
                    reason: format!("memory.json invalid: {}", first_line(&msg)),
                });
            }
            Err(Error::Git(e)) => {
                // A `git cat-file` failure that isn't translated to
                // "absent" by the cat_blob helper — e.g. the ref
                // resolves to a blob, not a commit. Surface as
                // unreadable rather than aborting the whole rebuild.
                unreadable_refs.push(UnreadableRef {
                    name: format!("{}{}", v3_write::refs::MEMORIES_PREFIX, key),
                    reason: format!("git read failed: {}", first_line(&e.to_string())),
                });
            }
            Err(e) => return Err(e),
        }
    }
    memories.sort_by(|a, b| a.key.cmp(&b.key));

    // Stable order so warnings and tests don't churn on HashMap
    // iteration order. Refnames are unique within the result set.
    unreadable_refs.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(SnapshotCache::from_parts_with_kind(
        FORMAT_KIND_V3.to_owned(),
        key.to_owned(),
        issues,
        memories,
        unreadable_refs,
    ))
}

/// Return the first line of a multi-line error string, stripped of
/// trailing whitespace. Used to keep the per-ref `reason` field
/// single-line so the CLI's stderr warning doesn't fan out across
/// the operator's terminal.
fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim_end().to_owned()
}

/// Rebuild the cache from the bookmark tip.
///
/// One batched `jj file show` invocation per top-level dir reads every
/// file in a single process spawn, with a sentinel-separated path-
/// then-content stream we parse in process. This replaces N spawns
/// (one per `.json` + one per `.comments.jsonl`) with two spawns —
/// the headline win that gets steady-state `list_ready` from O(N)
/// seconds to O(1) milliseconds.
pub(crate) fn rebuild(
    repo: &JjRepo,
    head_commit: &str,
) -> Result<SnapshotCache> {
    // We probe each top-level dir independently because `jj file show`
    // errors with "No such path" if any one filter doesn't exist (a
    // fresh repo with no memories yet, for example). Two probes is
    // still a constant — the per-N cost stays in the single-spawn
    // batched read below.
    let issues_blob = batched_show(repo, "issues/")?;
    let memories_blob = batched_show(repo, "memories/")?;

    let mut issue_records: HashMap<IssueId, IssueRecord> = HashMap::new();
    let mut issue_comments: HashMap<IssueId, Vec<Comment>> = HashMap::new();
    parse_issues_blob(&issues_blob, &mut issue_records, &mut issue_comments)?;

    let mut memories: Vec<Memory> = parse_memories_blob(&memories_blob)?;
    memories.sort_by(|a, b| a.key.cmp(&b.key));

    // Compose IssueRecord + comments → Issue, mirroring read.rs::read.
    let mut issues: Vec<Issue> = Vec::with_capacity(issue_records.len());
    for (id, record) in issue_records {
        let mut comments = issue_comments.remove(&id).unwrap_or_default();
        comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let mut labels = record.labels;
        labels.sort();
        labels.dedup();
        let mut dependencies = record.dependencies;
        dependencies.sort();
        dependencies.dedup();
        issues.push(Issue {
            id: record.id,
            title: record.title,
            slug: record.slug,
            body: record.body,
            status: record.status,
            block_reason: record.block_reason,
            type_: record.type_,
            labels,
            dependencies,
            assignee: record.assignee,
            comments,
            created_at: record.created_at,
            updated_at: record.updated_at,
        });
    }

    Ok(SnapshotCache::from_parts(
        head_commit.to_owned(),
        issues,
        memories,
    ))
}

/// One `jj file show` call against a directory under
/// `bookmarks(issues)`. Returns the raw blob (sentinel-separated path
/// + content). Empty if the directory doesn't exist at the revision
/// — that path is the absence-handler, NOT an error.
fn batched_show(repo: &JjRepo, dir: &str) -> Result<String> {
    let tmpl = format!(
        "\"\\n{sep}\\n\" ++ path ++ \"\\n{sep}\\n\"",
        sep = REBUILD_SENTINEL
    );
    // `root:<dir>` pins the path to repo-root-relative (the same
    // shape `list_ids` uses for `jj file list`) so a subprocess
    // invoked from any cwd doesn't try to climb to an absolute
    // path.
    let path_arg = format!("root:{}", dir);
    match repo.run(&[
        "file",
        "show",
        "-r",
        ISSUES_BOOKMARK_REVSET,
        "-T",
        &tmpl,
        &path_arg,
    ]) {
        Ok(s) => Ok(s),
        Err(e) => {
            // `No such path: <dir>` (and historic variants) means
            // the directory has no files at this revision. Treat
            // as empty; only re-raise other jj failures.
            if let crate::jj::JjError::Cli { stderr, .. } = &e {
                if stderr.contains("No such path") {
                    return Ok(String::new());
                }
            }
            Err(Error::Jj(e))
        }
    }
}

/// Parse the sentinel-separated blob into per-file records.
///
/// Blob shape (one segment per file):
/// ```text
///
/// --JJF-CACHE-SEP--
/// issues/<id>.json
/// --JJF-CACHE-SEP--
/// <json content>
/// ```
///
/// The leading newline is absorbed by the iterator's logic; we
/// tolerate it.
fn parse_issues_blob(
    blob: &str,
    records: &mut HashMap<IssueId, IssueRecord>,
    comments: &mut HashMap<IssueId, Vec<Comment>>,
) -> Result<()> {
    for (path, content) in iter_sentinel_blob(blob) {
        if let Some(stem) = path.strip_prefix("issues/") {
            // Order matters: `.comments.jsonl` ends in `.json` (well,
            // `.jsonl`, but defensively), so we check the longer
            // suffix first.
            if let Some(id_str) = stem.strip_suffix(".comments.jsonl") {
                if let Ok(id) = IssueId::parse(id_str) {
                    let mut cs = Vec::new();
                    for line in content.lines() {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        cs.push(serde_json::from_str(line)?);
                    }
                    comments.insert(id, cs);
                }
                continue;
            }
            if let Some(id_str) = stem.strip_suffix(".json") {
                if let Ok(id) = IssueId::parse(id_str) {
                    let record: IssueRecord = serde_json::from_str(content)?;
                    records.insert(id, record);
                }
                continue;
            }
        }
        // Files we don't recognize (future schema extensions, stray
        // artifacts) are skipped silently — mirrors `list_ids`'s
        // tolerance.
    }
    Ok(())
}

/// Parse the memories blob into `Memory` records. Same sentinel
/// shape as `parse_issues_blob`, single file class.
fn parse_memories_blob(blob: &str) -> Result<Vec<Memory>> {
    let mut out = Vec::new();
    for (path, content) in iter_sentinel_blob(blob) {
        if let Some(stem) = path.strip_prefix("memories/") {
            if let Some(_key) = stem.strip_suffix(".json") {
                let mem: Memory = serde_json::from_str(content)?;
                out.push(mem);
            }
        }
    }
    Ok(out)
}

/// Iterate `(path, content)` pairs out of the sentinel-separated blob
/// emitted by `batched_show`'s template.
fn iter_sentinel_blob(blob: &str) -> impl Iterator<Item = (&str, &str)> {
    SentinelBlobIter::new(blob)
}

/// Splits the blob on the `--JJF-CACHE-SEP--\n<path>\n--JJF-CACHE-SEP--\n`
/// segments. Path is the line between two sentinels; content runs from
/// the byte after the second sentinel's newline up to the next
/// `\n--JJF-CACHE-SEP--\n` or EOF.
struct SentinelBlobIter<'a> {
    rest: &'a str,
}

impl<'a> SentinelBlobIter<'a> {
    fn new(blob: &'a str) -> Self {
        Self { rest: blob }
    }
}

impl<'a> Iterator for SentinelBlobIter<'a> {
    type Item = (&'a str, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        let sep = REBUILD_SENTINEL;
        // Find next sentinel (header open).
        let open_idx = self.rest.find(sep)?;
        let after_open = &self.rest[open_idx + sep.len()..];
        let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);
        // Path runs up to the next sentinel.
        let close_idx = after_open.find(sep)?;
        let path = after_open[..close_idx].trim_end_matches('\n');
        let after_close = &after_open[close_idx + sep.len()..];
        let after_close = after_close.strip_prefix('\n').unwrap_or(after_close);
        // Content runs up to the next sentinel or EOF. The next
        // sentinel is preceded by a `\n` (from our template's leading
        // `\n` in `"\n<sep>\n"`); trim one trailing `\n` from content.
        let next_idx = after_close.find(sep);
        let (content_end_in_after_close, advance_target) = match next_idx {
            Some(i) => (i, i),
            None => (after_close.len(), after_close.len()),
        };
        let content = after_close[..content_end_in_after_close]
            .trim_end_matches('\n');
        // Compute absolute index in `self.rest` to advance to.
        let after_close_offset =
            after_close.as_ptr() as usize - self.rest.as_ptr() as usize;
        self.rest = &self.rest[after_close_offset + advance_target..];
        Some((path, content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_iter_handles_empty_blob() {
        let v: Vec<_> = iter_sentinel_blob("").collect();
        assert!(v.is_empty());
    }

    #[test]
    fn sentinel_iter_extracts_single_file() {
        let blob = "\n--JJF-CACHE-SEP--\nissues/aabbccd.json\n--JJF-CACHE-SEP--\n{\"version\":2,\"id\":\"aabbccd\"}\n";
        let v: Vec<_> = iter_sentinel_blob(blob).collect();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].0, "issues/aabbccd.json");
        assert_eq!(v[0].1, "{\"version\":2,\"id\":\"aabbccd\"}");
    }

    #[test]
    fn sentinel_iter_extracts_multiple_files() {
        let blob = "\n--JJF-CACHE-SEP--\nissues/a.json\n--JJF-CACHE-SEP--\nCONTENT-A\n--JJF-CACHE-SEP--\nissues/b.json\n--JJF-CACHE-SEP--\nCONTENT-B\n";
        let v: Vec<_> = iter_sentinel_blob(blob).collect();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], ("issues/a.json", "CONTENT-A"));
        assert_eq!(v[1], ("issues/b.json", "CONTENT-B"));
    }

    #[test]
    fn sentinel_iter_handles_multiline_content() {
        let blob = "\n--JJF-CACHE-SEP--\nissues/a.json\n--JJF-CACHE-SEP--\n{\n  \"id\":\"a\"\n}\n--JJF-CACHE-SEP--\nissues/b.json\n--JJF-CACHE-SEP--\nB\n";
        let v: Vec<_> = iter_sentinel_blob(blob).collect();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], ("issues/a.json", "{\n  \"id\":\"a\"\n}"));
        assert_eq!(v[1], ("issues/b.json", "B"));
    }

    #[test]
    fn snapshot_from_parts_builds_slug_index() {
        let head = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_owned();
        let id_a = IssueId::parse("aabbccd").unwrap();
        let id_b = IssueId::parse("eeff001").unwrap();
        let issue_a = Issue {
            id: id_a.clone(),
            title: "a".into(),
            slug: Some("slug-a".into()),
            body: String::new(),
            status: crate::record::Status::Open,
            block_reason: None,
            type_: crate::record::IssueType::Unspecified,
            labels: Vec::new(),
            dependencies: Vec::new(),
            assignee: None,
            comments: Vec::new(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        };
        let issue_b = Issue {
            slug: None,
            id: id_b.clone(),
            ..issue_a.clone()
        };
        let cache = SnapshotCache::from_parts(head.clone(), vec![issue_a, issue_b], Vec::new());
        assert_eq!(cache.head_commit, head);
        assert_eq!(cache.issues.len(), 2);
        assert_eq!(cache.slug_index.get("slug-a"), Some(&id_a));
        assert_eq!(cache.slug_index.len(), 1);
    }
}
