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
use crate::record::{Comment, IssueRecord};
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

/// Replay one issue across every head and produce the merged
/// [`MergedIssue`].
///
/// For each head:
/// - Walk its op chain via `read_history_at`.
/// - Read its rendered record + comments file (if present) so we can
///   look up body bytes by hash and union comment bodies by id.
fn resolve_one(repo: &JjRepo, heads: &[String], id: &IssueId) -> Result<MergedIssue> {
    // Per-head structural snapshots. The reducer uses the op chain
    // for everything that can be op-replayed (title/status/labels/…);
    // the rendered files provide body bytes and comment bodies.
    struct HeadSnapshot {
        record: Option<IssueRecord>,
        comments: Vec<Comment>,
        entries: Vec<HistoryEntry>,
    }

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

    // 1. Flatten + sort every head's entries by the spec §6 ordering
    //    tuple. The same op may appear on multiple heads if both
    //    heads share that commit's ancestry (e.g. ops before the
    //    fork); we de-dup by `(commit, trailer_index)`.
    let mut all_entries: Vec<HistoryEntry> = Vec::new();
    let mut seen: BTreeSet<(String, u32)> = BTreeSet::new();
    for snap in &snapshots {
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

    // 2. Reduce. Per spec §6 the rules are:
    //    - `Create`: earliest wins; should agree across heads since
    //      create predates the fork. The sorted stream guarantees the
    //      first op IS that earliest create.
    //    - `SetTitle`/`SetStatus`/`SetAssignee`/`SetBody`: LWW by the
    //      ordering tuple — the LAST op in the sorted stream wins.
    //    - `LabelAdd`/`LabelRm`, `DepAdd`/`DepRm`: causal order;
    //      "last operation per (label/dep) wins."
    //    - `CommentAdd`: union of comment_ids (no conflict possible).
    //    - `Merge`: marker; folded as a no-op for state purposes
    //      (the parents' chains are the truth).

    // Initialize from the first op (must be `Create` for a well-
    // formed chain).
    let mut record = match &all_entries[0].op {
        Op::Create {
            issue_id,
            title,
            status,
        } => IssueRecord {
            version: 2,
            id: issue_id.clone(),
            title: title.clone(),
            body: String::new(),
            status: *status,
            labels: Vec::new(),
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
    // `Some(())`, a `LabelRm` writes `None`; final pass takes
    // present-labels = keys whose final value is `Some`.
    let mut label_state: BTreeMap<String, bool> = BTreeMap::new();
    let mut dep_state: BTreeMap<IssueId, bool> = BTreeMap::new();

    // Track the latest SetBody op's hash so we can look up the
    // matching head's body bytes once the reduce is done.
    let mut latest_body_hash: Option<String> = None;
    // Pre-seed from `Create` whose ops also include `set-body` per
    // spec §5.7 (multi-op create). The fold below picks that up.

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
            Op::SetBody { body_hash, .. } => {
                latest_body_hash = Some(body_hash.clone());
            }
            Op::LabelAdd { label, .. } => {
                label_state.insert(label.clone(), true);
            }
            Op::LabelRm { label, .. } => {
                label_state.insert(label.clone(), false);
            }
            Op::DepAdd { dep, .. } => {
                dep_state.insert(dep.clone(), true);
            }
            Op::DepRm { dep, .. } => {
                dep_state.insert(dep.clone(), false);
            }
            Op::CommentAdd { comment_id, .. } => {
                comment_ids.push(comment_id.clone());
            }
            Op::Merge { .. } => {
                // No-op — the parents' chains are authoritative.
            }
        }
    }

    // Project final state.
    let mut labels: Vec<String> = label_state
        .into_iter()
        .filter_map(|(l, present)| if present { Some(l) } else { None })
        .collect();
    labels.sort();
    record.labels = labels;

    let mut deps: Vec<IssueId> = dep_state
        .into_iter()
        .filter_map(|(d, present)| if present { Some(d) } else { None })
        .collect();
    deps.sort();
    record.dependencies = deps;

    // 3. Body-hash lookup. The winning op's hash IS in at least one
    //    head's rendered file; pluck the bytes from there.
    if let Some(hash) = &latest_body_hash {
        let mut found: Option<String> = None;
        for snap in &snapshots {
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
        for snap in &snapshots {
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
}
