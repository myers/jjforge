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
//! 1. **File-read.** Pull `issues/<id>.json` and
//!    `issues/<id>.comments.jsonl` straight off the bookmark tip via
//!    `jj file show`.
//! 2. **Op-replay.** Walk `ancestors(bookmarks(issues))` filtered to
//!    `root:issues/<id>.json`, parse the `Jjf-Op:` trailers out of each
//!    commit description (oldest first), and fold them into a record.
//!
//! When file-read and op-replay disagree on a structural field, that's
//! a violation of the storage contract — either the writer didn't
//! record an op for a mutation, or the writer wrote the file without a
//! corresponding op. Crashing in debug builds is the cheapest way to
//! catch a regression in the write path. Release builds trust the
//! file (it's the authoritative copy).
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

use crate::id::IssueId;
use crate::jj::JjRepo;
#[cfg(any(debug_assertions, test))]
use crate::op::Op;
#[cfg(any(debug_assertions, test))]
use crate::record::Status;
use crate::record::{Comment, Issue, IssueRecord};
#[cfg(debug_assertions)]
use crate::trailer::parse_ops;
use crate::{
    issue_comments_relpath, issue_json_relpath, v1_issue_comments_relpath,
    v1_issue_json_relpath, Error, Result, ISSUES_BOOKMARK_REVSET,
};

/// Read a single issue from the `issues` bookmark tip.
///
/// Errors:
/// - `IssueNotFound` if `issues/<id>.json` is absent at the bookmark.
/// - `Json` if the on-disk record or any comment line is malformed.
/// - `Jj` if the underlying `jj` shell-out fails for any non-missing-file
///   reason.
pub(crate) fn read(repo: &JjRepo, id: &IssueId) -> Result<Issue> {
    let record = read_record(repo, id)?;
    let comments = read_comments(repo, id)?;

    // Defensive sort: comments by created_at ascending. The writer
    // appends in order, but the merge driver may union two append
    // streams whose interleaving is undefined. Spec §4.2 says readers
    // *may* re-sort; we do.
    let mut comments = comments;
    comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    #[cfg(debug_assertions)]
    {
        let op_view = replay_ops(repo, id)?;
        // Skip the cross-check whenever the chain has been through a
        // merge commit. The `Jjf-Op: merge` trailer carries no
        // payload (spec §5.2): "the merge driver records the
        // resolution itself in the file diff." Op-replay can therefore
        // walk both branches of the merge, fold their `set-*` ops in
        // some order, and end up at a structural view that does NOT
        // match the file (the merge driver picked a winner that
        // disagrees with whichever side op-replay happened to apply
        // last). The file remains authoritative after a merge; the
        // cross-check is a debug-only safety net for the
        // non-merged write path. A future ticket may enrich the merge
        // trailer with per-field "Jjf-Resolved-*" payload so op-replay
        // can be authoritative across merges too; for now, skip.
        if !op_view.touched_by_merge {
            cross_check(&record, &comments, &op_view);
        }
    }

    Ok(Issue {
        id: record.id,
        title: record.title,
        body: record.body,
        status: record.status,
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
        assignee: record.assignee,
        comments,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

/// Read the JSON record straight off the bookmark tip. Returns
/// `IssueNotFound` if the file is absent at that revision.
fn read_record(repo: &JjRepo, id: &IssueId) -> Result<IssueRecord> {
    let relpath = issue_json_relpath(id);
    let text = match repo.run(&[
        "file",
        "show",
        "-r",
        ISSUES_BOOKMARK_REVSET,
        &format!("root:{}", relpath.display()),
    ]) {
        Ok(s) => s,
        Err(_) => return Err(Error::IssueNotFound(id.clone())),
    };
    Ok(serde_json::from_str(&text)?)
}

/// Read the comments JSONL straight off the bookmark tip. A missing
/// file means "no comments" — the writer creates an empty file on
/// issue creation, but tolerating absence keeps the read path resilient
/// to repos created by future bootstrap paths that might not.
fn read_comments(repo: &JjRepo, id: &IssueId) -> Result<Vec<Comment>> {
    let relpath = issue_comments_relpath(id);
    let text = match repo.run(&[
        "file",
        "show",
        "-r",
        ISSUES_BOOKMARK_REVSET,
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
    /// `Some(hash)` if a `set-body` op was applied; `None` if the issue
    /// was only ever touched by `create` (whose op chain has no body
    /// hash — the create-time body is in the JSON record). We use this
    /// to validate the on-disk body when a hash is available, and skip
    /// the check otherwise.
    body_hash: Option<String>,
    status: Status,
    labels: Vec<String>,
    dependencies: Vec<IssueId>,
    assignee: Option<String>,
    /// Comment IDs in the order they were added (op chain order, oldest
    /// first). Used to validate that the JSONL file matches.
    comment_ids: Vec<IssueId>,
    /// `true` once a `Jjf-Op: merge` trailer has been seen anywhere in
    /// the chain. The cross-check honors this by skipping — see the
    /// rationale at the call-site in `read`.
    touched_by_merge: bool,
}

/// Walk the per-issue op chain and fold it into a structural view.
///
/// Uses `ancestors(bookmarks(issues))` filtered by
/// `root:issues/<id>.json`, templated to dump the full description so
/// we can parse trailers. The output is newest-first; we reverse to
/// oldest-first before folding.
#[cfg(debug_assertions)]
fn replay_ops(repo: &JjRepo, id: &IssueId) -> Result<OpView> {
    let json_relpath = issue_json_relpath(id);
    let comments_relpath = issue_comments_relpath(id);
    let v1_json_relpath = v1_issue_json_relpath(id);
    let v1_comments_relpath = v1_issue_comments_relpath(id);
    // Filter spans all four paths (v1 + v2 × json + comments-jsonl).
    // Same reasoning as `history.rs::read_history_at`:
    //   - v1 paths catch pre-migration ops that touched `bugs/<id>.*`.
    //     Without them, an issue created before the v1→v2 migration
    //     drops its `create` op out of the replay chain and folds to
    //     nothing.
    //   - Both file kinds at each version: spec §5.6 — a comments-jsonl
    //     append in the same second as a prior mutation produces no
    //     json diff and gets missed if we filter only on the json file.
    let sep = "\n----JJF-DESC-END-c0ffee----\n";
    let template = format!("description ++ \"{}\"", sep.replace('\n', "\\n"));
    let raw = repo.run(&[
        "log",
        "--no-graph",
        "-r",
        "ancestors(bookmarks(issues))",
        "-T",
        &template,
        &format!("root:{}", json_relpath.display()),
        &format!("root:{}", comments_relpath.display()),
        &format!("root:{}", v1_json_relpath.display()),
        &format!("root:{}", v1_comments_relpath.display()),
    ])?;

    // Newest-first → oldest-first.
    let mut descs: Vec<&str> = raw.split(sep).filter(|s| !s.trim().is_empty()).collect();
    descs.reverse();

    if descs.is_empty() {
        return Err(Error::IssueNotFound(id.clone()));
    }

    // Fold ops left-to-right. The first commit MUST carry a `create`
    // for this issue; we initialize the view from it.
    let mut view: Option<OpView> = None;
    for desc in descs {
        for op in parse_ops(desc, id) {
            apply_op(&mut view, op);
        }
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
                body_hash: None,
                status,
                labels: Vec::new(),
                dependencies: Vec::new(),
                assignee: None,
                comment_ids: Vec::new(),
                touched_by_merge: false,
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
                Op::DepAdd { dep, .. } => {
                    if !v.dependencies.iter().any(|d| d == &dep) {
                        v.dependencies.push(dep);
                        v.dependencies.sort();
                    }
                }
                Op::DepRm { dep, .. } => v.dependencies.retain(|d| d != &dep),
                Op::SetAssignee { assignee, .. } => v.assignee = assignee,
                Op::CommentAdd { comment_id, .. } => v.comment_ids.push(comment_id),
                Op::Merge { .. } => {
                    // No structural change; the merge driver records
                    // the resolution itself in the file diff. We flag
                    // the view so the cross-check skips — op-replay
                    // can walk the merge's two parent branches in
                    // some order, fold their `set-*` ops, and produce
                    // a structural view that doesn't match the file
                    // (the merge driver's pick disagrees with the
                    // side op-replay happened to apply last).
                    v.touched_by_merge = true;
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
            body_hash: None,
            status: Status::Open,
            labels: Vec::new(),
            dependencies: Vec::new(),
            assignee: None,
            comment_ids: Vec::new(),
            touched_by_merge: false,
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
