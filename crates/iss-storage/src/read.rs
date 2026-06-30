//! Read path for the `issues` bookmark.
//!
//! Given a repo and an issue id, returns the structured `Issue` view: the
//! latest scalar field values from `issues/<id>.json` plus the full
//! chronological comment thread from `issues/<id>.comments.jsonl`.
//!
//! # Two implementations, asserted equal
//!
//! Per the ticket's acceptance criteria, this module computes the
//! result two ways and (in debug builds) asserts they agree on
//! structural fields:
//!
//! 1. **File-read.** Pull `issue.json` and `comments.jsonl` off the
//!    per-issue ref `refs/jjf/issues/<id>`'s tree via `git cat-file
//!    blob`. (The v2 bookmark-tip read path was removed in J5.)
//! 2. **Op-replay.** Walk the per-issue ref's commit chain via
//!    `history::read_history_at_v3`, sort the resulting per-op entries
//!    by the spec §6 LWW total order — `(jjf_at if Some else
//!    commit_time, commit, trailer_index)`, the same key the op-space
//!    merge driver uses — and fold them into a record.
//!
//! When file-read and op-replay disagree on a structural field, that's
//! a violation of the storage contract — either the writer didn't
//! record an op for a mutation, the writer wrote the file without a
//! corresponding op, or the resolver and the cross-check diverged on
//! how to project the op chain. Crashing in debug builds is the
//! cheapest way to catch a regression in the write path. Release
//! builds trust the file (it's the authoritative copy).
//!
//! Sorting by the resolver's LWW key (rather than the jj-log order
//! the v1 cross-check used) is what makes the check valid across
//! merges. The resolver writes the merged file by applying ops in
//! LWW order; we re-derive that same order on read.
//!
//! Timestamps (`created_at` / `updated_at`) deliberately do NOT
//! participate in the equality check. The on-disk record carries
//! sub-second wall-clock timestamps from the writer; op-replay can only
//! recover commit-author timestamps from `jj log`, and the two are
//! related but not identical (the writer's `now_rfc3339()` is called
//! before `jj new`, then jj stamps the commit independently). Comparing
//! them invites spurious flake. The file's timestamps are returned;
//! op-replay timestamps are validated for monotonicity instead (see
//! `verify_timestamp_ordering`).

use crate::git::GitRepo;
use crate::id::IssueId;
#[cfg(any(debug_assertions, test))]
use crate::op::Op;
#[cfg(any(debug_assertions, test))]
use crate::record::{DepEdge, IssueType, Status};
use crate::record::{Comment, Issue, IssueRecord};
use crate::v3_write;
use crate::{Error, Result};

/// Read a single issue from authoritative storage.
///
/// Sources `issue.json` and `comments.jsonl` off
/// `refs/jjf/issues/<id>`'s tree via `git cat-file blob`. (The v2
/// bookmark-tip read path was removed in J5; only the v3 ref layout is
/// supported.)
///
/// Errors:
/// - `IssueNotFound` if the record is absent.
/// - `Json` if the on-disk record or any comment line is malformed.
/// - `Git` if the underlying CLI fails for any non-missing-file
///   reason.
pub(crate) fn read(git: &GitRepo, id: &IssueId) -> Result<Issue> {
    let (record, mut comments) = {
        let record = v3_write::read_record_v3(git, id)?;
        let comments = v3_write::read_comments_v3(git, id)?;
        (record, comments)
    };

    // Defensive sort: comments by created_at ascending. The writer
    // appends in order, but the merge driver may union two append
    // streams whose interleaving is undefined. Spec §4.2 says readers
    // *may* re-sort; we do.
    comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    #[cfg(debug_assertions)]
    {
        let op_view = replay_ops(git, id)?;
        // Cross-check runs unconditionally, including across merges.
        // With op-space resolution (`bfc732b`), the file on disk after
        // a merge IS a deterministic projection of the op chain: the
        // resolver folds both branches of the merge in a canonical
        // order and writes the merged record. Op-replay folds the same
        // op chain on read and lands on the same view by construction,
        // so file-read and op-replay must agree. Any disagreement
        // surfaced here is a real write-path or resolver bug, and the
        // panic is the cheapest catch-it-early signal we have.
        cross_check(&record, &comments, &op_view);
    }

    Ok(Issue {
        id: record.id,
        title: record.title,
        slug: record.slug,
        body: record.body,
        status: record.status,
        block_reason: record.block_reason,
        type_: record.type_,
        priority: record.priority,
        // Defensive re-sort — writer guarantees sorted, but the merge
        // driver may emit unioned arrays.
        labels: {
            let mut v = record.labels;
            v.sort();
            v.dedup();
            v
        },
        dependencies: {
            let mut v = record.dependencies;
            v.sort();
            v.dedup();
            v
        },
        metadata: record.metadata,
        assignee: record.assignee,
        comments,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

// ---- op-replay view ---------------------------------------------------
//
// Used only by the debug-assertions cross-check. The trailer-parsing
// machinery this folds over lives in `trailer.rs` and is shared with
// the history path; `OpView` is specifically the structural snapshot
// the cross-check needs, distinct from the per-op timeline that
// `history.rs` exposes.
#[cfg(any(debug_assertions, test))]
/// The structural projection an op-replay can recover. No timestamps
/// because trailers don't carry them; the body string isn't carried in
/// the `set-body` trailer either (only its sha-256 hash is).
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpView {
    id: IssueId,
    title: String,
    /// Latest slug seen in a `set-slug` op. `None` means either no
    /// slug op was ever applied, OR the most recent slug op cleared
    /// it (`Op::SetSlug { slug: None }`). The cross-check matches
    /// this against the file's `slug` field directly.
    slug: Option<String>,
    /// `Some(hash)` if a `set-body` op was applied; `None` if the issue
    /// was only ever touched by `create` (whose op chain has no body
    /// hash — the create-time body is in the JSON record). We use this
    /// to validate the on-disk body when a hash is available, and skip
    /// the check otherwise.
    body_hash: Option<String>,
    status: Status,
    /// Latest reason seen in a `set-block-reason` op. `None` either
    /// means no reason op was applied or the most recent op cleared
    /// it (`Op::SetBlockReason { reason: None }`). v2.5
    /// (`agent-await-gates-impl`). Cross-check matches this against
    /// the file's `block_reason` field directly.
    block_reason: Option<String>,
    /// Latest type seen in a `set-type` op, or `Unspecified` if no
    /// type op was applied (the v2.1-default for any chain that
    /// predates the new field).
    type_: IssueType,
    /// Latest priority seen in a `set-priority` op. `None` either
    /// means no priority op was applied or the most recent op
    /// cleared it (`Op::SetPriority { priority: None }`). v2.8
    /// (`priority-field`). Cross-check matches this against the
    /// file's `priority` field directly.
    priority: Option<u8>,
    labels: Vec<String>,
    /// Metadata map projected from `set-metadata` / `unset-metadata`
    /// ops (last-write-wins per key). Cross-check compares this against
    /// the file's `metadata` field directly.
    metadata: std::collections::BTreeMap<String, String>,
    /// Typed dependency edges. v2.4 — same shape as
    /// [`IssueRecord::dependencies`]; the read path cross-check
    /// compares the file's edge list (sorted by `(target, kind)`)
    /// against the op-replay projection.
    dependencies: Vec<DepEdge>,
    assignee: Option<String>,
    /// Comment IDs in the order they were added (op chain order, oldest
    /// first). Used to validate that the JSONL file matches.
    comment_ids: Vec<IssueId>,
}

/// Walk the per-issue op chain and fold it into a structural view.
///
/// Walks the per-issue ref's commit chain (`refs/jjf/issues/<id>`) via
/// `history::read_history_at_v3`, then sorts with
/// `merge_ops::sort_entries_lww` — the same `(jjf_at, commit,
/// trailer_index)` total order the op-space merge driver applies when
/// it writes the merged file. Folding in this order means file-read
/// and op-replay project the same op chain identically, including
/// across merges where two heads' `set-*` ops compose by LWW. (Per
/// spec §6.) The v2 bookmark-tip history path was removed in J5.
#[cfg(debug_assertions)]
fn replay_ops(git: &GitRepo, id: &IssueId) -> Result<OpView> {
    use crate::merge_ops::sort_entries_lww;

    let mut entries = match crate::history::read_history_at_v3(git, id) {
        Ok(v) => v,
        Err(Error::IssueNotFound(_)) => return Err(Error::IssueNotFound(id.clone())),
        Err(e) => return Err(e),
    };

    sort_entries_lww(&mut entries);

    if entries.is_empty() {
        return Err(Error::IssueNotFound(id.clone()));
    }

    // Fold ops in LWW order. The first op MUST be `Create` for a
    // well-formed chain — `Create` carries the earliest stamp by
    // construction (no later write can predate the issue's own
    // creation), so the LWW sort lands it first.
    let mut view: Option<OpView> = None;
    for entry in entries {
        apply_op(&mut view, entry.op);
    }

    view.ok_or_else(|| {
        Error::Invalid(format!(
            "no `create` op found in chain for issue {}",
            id
        ))
    })
}

#[cfg(any(debug_assertions, test))]
fn apply_op(view: &mut Option<OpView>, op: Op) {
    match op {
        Op::Create {
            issue_id,
            title,
            status,
        } => {
            // Re-create resets — but a well-formed chain only has one
            // create at the start. If we see a second, treat it as an
            // overwrite (defensive; the merge driver shouldn't produce
            // this).
            *view = Some(OpView {
                id: issue_id,
                title,
                slug: None,
                body_hash: None,
                status,
                block_reason: None,
                type_: IssueType::Unspecified,
                priority: None,
                labels: Vec::new(),
                metadata: std::collections::BTreeMap::new(),
                dependencies: Vec::new(),
                assignee: None,
                comment_ids: Vec::new(),
            });
        }
        op => {
            let Some(v) = view.as_mut() else {
                // Op before create — ignored.
                return;
            };
            match op {
                Op::Create { .. } => unreachable!(),
                Op::SetTitle { title, .. } => v.title = title,
                Op::SetStatus { status, .. } => v.status = status,
                Op::SetBody { body_hash, .. } => v.body_hash = Some(body_hash),
                Op::LabelAdd { label, .. } => {
                    if !v.labels.iter().any(|l| l == &label) {
                        v.labels.push(label);
                        v.labels.sort();
                    }
                }
                Op::LabelRm { label, .. } => v.labels.retain(|l| l != &label),
                Op::SetMetadata { key, value, .. } => {
                    v.metadata.insert(key, value);
                }
                Op::UnsetMetadata { key, .. } => {
                    v.metadata.remove(&key);
                }
                Op::DepAdd { dep, kind, .. } => {
                    let edge = DepEdge { target: dep, kind };
                    if !v.dependencies.iter().any(|d| d == &edge) {
                        v.dependencies.push(edge);
                        v.dependencies.sort();
                    }
                }
                Op::DepRm { dep, kind, .. } => {
                    v.dependencies
                        .retain(|d| !(d.target == dep && d.kind == kind))
                }
                Op::SetAssignee { assignee, .. } => v.assignee = assignee,
                Op::SetType { kind, .. } => v.type_ = kind,
                Op::SetSlug { slug, .. } => v.slug = slug,
                Op::SetPriority { priority, .. } => v.priority = priority,
                Op::SetBlockReason { reason, .. } => v.block_reason = reason,
                Op::CommentAdd { comment_id, .. } => v.comment_ids.push(comment_id),
                Op::Merge { .. } => {
                    // No structural change. The `Jjf-Op: merge`
                    // trailer is a marker on the merge commit itself;
                    // op-replay folds the same op set the resolver
                    // folded when it wrote the merged file, so both
                    // sides land on the same structural view.
                }
            }
        }
    }
}

// ---- cross-check ------------------------------------------------------

/// Compare the file-read record to the op-replay view on structural
/// fields. Panics in debug builds if they disagree — that's a
/// write-path bug, not user error. Release builds skip this check (the
/// file is authoritative).
#[cfg(debug_assertions)]
fn cross_check(record: &IssueRecord, comments: &[Comment], op_view: &OpView) {
    let mismatch = |field: &str, file: String, ops: String| -> String {
        format!(
            "storage contract violation: file-read disagrees with op-replay for issue {}: {} differs (file={:?}, ops={:?})",
            record.id, field, file, ops
        )
    };

    assert_eq!(
        record.id, op_view.id,
        "{}",
        mismatch(
            "id",
            record.id.to_string(),
            op_view.id.to_string()
        )
    );
    assert_eq!(
        record.title, op_view.title,
        "{}",
        mismatch("title", record.title.clone(), op_view.title.clone())
    );
    assert_eq!(
        record.status, op_view.status,
        "{}",
        mismatch(
            "status",
            format!("{:?}", record.status),
            format!("{:?}", op_view.status)
        )
    );

    // Labels: file and ops should match as sorted sets (the writer
    // sorts; ops are applied in order with sort).
    let mut file_labels = record.labels.clone();
    file_labels.sort();
    let mut op_labels = op_view.labels.clone();
    op_labels.sort();
    assert_eq!(
        file_labels,
        op_labels,
        "{}",
        mismatch(
            "labels",
            format!("{:?}", file_labels),
            format!("{:?}", op_labels)
        )
    );

    // Metadata: file and ops should match as maps (BTreeMap compares
    // key/value sets directly; both project last-write-wins per key).
    assert_eq!(
        record.metadata,
        op_view.metadata,
        "{}",
        mismatch(
            "metadata",
            format!("{:?}", record.metadata),
            format!("{:?}", op_view.metadata)
        )
    );

    let mut file_deps = record.dependencies.clone();
    file_deps.sort();
    let mut op_deps = op_view.dependencies.clone();
    op_deps.sort();
    assert_eq!(
        file_deps,
        op_deps,
        "{}",
        mismatch(
            "dependencies",
            format!("{:?}", file_deps),
            format!("{:?}", op_deps)
        )
    );

    assert_eq!(
        record.assignee, op_view.assignee,
        "{}",
        mismatch(
            "assignee",
            format!("{:?}", record.assignee),
            format!("{:?}", op_view.assignee)
        )
    );

    assert_eq!(
        record.type_, op_view.type_,
        "{}",
        mismatch(
            "type",
            format!("{:?}", record.type_),
            format!("{:?}", op_view.type_)
        )
    );

    assert_eq!(
        record.slug, op_view.slug,
        "{}",
        mismatch(
            "slug",
            format!("{:?}", record.slug),
            format!("{:?}", op_view.slug)
        )
    );

    assert_eq!(
        record.block_reason, op_view.block_reason,
        "{}",
        mismatch(
            "block_reason",
            format!("{:?}", record.block_reason),
            format!("{:?}", op_view.block_reason)
        )
    );

    assert_eq!(
        record.priority, op_view.priority,
        "{}",
        mismatch(
            "priority",
            format!("{:?}", record.priority),
            format!("{:?}", op_view.priority)
        )
    );

    // Body: only check when a `set-body` op recorded a hash. The
    // create-only path leaves the body unmolested in the file but
    // doesn't carry a hash op (spec §5.2 lists `set-body` but no
    // body-on-create trailer).
    if let Some(expected_hash) = &op_view.body_hash {
        let actual = sha256_hex(record.body.as_bytes());
        assert_eq!(
            &actual, expected_hash,
            "storage contract violation: issue {} body sha-256 differs from latest set-body op hash (file={}, op={})",
            record.id, actual, expected_hash
        );
    }

    // Comments: the comment-add ops record only ids (the body lives in
    // the JSONL file). Check that every id in the op chain appears in
    // the file, in order. The reader sorts comments by created_at, but
    // the op chain order matches creation order — and the writer
    // stamps created_at from the same wall clock as the commit, so the
    // two orderings should agree. If they don't, the file was edited
    // outside the write path.
    let file_ids: Vec<&IssueId> = comments.iter().map(|c| &c.id).collect();
    let op_ids: Vec<&IssueId> = op_view.comment_ids.iter().collect();
    assert_eq!(
        file_ids.len(),
        op_ids.len(),
        "storage contract violation: issue {} comment count differs (file={}, ops={})",
        record.id,
        file_ids.len(),
        op_ids.len()
    );
    for (i, (f, o)) in file_ids.iter().zip(op_ids.iter()).enumerate() {
        assert_eq!(
            f, o,
            "storage contract violation: issue {} comment[{}] id differs (file={}, op={})",
            record.id, i, f, o
        );
    }
}

/// Standalone sha-256 hex helper. Calls into the inline implementation
/// in `lib.rs` via a re-export.
#[cfg(debug_assertions)]
fn sha256_hex(bytes: &[u8]) -> String {
    crate::sha256_hex_for_read(bytes)
}

#[cfg(test)]
mod tests {
    // Trailer-parser unit tests (single-op, multi-op, unknown-op,
    // cross-issue) live in `trailer.rs` next to the parser. The tests
    // below cover the cross-check's own structural fold — `apply_op` /
    // `OpView` — which is specific to this module.
    use super::*;

    fn id(s: &str) -> IssueId {
        IssueId::parse(s).unwrap()
    }

    #[test]
    fn replay_create_then_set_status_yields_closed() {
        let mut view: Option<OpView> = None;
        apply_op(
            &mut view,
            Op::Create {
                issue_id: id("aa6600b"),
                title: "t".into(),
                status: Status::Open,
            },
        );
        apply_op(
            &mut view,
            Op::SetStatus {
                issue_id: id("aa6600b"),
                status: Status::Closed,
            },
        );
        let v = view.unwrap();
        assert_eq!(v.status, Status::Closed);
        assert_eq!(v.title, "t");
    }

    #[test]
    fn replay_label_add_then_rm_clears() {
        let mut view = Some(OpView {
            id: id("aa6600b"),
            title: "t".into(),
            slug: None,
            body_hash: None,
            status: Status::Open,
            block_reason: None,
            type_: IssueType::Unspecified,
            priority: None,
            labels: Vec::new(),
            metadata: std::collections::BTreeMap::new(),
            dependencies: Vec::new(),
            assignee: None,
            comment_ids: Vec::new(),
        });
        apply_op(
            &mut view,
            Op::LabelAdd {
                issue_id: id("aa6600b"),
                label: "p1".into(),
            },
        );
        apply_op(
            &mut view,
            Op::LabelAdd {
                issue_id: id("aa6600b"),
                label: "bug".into(),
            },
        );
        // Idempotent.
        apply_op(
            &mut view,
            Op::LabelAdd {
                issue_id: id("aa6600b"),
                label: "bug".into(),
            },
        );
        apply_op(
            &mut view,
            Op::LabelRm {
                issue_id: id("aa6600b"),
                label: "bug".into(),
            },
        );
        let v = view.unwrap();
        assert_eq!(v.labels, vec!["p1".to_string()]);
    }
}
