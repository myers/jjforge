//! Op-space merge driver: replay each head's op chain, reduce
//! field-by-field with the LWW ordering tuple defined in spec §6, and
//! render the merged issue record. The file becomes a deterministic
//! projection of the op stream — there is no body-text "unmergeable"
//! failure mode, and label add/remove from different heads composes
//! by causal order rather than the v1 file-bytes driver's set-union
//! approximation.
//!
//! This is the v2 driver that supersedes `jjf-merge` on the operator
//! pull path. `jjf-merge` stays in the workspace as a library for
//! non-jjforge consumers (a vanilla `jj resolve`-style invocation,
//! future tooling) and as a parser-behavior fixture; the operator
//! path goes through this module instead.
//!
//! # The ordering tuple
//!
//! Every op in a head's history sorts by
//!
//!     (jjf_at if Some else commit_time, commit, trailer_index)
//!
//! - `jjf_at` is the writer's `now_rfc3339_nanos()` stamp embedded in
//!   the `Jjf-At:` trailer added in spec §5 for the op-space bump.
//! - `commit_time` is jj's author timestamp (second resolution) — the
//!   fallback for stanzas predating the op-time bump. Per the
//!   orchestrator's spec call (issue `bfc732b` comment `b9f5c27`),
//!   pre-bump stanzas sort *before* stamped ops at the same second,
//!   which is the desired migration semantics (older data loses to
//!   newer data).
//! - `commit` is the 40-hex commit_id; deterministic across clones.
//! - `trailer_index` is the 0-based stanza position within the
//!   commit; the final tiebreaker for ops sharing a commit (every
//!   multi-op commit has at least two stanzas with the same
//!   `(jjf_at, commit)`).
//!
//! # Body-hash lookup
//!
//! `Op::SetBody` carries only `body_hash` (spec §5.2). The op-space
//! reducer picks the winning hash; the body bytes themselves come
//! from whichever head's rendered `issues/<id>.json` matches that
//! hash. Both heads might match if they shared the body op (the
//! bytes are identical either way); at least one head will match by
//! construction since `SetBody` always lands on top of a record write.

use std::collections::{BTreeMap, BTreeSet};

use crate::history::HistoryEntry;
use crate::id::IssueId;
use crate::jj::JjRepo;
use crate::op::Op;
use crate::record::{Comment, DepEdge, DepKind, IssueRecord, IssueType};
use crate::{issue_comments_relpath, issue_json_relpath, Error, Result};

/// One issue after the op-space reducer has folded all heads' chains
/// into a single winning state. The merged on-disk file
/// (`issues/<id>.json`) is `record` serialized canonically; the merged
/// comments file is `comments` written one JSON-per-line.
#[derive(Debug, Clone)]
pub struct MergedIssue {
    pub id: IssueId,
    pub record: IssueRecord,
    pub comments: Vec<Comment>,
}

/// What [`super::Storage::resolve_divergence`] returns: one
/// [`MergedIssue`] per issue that needed resolution. An issue that
/// exists only on one head still appears here so the caller can write
/// its bytes into the merge commit's working copy without
/// special-casing.
#[derive(Debug, Clone)]
pub struct MergeReport {
    pub issues: Vec<MergedIssue>,
}

/// The reducer's ordering key. Total order across all (head, op)
/// tuples in any divergent bookmark.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct OpKey {
    /// `Some(jjf_at)` for stanzas emitted post-spec-bump; `None`
    /// otherwise. We project to `(0, jjf_at)` for `Some` and
    /// `(0, commit_time)` for `None` so the ordering tuple is uniform
    /// across both, with stamped ops always winning ties at the same
    /// commit-time second per the orchestrator's spec call.
    primary: PrimaryKey,
    commit: String,
    trailer_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PrimaryKey {
    /// Pre-spec-bump stanza: only commit-time is available. Sorts
    /// before any stamped op at the same second.
    Unstamped(String),
    /// Post-spec-bump stanza: nano-precision op-time.
    Stamped(String),
}

impl OpKey {
    fn from_entry(entry: &HistoryEntry) -> Self {
        let primary = match &entry.jjf_at {
            Some(at) => PrimaryKey::Stamped(at.clone()),
            None => PrimaryKey::Unstamped(entry.timestamp.clone()),
        };
        OpKey {
            primary,
            commit: entry.commit.clone(),
            trailer_index: entry.trailer_index,
        }
    }
}

/// Sort a slice of [`HistoryEntry`] in spec §6 LWW order:
/// `(jjf_at if Some else commit_time, commit, trailer_index)`. Shared
/// with the read-path cross-check in `read.rs` so file-read and
/// op-replay land on identical projections of the same op chain.
pub(crate) fn sort_entries_lww(entries: &mut [HistoryEntry]) {
    entries.sort_by(|a, b| OpKey::from_entry(a).cmp(&OpKey::from_entry(b)));
}

/// Run the op-space resolver against the given heads. Returns one
/// [`MergedIssue`] per issue touched by any head's chain.
///
/// Errors:
/// - `IssueNotFound` is impossible here — we only consider issues that
///   at least one head saw, and `read_history_at` only returns
///   `IssueNotFound` when we ask it about an issue no commit touches.
/// - `Jj` if any of the `jj log` / `jj file show` shell-outs fail.
pub(crate) fn resolve(repo: &JjRepo, heads: &[String]) -> Result<MergeReport> {
    if heads.is_empty() {
        return Ok(MergeReport { issues: Vec::new() });
    }

    // 1. Enumerate every issue id that appears on any head. Reuse the
    //    same `issues/` directory listing pattern `list_ids` uses, but
    //    parameterized on the head rev so we don't accidentally
    //    enumerate the bookmark tip's view.
    let mut issue_ids: BTreeSet<IssueId> = BTreeSet::new();
    for head in heads {
        for id in list_ids_at(repo, head)? {
            issue_ids.insert(id);
        }
    }

    let mut out: Vec<MergedIssue> = Vec::new();
    for id in issue_ids {
        let merged = resolve_one(repo, heads, &id)?;
        out.push(merged);
    }

    Ok(MergeReport { issues: out })
}

/// List every issue id present in `issues/<id>.json` at the given rev.
fn list_ids_at(repo: &JjRepo, rev: &str) -> Result<Vec<IssueId>> {
    let text = repo.run(&[
        "file",
        "list",
        "-r",
        rev,
        "-T",
        "path ++ \"\\n\"",
        "root:issues/",
    ])?;
    let mut ids: Vec<IssueId> = Vec::new();
    for line in text.lines() {
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
    Ok(ids)
}

/// Per-head structural snapshot the pure reducer consumes. The reducer
/// uses the op chain for everything that can be op-replayed
/// (title/status/labels/…); the rendered files provide body bytes
/// (matched to the winning `SetBody` hash) and comment bodies
/// (matched to `CommentAdd` ids).
///
/// Lifted out of `resolve_one` so the pure-reduction step
/// [`reduce_to_merged`] is reachable from the unit-test module without
/// a real `JjRepo`. The full operator path still goes through
/// `resolve_one` → `reduce_to_merged`; this struct is the seam between
/// the I/O loader and the pure folder.
pub(crate) struct HeadSnapshot {
    pub record: Option<IssueRecord>,
    pub comments: Vec<Comment>,
    pub entries: Vec<HistoryEntry>,
}

/// Replay one issue across every head and produce the merged
/// [`MergedIssue`].
///
/// For each head:
/// - Walk its op chain via `read_history_at`.
/// - Read its rendered record + comments file (if present) so we can
///   look up body bytes by hash and union comment bodies by id.
fn resolve_one(repo: &JjRepo, heads: &[String], id: &IssueId) -> Result<MergedIssue> {
    let mut snapshots: Vec<HeadSnapshot> = Vec::with_capacity(heads.len());
    for head in heads {
        let entries = match super::history::read_history_at(repo, head, id) {
            Ok(v) => v,
            Err(Error::IssueNotFound(_)) => Vec::new(),
            Err(e) => return Err(e),
        };
        let record = read_record_at(repo, head, id)?;
        let comments = read_comments_at(repo, head, id)?;
        snapshots.push(HeadSnapshot {
            record,
            comments,
            entries,
        });
    }

    reduce_to_merged(id, &snapshots)
}

/// Pure reducer: given each head's `(entries, record, comments)`
/// snapshot, produce the merged [`MergedIssue`] per spec §6's
/// per-field policy.
///
/// Steps:
/// 1. Flatten every head's entries, de-dup by `(commit, trailer_index)`
///    (the same op may appear on multiple heads if both heads share
///    that commit's ancestry — e.g. ops before the fork), and sort by
///    the [`OpKey`] tuple.
/// 2. Initialize the record from the first op (must be `Create`).
///    Reduce field-by-field:
///    - `SetTitle`/`SetStatus`/`SetAssignee`/`SetType`/`SetSlug`/
///      `SetBody`: LWW — the last op in the sorted stream wins.
///    - `LabelAdd`/`LabelRm`, `DepAdd`/`DepRm`: causal order; last
///      operation per `(label / dep)` decides presence.
///    - `CommentAdd`: union of `comment_id`s (no conflict possible).
///    - `Merge`: marker; no-op for state.
/// 3. Resolve the winning `SetBody` hash against the heads' rendered
///    body bytes. No `SetBody` at all: take the body from whichever
///    head has a record (create-time body, multi-op create only emits
///    `SetBody` when non-empty).
/// 4. Union comment bodies by id, ordered by `created_at` ascending.
///
/// Errors:
/// - `Invalid` if the chain is empty (shouldn't happen — `resolve_one`
///   only calls us for ids that surfaced on at least one head).
/// - `Invalid` if the first op isn't `Create`.
/// - `Invalid` if the winning `body_hash` isn't present on any head's
///   rendered record.
pub(crate) fn reduce_to_merged(
    id: &IssueId,
    snapshots: &[HeadSnapshot],
) -> Result<MergedIssue> {
    // 1. Flatten + dedup + sort.
    let mut all_entries: Vec<HistoryEntry> = Vec::new();
    let mut seen: BTreeSet<(String, u32)> = BTreeSet::new();
    for snap in snapshots {
        for entry in &snap.entries {
            let key = (entry.commit.clone(), entry.trailer_index);
            if seen.insert(key) {
                all_entries.push(entry.clone());
            }
        }
    }
    all_entries.sort_by(|a, b| OpKey::from_entry(a).cmp(&OpKey::from_entry(b)));

    if all_entries.is_empty() {
        // Both heads agree the issue doesn't exist. Should be
        // unreachable because list_ids_at only surfaced ids present
        // on at least one head; we synthesize a sensible error rather
        // than panic.
        return Err(Error::Invalid(format!(
            "resolve: issue {} not present on any head, yet enumerated",
            id
        )));
    }

    // 2. Initialize from the first op (must be `Create`).
    let mut record = match &all_entries[0].op {
        Op::Create {
            issue_id,
            title,
            status,
        } => IssueRecord {
            version: 2,
            id: issue_id.clone(),
            title: title.clone(),
            slug: None,
            body: String::new(),
            status: *status,
            block_reason: None,
            type_: IssueType::Unspecified,
            priority: None,
            labels: Vec::new(),
            metadata: BTreeMap::new(),
            dependencies: Vec::new(),
            assignee: None,
            // Seed both timestamps from the create op's stamp; the
            // updated_at gets bumped as we replay later ops.
            created_at: created_at_for(&all_entries[0]),
            updated_at: created_at_for(&all_entries[0]),
        },
        _ => {
            return Err(Error::Invalid(format!(
                "resolve: issue {} chain does not start with `create`",
                id
            )));
        }
    };

    // Per-label / per-dep last-write tracker. A `LabelAdd` writes
    // `true`, a `LabelRm` writes `false`; final pass takes the keys
    // whose final value is `true`.
    //
    // v2.4: the dep tracker is keyed by `(target, kind)` so the four
    // kinds compose independently. Same causal algorithm as labels;
    // wider key. A v1 stanza (no kind tag) materialized through the
    // parser carries `kind: Blocks`, so old data composes with new
    // under this same key without special-casing.
    let mut label_state: BTreeMap<String, bool> = BTreeMap::new();
    let mut dep_state: BTreeMap<(IssueId, DepKind), bool> = BTreeMap::new();
    // Per-key metadata last-write tracker. `SetMetadata` writes
    // `Some(value)`, `UnsetMetadata` writes `None`; final pass keeps
    // the keys whose final value is `Some`. Same causal algorithm as
    // labels; the value (not just presence) is carried.
    let mut metadata_state: BTreeMap<String, Option<String>> = BTreeMap::new();

    // Track the latest SetBody op's hash so we can look up the
    // matching head's body bytes once the reduce is done.
    let mut latest_body_hash: Option<String> = None;

    let mut comment_ids: Vec<IssueId> = Vec::new();

    for entry in &all_entries {
        // Bump updated_at to the latest op's stamp. Use the op-time
        // when present (nano-resolution) but truncated to seconds for
        // the record (spec §3.1 stays second-res); the op-time stays
        // nano in the trailer.
        record.updated_at = updated_at_for(entry);
        match &entry.op {
            Op::Create { .. } => {
                // Already initialized; ignore later creates (a well-
                // formed chain has exactly one). The orchestrator's
                // spec call expects we'd assert this — but if a
                // divergent edit somehow produced two creates we'd
                // rather pick the earliest deterministically than
                // fail closed.
            }
            Op::SetTitle { title, .. } => record.title = title.clone(),
            Op::SetStatus { status, .. } => record.status = *status,
            Op::SetAssignee { assignee, .. } => {
                record.assignee = assignee.clone();
            }
            Op::SetType { kind, .. } => {
                record.type_ = *kind;
            }
            Op::SetSlug { slug, .. } => {
                record.slug = slug.clone();
            }
            Op::SetPriority { priority, .. } => {
                // v2.8: scalar LWW — same shape as type/slug. Existing
                // `(jjf_at, commit, trailer_index)` total order picks
                // the winning op; we just overwrite.
                record.priority = *priority;
            }
            Op::SetBlockReason { reason, .. } => {
                // v2.5: scalar LWW — same shape as title/slug. The
                // resolver's existing `(jjf_at, commit, trailer_index)`
                // total order picks the winning op; we just overwrite.
                record.block_reason = reason.clone();
            }
            Op::SetBody { body_hash, .. } => {
                latest_body_hash = Some(body_hash.clone());
            }
            Op::LabelAdd { label, .. } => {
                label_state.insert(label.clone(), true);
            }
            Op::LabelRm { label, .. } => {
                label_state.insert(label.clone(), false);
            }
            Op::SetMetadata { key, value, .. } => {
                metadata_state.insert(key.clone(), Some(value.clone()));
            }
            Op::UnsetMetadata { key, .. } => {
                metadata_state.insert(key.clone(), None);
            }
            Op::DepAdd { dep, kind, .. } => {
                dep_state.insert((dep.clone(), *kind), true);
            }
            Op::DepRm { dep, kind, .. } => {
                dep_state.insert((dep.clone(), *kind), false);
            }
            Op::CommentAdd { comment_id, .. } => {
                comment_ids.push(comment_id.clone());
            }
            Op::Merge { .. } => {
                // No-op — the parents' chains are authoritative.
            }
        }
    }

    // Project final label / dep state.
    let mut labels: Vec<String> = label_state
        .into_iter()
        .filter_map(|(l, present)| if present { Some(l) } else { None })
        .collect();
    labels.sort();
    record.labels = labels;

    // Project final metadata state — keep only keys whose final write
    // was a `Some`. `BTreeMap::collect` yields sorted keys.
    record.metadata = metadata_state
        .into_iter()
        .filter_map(|(k, v)| v.map(|val| (k, val)))
        .collect();

    let mut deps: Vec<DepEdge> = dep_state
        .into_iter()
        .filter_map(|((target, kind), present)| {
            if present {
                Some(DepEdge { target, kind })
            } else {
                None
            }
        })
        .collect();
    // Stable sort by (target, kind) — the BTreeMap iteration order
    // already gives us this, but the projection is explicit so the
    // on-disk order is independent of the map impl.
    deps.sort();
    record.dependencies = deps;

    // 3. Body-hash lookup. The winning op's hash IS in at least one
    //    head's rendered file; pluck the bytes from there.
    if let Some(hash) = &latest_body_hash {
        let mut found: Option<String> = None;
        for snap in snapshots {
            if let Some(rec) = &snap.record {
                if crate::body_hash_hex(&rec.body) == *hash {
                    found = Some(rec.body.clone());
                    break;
                }
            }
        }
        match found {
            Some(b) => record.body = b,
            None => {
                // Neither head has the winning body bytes — should be
                // impossible for v1 (SetBody always lands on top of
                // a record write that includes the body). Surface a
                // typed error rather than a panic.
                return Err(Error::Invalid(format!(
                    "resolve: issue {} winning body hash {} not present on any head",
                    id, hash
                )));
            }
        }
    }
    // No SetBody op at all: the create-time body lives in the
    // rendered record (the multi-op create emits SetBody only if the
    // body is non-empty). Take it from whichever head has the record.
    if latest_body_hash.is_none() {
        if let Some(rec) = snapshots
            .iter()
            .find_map(|s| s.record.as_ref())
        {
            record.body = rec.body.clone();
        }
    }

    // 4. Comment union. Each `CommentAdd` op references a comment_id;
    //    the actual body lives in whichever head's `.comments.jsonl`
    //    has that id. Take each comment from the first head that
    //    carries it (both heads' bytes are identical for shared ops).
    //    Sort by `created_at` ascending per spec §4.2 (file is
    //    append-only by created_at).
    let mut seen_comments: BTreeSet<IssueId> = BTreeSet::new();
    let mut comments: Vec<Comment> = Vec::new();
    for cid in &comment_ids {
        if !seen_comments.insert(cid.clone()) {
            continue;
        }
        let mut picked: Option<Comment> = None;
        for snap in snapshots {
            if let Some(c) = snap.comments.iter().find(|c| &c.id == cid) {
                picked = Some(c.clone());
                break;
            }
        }
        if let Some(c) = picked {
            comments.push(c);
        }
        // A comment-id with no body bytes is a spec violation
        // (`CommentAdd` always lands alongside the .comments.jsonl
        // append in one commit). We silently drop rather than fail
        // because there's nothing the operator can do about it.
    }
    comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    Ok(MergedIssue {
        id: id.clone(),
        record,
        comments,
    })
}

/// Read `issues/<id>.json` at `rev`. Returns `Ok(None)` if the file is
/// absent at that revision (e.g. one head deleted it, which v1 doesn't
/// actually support but we tolerate defensively).
fn read_record_at(repo: &JjRepo, rev: &str, id: &IssueId) -> Result<Option<IssueRecord>> {
    let relpath = issue_json_relpath(id);
    let text = match repo.run(&[
        "file",
        "show",
        "-r",
        rev,
        &format!("root:{}", relpath.display()),
    ]) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    Ok(Some(serde_json::from_str(&text)?))
}

/// Read `issues/<id>.comments.jsonl` at `rev`. Missing file => no
/// comments. JSON-line errors bubble up as `Error::Json`.
fn read_comments_at(repo: &JjRepo, rev: &str, id: &IssueId) -> Result<Vec<Comment>> {
    let relpath = issue_comments_relpath(id);
    let text = match repo.run(&[
        "file",
        "show",
        "-r",
        rev,
        &format!("root:{}", relpath.display()),
    ]) {
        Ok(s) => s,
        Err(_) => return Ok(Vec::new()),
    };
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

/// Pick the `created_at` value from an entry: prefer the truncated
/// op-time (second resolution per spec §3.1), fall back to the
/// commit-time when the stanza has no `Jjf-At:`.
fn created_at_for(entry: &HistoryEntry) -> String {
    match &entry.jjf_at {
        Some(at) => truncate_to_seconds(at),
        None => entry.timestamp.clone(),
    }
}

fn updated_at_for(entry: &HistoryEntry) -> String {
    created_at_for(entry)
}

/// Strip the fractional-seconds suffix from an RFC 3339 nano stamp,
/// leaving the second-resolution form spec §3.1 requires for record
/// timestamps. Input shape: `YYYY-MM-DDTHH:MM:SS.fffffffffZ` → output
/// shape: `YYYY-MM-DDTHH:MM:SSZ`. Inputs without a fractional part
/// are returned verbatim.
fn truncate_to_seconds(rfc_nano: &str) -> String {
    // Find the `.` separator and the trailing `Z`. If either is
    // missing, return the input unchanged — defensive against malformed
    // trailers (the spec mandates the shape, but a tolerant parser is
    // the right move for a derived format).
    let Some(dot_idx) = rfc_nano.find('.') else {
        return rfc_nano.to_owned();
    };
    if !rfc_nano.ends_with('Z') {
        return rfc_nano.to_owned();
    }
    let mut out = String::with_capacity(rfc_nano.len() - 10);
    out.push_str(&rfc_nano[..dot_idx]);
    out.push('Z');
    out
}

/// Re-derive the sha-256 hex of a body string. Used by the op-space
/// reducer to match a winning `SetBody` op's hash to its head's body
/// bytes. Mirrors the same hash the storage writer computes at
/// `set_body` time.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Status;

    // ---- helpers --------------------------------------------------------

    /// A fixed 7-hex issue id reused across tests. The reducer never
    /// looks at the id semantically; any valid id works.
    fn iid(s: &str) -> IssueId {
        IssueId::parse(s).expect("test issue id is 7 lowercase hex")
    }

    /// Build a `HistoryEntry` carrying the given op. `jjf_at` is the
    /// nano-precision op-time stamp the post-spec-bump writer emits;
    /// `None` simulates a pre-bump (v1-trailer) stanza. `timestamp` is
    /// the commit's author second; the reducer falls back to it when
    /// `jjf_at` is `None`. `commit` is the 40-hex commit_id; the
    /// reducer uses it as a tiebreaker and to dedup ops shared across
    /// heads. `trailer_index` is the 0-based stanza position within
    /// the commit; the final tiebreaker when `(jjf_at, commit)` ties.
    fn entry(
        commit: &str,
        timestamp: &str,
        jjf_at: Option<&str>,
        trailer_index: u32,
        op: Op,
    ) -> HistoryEntry {
        HistoryEntry {
            commit: commit.into(),
            author: "Test <t@example.com>".into(),
            timestamp: timestamp.into(),
            jjf_at: jjf_at.map(|s| s.into()),
            trailer_index,
            op,
        }
    }

    /// Build a `Create` entry rooted at `t0`. Tests build a chain on
    /// top of this seed; the reducer requires the first sorted op to
    /// be `Create` or it errors out.
    fn create_entry(id: &IssueId, t0: &str) -> HistoryEntry {
        entry(
            "00000000000000000000000000000000000000a0",
            t0,
            Some(t0),
            0,
            Op::Create {
                issue_id: id.clone(),
                title: "initial".into(),
                status: Status::Open,
            },
        )
    }

    /// Pre-bump (v1-trailer) Create entry — no `Jjf-At` stamp. Used by
    /// tests that mix unstamped and stamped ops on the same chain: in
    /// the implementation, *every* unstamped op (`PrimaryKey::Unstamped`)
    /// sorts before *every* stamped op (`PrimaryKey::Stamped`)
    /// regardless of clock time (see the variant declaration order on
    /// `PrimaryKey`). So mixing a stamped Create with unstamped ops
    /// makes the unstamped op sort BEFORE the Create — the chain
    /// errors with "does not start with `create`." For cross-version
    /// scenarios that mirror real data (where v1 ops predate the v2
    /// spec bump), the Create is also v1-style: unstamped.
    fn unstamped_create_entry(id: &IssueId, t0: &str) -> HistoryEntry {
        entry(
            "00000000000000000000000000000000000000a0",
            t0,
            None,
            0,
            Op::Create {
                issue_id: id.clone(),
                title: "initial".into(),
                status: Status::Open,
            },
        )
    }

    /// Wrap entries into a single-head snapshot. Tests that don't
    /// exercise body or comment lookup can use this — they leave the
    /// rendered record and comments empty.
    fn snap(entries: Vec<HistoryEntry>) -> HeadSnapshot {
        HeadSnapshot {
            record: None,
            comments: Vec::new(),
            entries,
        }
    }

    /// Run the reducer on the given head snapshots and unwrap.
    fn reduce(id: &IssueId, snapshots: &[HeadSnapshot]) -> MergedIssue {
        reduce_to_merged(id, snapshots).expect("reducer should succeed")
    }

    // ---- existing tests -------------------------------------------------

    #[test]
    fn truncate_drops_nanos() {
        assert_eq!(
            truncate_to_seconds("2026-06-22T12:00:00.123456789Z"),
            "2026-06-22T12:00:00Z"
        );
    }

    #[test]
    fn truncate_no_nanos_is_identity() {
        assert_eq!(
            truncate_to_seconds("2026-06-22T12:00:00Z"),
            "2026-06-22T12:00:00Z"
        );
    }

    #[test]
    fn op_key_stamped_beats_unstamped_at_same_second() {
        let unstamped = OpKey {
            primary: PrimaryKey::Unstamped("2026-06-22T12:00:00Z".into()),
            commit: "aaa".into(),
            trailer_index: 0,
        };
        let stamped = OpKey {
            primary: PrimaryKey::Stamped("2026-06-22T12:00:00.000000001Z".into()),
            commit: "aaa".into(),
            trailer_index: 0,
        };
        // PrimaryKey::Unstamped variant comes first in declaration
        // order, so derived Ord puts unstamped < stamped — which is
        // exactly the orchestrator's "older data loses to newer data"
        // semantics.
        assert!(unstamped < stamped);
    }

    // ---- LWW scalars: status -------------------------------------------

    #[test]
    fn status_lww_later_jjf_at_wins() {
        // Two heads: head A closes the issue at t1, head B leaves it
        // open at t2. By the LWW rule, head B's later op wins.
        let id = iid("1111111");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::SetStatus {
                    issue_id: id.clone(),
                    status: Status::Closed,
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:02Z",
                Some("2026-06-22T12:00:02.000000000Z"),
                0,
                Op::SetStatus {
                    issue_id: id.clone(),
                    status: Status::Open,
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.status, Status::Open);
    }

    #[test]
    fn status_tie_break_by_commit_hash() {
        // Same Jjf-At on both heads; the larger commit-id string wins.
        let id = iid("2222222");
        let at = "2026-06-22T12:00:01.000000000Z";
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aaaa",
                "2026-06-22T12:00:01Z",
                Some(at),
                0,
                Op::SetStatus {
                    issue_id: id.clone(),
                    status: Status::Closed,
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bbbb",
                "2026-06-22T12:00:01Z",
                Some(at),
                0,
                Op::SetStatus {
                    issue_id: id.clone(),
                    status: Status::Open,
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        // "bbbb" > "aaaa" lexicographically, so head_b's op wins.
        assert_eq!(merged.record.status, Status::Open);
    }

    #[test]
    fn status_tie_break_by_trailer_index() {
        // One commit carries two SetStatus stanzas — the higher
        // trailer_index wins.
        let id = iid("3333333");
        let at = "2026-06-22T12:00:01.000000000Z";
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "cc",
                "2026-06-22T12:00:01Z",
                Some(at),
                0,
                Op::SetStatus {
                    issue_id: id.clone(),
                    status: Status::Closed,
                },
            ),
            entry(
                "cc",
                "2026-06-22T12:00:01Z",
                Some(at),
                1,
                Op::SetStatus {
                    issue_id: id.clone(),
                    status: Status::Open,
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a]);
        assert_eq!(merged.record.status, Status::Open);
    }

    #[test]
    fn status_unstamped_loses_to_stamped_at_same_second() {
        // Head A's op is pre-bump (no Jjf-At). Head B's op stamps at
        // the same author-second. The stamped op wins. The Create
        // itself is unstamped — see `unstamped_create_entry`'s
        // docstring for why mixing stamped Create + unstamped op
        // breaks the "first sorted op must be Create" invariant.
        let id = iid("4444444");
        let head_a = snap(vec![
            unstamped_create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                None,
                0,
                Op::SetStatus {
                    issue_id: id.clone(),
                    status: Status::Closed,
                },
            ),
        ]);
        let head_b = snap(vec![
            unstamped_create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000001Z"),
                0,
                Op::SetStatus {
                    issue_id: id.clone(),
                    status: Status::Open,
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.status, Status::Open);
    }

    // ---- LWW scalars: title --------------------------------------------

    #[test]
    fn title_lww_later_wins() {
        let id = iid("5555555");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::SetTitle {
                    issue_id: id.clone(),
                    title: "from A".into(),
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:02Z",
                Some("2026-06-22T12:00:02.000000000Z"),
                0,
                Op::SetTitle {
                    issue_id: id.clone(),
                    title: "from B".into(),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.title, "from B");
    }

    #[test]
    fn title_tie_break_by_commit_hash() {
        let id = iid("6666666");
        let at = "2026-06-22T12:00:01.000000000Z";
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aaaa",
                "2026-06-22T12:00:01Z",
                Some(at),
                0,
                Op::SetTitle {
                    issue_id: id.clone(),
                    title: "from A".into(),
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bbbb",
                "2026-06-22T12:00:01Z",
                Some(at),
                0,
                Op::SetTitle {
                    issue_id: id.clone(),
                    title: "from B".into(),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.title, "from B");
    }

    #[test]
    fn title_tie_break_by_trailer_index() {
        let id = iid("7777777");
        let at = "2026-06-22T12:00:01.000000000Z";
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "cc",
                "2026-06-22T12:00:01Z",
                Some(at),
                0,
                Op::SetTitle {
                    issue_id: id.clone(),
                    title: "first stanza".into(),
                },
            ),
            entry(
                "cc",
                "2026-06-22T12:00:01Z",
                Some(at),
                1,
                Op::SetTitle {
                    issue_id: id.clone(),
                    title: "second stanza".into(),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a]);
        assert_eq!(merged.record.title, "second stanza");
    }

    #[test]
    fn title_unstamped_loses_to_stamped_at_same_second() {
        // Pre-bump Create (no Jjf-At) so mixing unstamped + stamped
        // ops doesn't break the "first op must be Create" invariant.
        let id = iid("8888888");
        let head_a = snap(vec![
            unstamped_create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                None,
                0,
                Op::SetTitle {
                    issue_id: id.clone(),
                    title: "from pre-bump".into(),
                },
            ),
        ]);
        let head_b = snap(vec![
            unstamped_create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000001Z"),
                0,
                Op::SetTitle {
                    issue_id: id.clone(),
                    title: "from post-bump".into(),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.title, "from post-bump");
    }

    // ---- LWW scalars: assignee -----------------------------------------

    #[test]
    fn assignee_lww_later_wins() {
        let id = iid("9999999");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::SetAssignee {
                    issue_id: id.clone(),
                    assignee: Some("alice".into()),
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:02Z",
                Some("2026-06-22T12:00:02.000000000Z"),
                0,
                Op::SetAssignee {
                    issue_id: id.clone(),
                    assignee: Some("bob".into()),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.assignee.as_deref(), Some("bob"));
    }

    #[test]
    fn assignee_tie_break_by_commit_hash() {
        let id = iid("aaaaaa0");
        let at = "2026-06-22T12:00:01.000000000Z";
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aaaa",
                "2026-06-22T12:00:01Z",
                Some(at),
                0,
                Op::SetAssignee {
                    issue_id: id.clone(),
                    assignee: Some("alice".into()),
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bbbb",
                "2026-06-22T12:00:01Z",
                Some(at),
                0,
                Op::SetAssignee {
                    issue_id: id.clone(),
                    assignee: Some("bob".into()),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.assignee.as_deref(), Some("bob"));
    }

    #[test]
    fn assignee_tie_break_by_trailer_index() {
        let id = iid("aaaaaa1");
        let at = "2026-06-22T12:00:01.000000000Z";
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "cc",
                "2026-06-22T12:00:01Z",
                Some(at),
                0,
                Op::SetAssignee {
                    issue_id: id.clone(),
                    assignee: Some("first".into()),
                },
            ),
            entry(
                "cc",
                "2026-06-22T12:00:01Z",
                Some(at),
                1,
                Op::SetAssignee {
                    issue_id: id.clone(),
                    assignee: Some("second".into()),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a]);
        assert_eq!(merged.record.assignee.as_deref(), Some("second"));
    }

    #[test]
    fn assignee_unstamped_loses_to_stamped_at_same_second() {
        // Pre-bump Create (no Jjf-At) so mixing unstamped + stamped
        // ops doesn't break the "first op must be Create" invariant.
        let id = iid("aaaaaa2");
        let head_a = snap(vec![
            unstamped_create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                None,
                0,
                Op::SetAssignee {
                    issue_id: id.clone(),
                    assignee: Some("pre".into()),
                },
            ),
        ]);
        let head_b = snap(vec![
            unstamped_create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000001Z"),
                0,
                Op::SetAssignee {
                    issue_id: id.clone(),
                    assignee: Some("post".into()),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.assignee.as_deref(), Some("post"));
    }

    #[test]
    fn assignee_unset_lww_wins() {
        // `SetAssignee { assignee: None }` is the unassign op — the
        // reducer must treat it as the winning value, not as
        // "no-op."
        let id = iid("aaaaaa3");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::SetAssignee {
                    issue_id: id.clone(),
                    assignee: Some("alice".into()),
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:02Z",
                Some("2026-06-22T12:00:02.000000000Z"),
                0,
                Op::SetAssignee {
                    issue_id: id.clone(),
                    assignee: None,
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.assignee, None);
    }

    // ---- LWW scalars: body ---------------------------------------------

    #[test]
    fn body_lww_winning_hash_resolved_to_bytes() {
        // Two heads each ran SetBody on disjoint body bytes. The
        // later head's hash is the winner; the reducer pulls the
        // bytes from whichever head's rendered record matches that
        // hash.
        let id = iid("aaaaaa4");
        let body_a = "body from A".to_owned();
        let body_b = "body from B".to_owned();
        let hash_a = crate::body_hash_hex(&body_a);
        let hash_b = crate::body_hash_hex(&body_b);
        let snap_a = HeadSnapshot {
            record: Some(IssueRecord {
                version: 2,
                id: id.clone(),
                title: "initial".into(),
                slug: None,
                body: body_a.clone(),
                status: Status::Open,
                block_reason: None,
                type_: IssueType::Unspecified,
                priority: None,
                labels: Vec::new(),
                metadata: BTreeMap::new(),
                dependencies: Vec::new(),
                assignee: None,
                created_at: "2026-06-22T12:00:00Z".into(),
                updated_at: "2026-06-22T12:00:01Z".into(),
            }),
            comments: Vec::new(),
            entries: vec![
                create_entry(&id, "2026-06-22T12:00:00Z"),
                entry(
                    "aa",
                    "2026-06-22T12:00:01Z",
                    Some("2026-06-22T12:00:01.000000000Z"),
                    0,
                    Op::SetBody {
                        issue_id: id.clone(),
                        body_hash: hash_a.clone(),
                    },
                ),
            ],
        };
        let snap_b = HeadSnapshot {
            record: Some(IssueRecord {
                version: 2,
                id: id.clone(),
                title: "initial".into(),
                slug: None,
                body: body_b.clone(),
                status: Status::Open,
                block_reason: None,
                type_: IssueType::Unspecified,
                priority: None,
                labels: Vec::new(),
                metadata: BTreeMap::new(),
                dependencies: Vec::new(),
                assignee: None,
                created_at: "2026-06-22T12:00:00Z".into(),
                updated_at: "2026-06-22T12:00:02Z".into(),
            }),
            comments: Vec::new(),
            entries: vec![
                create_entry(&id, "2026-06-22T12:00:00Z"),
                entry(
                    "bb",
                    "2026-06-22T12:00:02Z",
                    Some("2026-06-22T12:00:02.000000000Z"),
                    0,
                    Op::SetBody {
                        issue_id: id.clone(),
                        body_hash: hash_b.clone(),
                    },
                ),
            ],
        };
        let merged = reduce(&id, &[snap_a, snap_b]);
        assert_eq!(merged.record.body, body_b);
    }

    #[test]
    fn body_tie_break_by_trailer_index() {
        // Same commit, two SetBody stanzas; higher trailer_index wins.
        let id = iid("aaaaaa5");
        let body_low = "first stanza body".to_owned();
        let body_high = "second stanza body".to_owned();
        let hash_low = crate::body_hash_hex(&body_low);
        let hash_high = crate::body_hash_hex(&body_high);
        // The single rendered record carries `body_high` — the
        // winning hash; the bytes for `body_low` aren't on disk
        // (they got overwritten when the commit landed). The reducer
        // only needs the winning bytes.
        let head_a = HeadSnapshot {
            record: Some(IssueRecord {
                version: 2,
                id: id.clone(),
                title: "initial".into(),
                slug: None,
                body: body_high.clone(),
                status: Status::Open,
                block_reason: None,
                type_: IssueType::Unspecified,
                priority: None,
                labels: Vec::new(),
                metadata: BTreeMap::new(),
                dependencies: Vec::new(),
                assignee: None,
                created_at: "2026-06-22T12:00:00Z".into(),
                updated_at: "2026-06-22T12:00:01Z".into(),
            }),
            comments: Vec::new(),
            entries: vec![
                create_entry(&id, "2026-06-22T12:00:00Z"),
                entry(
                    "cc",
                    "2026-06-22T12:00:01Z",
                    Some("2026-06-22T12:00:01.000000000Z"),
                    0,
                    Op::SetBody {
                        issue_id: id.clone(),
                        body_hash: hash_low,
                    },
                ),
                entry(
                    "cc",
                    "2026-06-22T12:00:01Z",
                    Some("2026-06-22T12:00:01.000000000Z"),
                    1,
                    Op::SetBody {
                        issue_id: id.clone(),
                        body_hash: hash_high.clone(),
                    },
                ),
            ],
        };
        let merged = reduce(&id, &[head_a]);
        assert_eq!(merged.record.body, body_high);
    }

    #[test]
    fn body_winning_hash_missing_from_heads_errors() {
        // Defensive path: the winning SetBody hash isn't present in
        // any head's rendered record. Should surface a typed
        // Error::Invalid, not panic.
        let id = iid("aaaaaa6");
        let bogus_hash = "deadbeef".repeat(8);
        let head_a = HeadSnapshot {
            record: Some(IssueRecord {
                version: 2,
                id: id.clone(),
                title: "initial".into(),
                slug: None,
                body: "actual bytes".into(),
                status: Status::Open,
                block_reason: None,
                type_: IssueType::Unspecified,
                priority: None,
                labels: Vec::new(),
                metadata: BTreeMap::new(),
                dependencies: Vec::new(),
                assignee: None,
                created_at: "2026-06-22T12:00:00Z".into(),
                updated_at: "2026-06-22T12:00:01Z".into(),
            }),
            comments: Vec::new(),
            entries: vec![
                create_entry(&id, "2026-06-22T12:00:00Z"),
                entry(
                    "aa",
                    "2026-06-22T12:00:01Z",
                    Some("2026-06-22T12:00:01.000000000Z"),
                    0,
                    Op::SetBody {
                        issue_id: id.clone(),
                        body_hash: bogus_hash,
                    },
                ),
            ],
        };
        let err = reduce_to_merged(&id, &[head_a]).unwrap_err();
        assert!(
            matches!(err, Error::Invalid(ref m) if m.contains("winning body hash")),
            "expected Invalid(winning body hash...), got {:?}",
            err
        );
    }

    #[test]
    fn body_no_set_body_op_pulls_create_time_body_from_record() {
        // Chain has no SetBody op (create's body was empty so the
        // multi-op writer skipped it). The reducer should still
        // pull bytes from whichever head has a rendered record.
        let id = iid("aaaaaa7");
        let body = "create-time body".to_owned();
        let head = HeadSnapshot {
            record: Some(IssueRecord {
                version: 2,
                id: id.clone(),
                title: "initial".into(),
                slug: None,
                body: body.clone(),
                status: Status::Open,
                block_reason: None,
                type_: IssueType::Unspecified,
                priority: None,
                labels: Vec::new(),
                metadata: BTreeMap::new(),
                dependencies: Vec::new(),
                assignee: None,
                created_at: "2026-06-22T12:00:00Z".into(),
                updated_at: "2026-06-22T12:00:00Z".into(),
            }),
            comments: Vec::new(),
            entries: vec![create_entry(&id, "2026-06-22T12:00:00Z")],
        };
        let merged = reduce(&id, &[head]);
        assert_eq!(merged.record.body, body);
    }

    // ---- Sets: labels --------------------------------------------------

    #[test]
    fn labels_add_on_one_side_present() {
        // Head A adds label "x"; head B leaves it alone. The merged
        // record carries {"x"}.
        let id = iid("bbbbbb0");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::LabelAdd {
                    issue_id: id.clone(),
                    label: "x".into(),
                },
            ),
        ]);
        let head_b = snap(vec![create_entry(&id, "2026-06-22T12:00:00Z")]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.labels, vec!["x".to_string()]);
    }

    #[test]
    fn labels_add_then_remove_remove_wins_when_later() {
        // Head A adds "x" at t1; head B removes "x" at t2. Removal
        // is later, so "x" is absent.
        let id = iid("bbbbbb1");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::LabelAdd {
                    issue_id: id.clone(),
                    label: "x".into(),
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:02Z",
                Some("2026-06-22T12:00:02.000000000Z"),
                0,
                Op::LabelRm {
                    issue_id: id.clone(),
                    label: "x".into(),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert!(merged.record.labels.is_empty(), "expected no labels, got {:?}", merged.record.labels);
    }

    #[test]
    fn labels_remove_then_add_add_wins_when_later() {
        // Mirror of the above: remove first, then add — add wins.
        let id = iid("bbbbbb2");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::LabelRm {
                    issue_id: id.clone(),
                    label: "x".into(),
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:02Z",
                Some("2026-06-22T12:00:02.000000000Z"),
                0,
                Op::LabelAdd {
                    issue_id: id.clone(),
                    label: "x".into(),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.labels, vec!["x".to_string()]);
    }

    #[test]
    fn labels_both_sides_add_same_label_idempotent() {
        // Both heads independently add "x" — present, single copy.
        let id = iid("bbbbbb3");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::LabelAdd {
                    issue_id: id.clone(),
                    label: "x".into(),
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:02Z",
                Some("2026-06-22T12:00:02.000000000Z"),
                0,
                Op::LabelAdd {
                    issue_id: id.clone(),
                    label: "x".into(),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.labels, vec!["x".to_string()]);
    }

    #[test]
    fn labels_one_side_add_then_remove_other_adds_y_final_is_y_only() {
        // Head A: add x at t1, remove x at t3. Head B: add y at t2.
        // Final: {y}.
        let id = iid("bbbbbb4");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "a1",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::LabelAdd {
                    issue_id: id.clone(),
                    label: "x".into(),
                },
            ),
            entry(
                "a2",
                "2026-06-22T12:00:03Z",
                Some("2026-06-22T12:00:03.000000000Z"),
                0,
                Op::LabelRm {
                    issue_id: id.clone(),
                    label: "x".into(),
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:02Z",
                Some("2026-06-22T12:00:02.000000000Z"),
                0,
                Op::LabelAdd {
                    issue_id: id.clone(),
                    label: "y".into(),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.labels, vec!["y".to_string()]);
    }

    // ---- Sets: dependencies --------------------------------------------

    #[test]
    fn deps_add_on_one_side_present() {
        let id = iid("ccccc00");
        let dep = iid("dad0001");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::DepAdd {
                    issue_id: id.clone(),
                    dep: dep.clone(),
                    kind: DepKind::Blocks,
                },
            ),
        ]);
        let head_b = snap(vec![create_entry(&id, "2026-06-22T12:00:00Z")]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.dependencies, vec![DepEdge::blocks(dep)]);
    }

    #[test]
    fn deps_add_then_remove_remove_wins_when_later() {
        let id = iid("ccccc01");
        let dep = iid("dad0001");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::DepAdd {
                    issue_id: id.clone(),
                    dep: dep.clone(),
                    kind: DepKind::Blocks,
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:02Z",
                Some("2026-06-22T12:00:02.000000000Z"),
                0,
                Op::DepRm {
                    issue_id: id.clone(),
                    dep: dep.clone(),
                    kind: DepKind::Blocks,
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert!(merged.record.dependencies.is_empty());
    }

    #[test]
    fn deps_both_sides_add_same_dep_idempotent() {
        let id = iid("ccccc02");
        let dep = iid("dad0001");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::DepAdd {
                    issue_id: id.clone(),
                    dep: dep.clone(),
                    kind: DepKind::Blocks,
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:02Z",
                Some("2026-06-22T12:00:02.000000000Z"),
                0,
                Op::DepAdd {
                    issue_id: id.clone(),
                    dep: dep.clone(),
                    kind: DepKind::Blocks,
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.dependencies, vec![DepEdge::blocks(dep)]);
    }

    #[test]
    fn deps_one_side_add_remove_other_adds_different_final_only_one() {
        // Symmetric to the labels test: A adds dep1 then removes;
        // B adds dep2. Final: {dep2}.
        let id = iid("ccccc03");
        let dep1 = iid("dad0001");
        let dep2 = iid("dad0002");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "a1",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::DepAdd {
                    issue_id: id.clone(),
                    dep: dep1.clone(),
                    kind: DepKind::Blocks,
                },
            ),
            entry(
                "a2",
                "2026-06-22T12:00:03Z",
                Some("2026-06-22T12:00:03.000000000Z"),
                0,
                Op::DepRm {
                    issue_id: id.clone(),
                    dep: dep1.clone(),
                    kind: DepKind::Blocks,
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:02Z",
                Some("2026-06-22T12:00:02.000000000Z"),
                0,
                Op::DepAdd {
                    issue_id: id.clone(),
                    dep: dep2.clone(),
                    kind: DepKind::Blocks,
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.dependencies, vec![DepEdge::blocks(dep2)]);
    }

    // ---- Comments ------------------------------------------------------

    #[test]
    fn comments_both_sides_distinct_union_by_created_at() {
        // Head A appends comment c1; head B appends c2. Merged: both,
        // ordered by created_at ascending.
        let id = iid("ddddd00");
        let c1 = iid("c000001");
        let c2 = iid("c000002");
        let comment1 = Comment {
            id: c1.clone(),
            author: "alice".into(),
            created_at: "2026-06-22T12:00:01Z".into(),
            body: "from A".into(),
        };
        let comment2 = Comment {
            id: c2.clone(),
            author: "bob".into(),
            created_at: "2026-06-22T12:00:02Z".into(),
            body: "from B".into(),
        };
        let head_a = HeadSnapshot {
            record: None,
            comments: vec![comment1.clone()],
            entries: vec![
                create_entry(&id, "2026-06-22T12:00:00Z"),
                entry(
                    "aa",
                    "2026-06-22T12:00:01Z",
                    Some("2026-06-22T12:00:01.000000000Z"),
                    0,
                    Op::CommentAdd {
                        issue_id: id.clone(),
                        comment_id: c1.clone(),
                    },
                ),
            ],
        };
        let head_b = HeadSnapshot {
            record: None,
            comments: vec![comment2.clone()],
            entries: vec![
                create_entry(&id, "2026-06-22T12:00:00Z"),
                entry(
                    "bb",
                    "2026-06-22T12:00:02Z",
                    Some("2026-06-22T12:00:02.000000000Z"),
                    0,
                    Op::CommentAdd {
                        issue_id: id.clone(),
                        comment_id: c2.clone(),
                    },
                ),
            ],
        };
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.comments.len(), 2);
        assert_eq!(merged.comments[0].id, c1);
        assert_eq!(merged.comments[1].id, c2);
    }

    #[test]
    fn comments_duplicate_id_across_heads_kept_once() {
        // Both heads add a CommentAdd op with the same comment_id
        // (spec says "shouldn't happen" — different writers must
        // mint different ids — but the reducer should be robust).
        let id = iid("ddddd01");
        let cid = iid("c000001");
        let comment = Comment {
            id: cid.clone(),
            author: "alice".into(),
            created_at: "2026-06-22T12:00:01Z".into(),
            body: "shared bytes".into(),
        };
        let head_a = HeadSnapshot {
            record: None,
            comments: vec![comment.clone()],
            entries: vec![
                create_entry(&id, "2026-06-22T12:00:00Z"),
                entry(
                    "aa",
                    "2026-06-22T12:00:01Z",
                    Some("2026-06-22T12:00:01.000000000Z"),
                    0,
                    Op::CommentAdd {
                        issue_id: id.clone(),
                        comment_id: cid.clone(),
                    },
                ),
            ],
        };
        let head_b = HeadSnapshot {
            record: None,
            comments: vec![comment.clone()],
            entries: vec![
                create_entry(&id, "2026-06-22T12:00:00Z"),
                entry(
                    "bb",
                    "2026-06-22T12:00:02Z",
                    Some("2026-06-22T12:00:02.000000000Z"),
                    0,
                    Op::CommentAdd {
                        issue_id: id.clone(),
                        comment_id: cid.clone(),
                    },
                ),
            ],
        };
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.comments.len(), 1);
        assert_eq!(merged.comments[0].id, cid);
    }

    // ---- Cross-version (v1 + v2 trailers in the same chain) ------------

    #[test]
    fn cross_version_stamped_beats_unstamped_at_same_second() {
        // Head A's op is a v1-style stanza (no Jjf-At). Head B's op
        // is v2 with a Jjf-At at the same author-second. Spec §6 +
        // the orchestrator's call: stamped beats unstamped at the
        // same second. The Create is also v1-style — see
        // `unstamped_create_entry`'s docstring for why.
        let id = iid("eeeee00");
        let head_a = snap(vec![
            unstamped_create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                None,
                0,
                Op::SetTitle {
                    issue_id: id.clone(),
                    title: "v1 trailer".into(),
                },
            ),
        ]);
        let head_b = snap(vec![
            unstamped_create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000001Z"),
                0,
                Op::SetTitle {
                    issue_id: id.clone(),
                    title: "v2 trailer".into(),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.title, "v2 trailer");
    }

    #[test]
    fn cross_version_any_stamped_beats_any_unstamped_regardless_of_clock() {
        // Surprise: the spec-comment claim "unstamped sorts before
        // stamped *at the same second*" is wider in implementation:
        // `PrimaryKey::Unstamped` is declared before
        // `PrimaryKey::Stamped`, so derived `Ord` makes EVERY
        // unstamped op sort before EVERY stamped op, regardless of
        // clock time. This pins the actual behavior: a stamped op at
        // t1 still beats an unstamped op at t2.
        //
        // In practice this is the right semantics — every unstamped
        // op is a pre-spec-bump artifact and so predates every
        // stamped op by construction — but the spec-comment is
        // narrower than the code. Worth flagging if the spec ever
        // tightens to require strict clock comparison.
        let id = iid("eeeee01");
        let head_a = snap(vec![
            unstamped_create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::SetTitle {
                    issue_id: id.clone(),
                    title: "v2 stamped at t1".into(),
                },
            ),
        ]);
        let head_b = snap(vec![
            unstamped_create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:02Z",
                None,
                0,
                Op::SetTitle {
                    issue_id: id.clone(),
                    title: "v1 unstamped at t2".into(),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        // The stamped op wins despite landing earlier on the clock.
        assert_eq!(merged.record.title, "v2 stamped at t1");
    }

    // ---- De-dup of shared ancestry -------------------------------------

    #[test]
    fn shared_pre_fork_ops_deduplicated() {
        // Both heads' chains include the pre-fork SetTitle at commit
        // "shared". The reducer dedups by (commit, trailer_index) so
        // the shared op only counts once. The later per-head op
        // determines the winning title.
        let id = iid("fffff00");
        let shared_set = entry(
            "shared",
            "2026-06-22T12:00:01Z",
            Some("2026-06-22T12:00:01.000000000Z"),
            0,
            Op::SetTitle {
                issue_id: id.clone(),
                title: "pre-fork title".into(),
            },
        );
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            shared_set.clone(),
            entry(
                "aa",
                "2026-06-22T12:00:02Z",
                Some("2026-06-22T12:00:02.000000000Z"),
                0,
                Op::SetTitle {
                    issue_id: id.clone(),
                    title: "head A wins".into(),
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            shared_set,
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.title, "head A wins");
    }

    // ---- Error path: chain not starting with `Create` ------------------

    #[test]
    fn chain_without_create_errors() {
        // The reducer requires the first sorted op to be a Create;
        // a chain starting with anything else surfaces a typed
        // Error::Invalid.
        let id = iid("fffff01");
        let head = snap(vec![entry(
            "aa",
            "2026-06-22T12:00:01Z",
            Some("2026-06-22T12:00:01.000000000Z"),
            0,
            Op::SetTitle {
                issue_id: id.clone(),
                title: "no create".into(),
            },
        )]);
        let err = reduce_to_merged(&id, &[head]).unwrap_err();
        assert!(
            matches!(err, Error::Invalid(ref m) if m.contains("does not start with `create`")),
            "expected Invalid(does not start with `create`), got {:?}",
            err
        );
    }

    // ---- LWW scalars: type and slug (v2.1 additions) ------------------

    #[test]
    fn type_lww_later_wins() {
        let id = iid("fffff02");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::SetType {
                    issue_id: id.clone(),
                    kind: IssueType::Bug,
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:02Z",
                Some("2026-06-22T12:00:02.000000000Z"),
                0,
                Op::SetType {
                    issue_id: id.clone(),
                    kind: IssueType::Feature,
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.type_, IssueType::Feature);
    }

    #[test]
    fn slug_lww_later_wins() {
        let id = iid("fffff03");
        let head_a = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "aa",
                "2026-06-22T12:00:01Z",
                Some("2026-06-22T12:00:01.000000000Z"),
                0,
                Op::SetSlug {
                    issue_id: id.clone(),
                    slug: Some("from-a".into()),
                },
            ),
        ]);
        let head_b = snap(vec![
            create_entry(&id, "2026-06-22T12:00:00Z"),
            entry(
                "bb",
                "2026-06-22T12:00:02Z",
                Some("2026-06-22T12:00:02.000000000Z"),
                0,
                Op::SetSlug {
                    issue_id: id.clone(),
                    slug: Some("from-b".into()),
                },
            ),
        ]);
        let merged = reduce(&id, &[head_a, head_b]);
        assert_eq!(merged.record.slug.as_deref(), Some("from-b"));
    }

    /// Regression for the divergence surfaced when the read-path
    /// cross-check guard was removed. Before the fold-by-LWW fix,
    /// `replay_ops` in `read.rs` walked the jj-log ordering (newest →
    /// oldest reversed), which does NOT match the resolver's spec §6
    /// total order on divergent heads. Two `SetTitle` ops at the same
    /// commit-time but with `jjf_at` stamps from different writers
    /// would fold in jj-log order on read while the resolver had
    /// already written the file via LWW order — a guaranteed mismatch.
    ///
    /// This test pins the LWW order across two stamped entries at the
    /// same commit-time second: the later `jjf_at` wins regardless of
    /// the input slice ordering, which is what makes the cross-check
    /// trivially agree with the resolver's on-disk projection.
    #[test]
    fn sort_entries_lww_orders_stamped_by_jjf_at() {
        let id = crate::id::IssueId::parse("aa6600b").unwrap();
        let earlier = HistoryEntry {
            commit: "bbb".into(),
            author: "alice".into(),
            timestamp: "2026-06-22T12:00:00Z".into(),
            jjf_at: Some("2026-06-22T12:00:00.111111111Z".into()),
            trailer_index: 0,
            op: Op::SetTitle {
                issue_id: id.clone(),
                title: "alice title".into(),
            },
        };
        let later = HistoryEntry {
            commit: "aaa".into(),
            author: "bob".into(),
            timestamp: "2026-06-22T12:00:00Z".into(),
            jjf_at: Some("2026-06-22T12:00:00.222222222Z".into()),
            trailer_index: 0,
            op: Op::SetTitle {
                issue_id: id.clone(),
                title: "bob title".into(),
            },
        };

        // Feed `later` first to force the sort to do work — the LWW
        // sort must pull `earlier` to position 0 regardless of input.
        let mut entries = vec![later.clone(), earlier.clone()];
        sort_entries_lww(&mut entries);
        assert_eq!(entries[0].commit, earlier.commit);
        assert_eq!(entries[1].commit, later.commit);
    }
}
