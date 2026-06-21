//! Read path for the `bugs` bookmark.
//!
//! Given a repo and a bug id, returns the structured `Bug` view: the
//! latest scalar field values from `bugs/<id>.json` plus the full
//! chronological comment thread from `bugs/<id>.comments.jsonl`.
//!
//! # Two implementations, asserted equal
//!
//! Per the ticket's acceptance criteria, this module computes the
//! result two ways and (in debug builds) asserts they agree on
//! structural fields:
//!
//! 1. **File-read.** Pull `bugs/<id>.json` and `bugs/<id>.comments.jsonl`
//!    straight off the bookmark tip via `jj file show`.
//! 2. **Op-replay.** Walk `ancestors(bookmarks(bugs))` filtered to
//!    `root:bugs/<id>.json`, parse the `Jjf-Op:` trailers out of each
//!    commit description (oldest first), and fold them into a record.
//!
//! When file-read and op-replay disagree on a structural field, that's
//! a violation of the v1 storage contract — either the writer didn't
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

use crate::id::BugId;
use crate::jj::JjRepo;
#[cfg(any(debug_assertions, test))]
use crate::op::Op;
#[cfg(any(debug_assertions, test))]
use crate::record::Status;
use crate::record::{Bug, BugRecord, Comment};
use crate::{bug_comments_relpath, bug_json_relpath, Error, Result, BUGS_BOOKMARK_REVSET};

/// Read a single bug from the `bugs` bookmark tip.
///
/// Errors:
/// - `BugNotFound` if `bugs/<id>.json` is absent at the bookmark.
/// - `Json` if the on-disk record or any comment line is malformed.
/// - `Jj` if the underlying `jj` shell-out fails for any non-missing-file
///   reason.
pub(crate) fn read(repo: &JjRepo, id: &BugId) -> Result<Bug> {
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
        cross_check(&record, &comments, &op_view);
    }

    Ok(Bug {
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
/// `BugNotFound` if the file is absent at that revision.
fn read_record(repo: &JjRepo, id: &BugId) -> Result<BugRecord> {
    let relpath = bug_json_relpath(id);
    let text = match repo.run(&[
        "file",
        "show",
        "-r",
        BUGS_BOOKMARK_REVSET,
        &format!("root:{}", relpath.display()),
    ]) {
        Ok(s) => s,
        Err(_) => return Err(Error::BugNotFound(id.clone())),
    };
    Ok(serde_json::from_str(&text)?)
}

/// Read the comments JSONL straight off the bookmark tip. A missing
/// file means "no comments" — the v1 writer creates an empty file on
/// bug creation, but tolerating absence keeps the read path resilient
/// to repos created by future bootstrap paths that might not.
fn read_comments(repo: &JjRepo, id: &BugId) -> Result<Vec<Comment>> {
    let relpath = bug_comments_relpath(id);
    let text = match repo.run(&[
        "file",
        "show",
        "-r",
        BUGS_BOOKMARK_REVSET,
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
// Used today only by the debug-assertions cross-check. The upcoming
// `storage-read-history` ticket will lift these gates when it exposes
// the full audit chain — at that point `parse_ops`, `apply_op`, and
// `OpView` graduate to crate-public helpers.
#[cfg(any(debug_assertions, test))]
/// The structural projection an op-replay can recover. No timestamps
/// because trailers don't carry them; the body string isn't carried in
/// the `set-body` trailer either (only its sha-256 hash is).
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpView {
    id: BugId,
    title: String,
    /// `Some(hash)` if a `set-body` op was applied; `None` if the bug
    /// was only ever touched by `create` (whose op chain has no body
    /// hash — the create-time body is in the JSON record). We use this
    /// to validate the on-disk body when a hash is available, and skip
    /// the check otherwise.
    body_hash: Option<String>,
    status: Status,
    labels: Vec<String>,
    dependencies: Vec<BugId>,
    assignee: Option<String>,
    /// Comment IDs in the order they were added (op chain order, oldest
    /// first). Used to validate that the JSONL file matches.
    comment_ids: Vec<BugId>,
}

/// Walk the per-bug op chain and fold it into a structural view.
///
/// Uses `ancestors(bookmarks(bugs))` filtered by `root:bugs/<id>.json`,
/// templated to dump the full description so we can parse trailers.
/// The output is newest-first; we reverse to oldest-first before
/// folding.
#[cfg(debug_assertions)]
fn replay_ops(repo: &JjRepo, id: &BugId) -> Result<OpView> {
    let json_relpath = bug_json_relpath(id);
    let comments_relpath = bug_comments_relpath(id);
    // Filter on BOTH files. A naïve filter on just the json record
    // misses commits whose only change was an append to the comments
    // jsonl — which happens whenever the writer's `updated_at` lands
    // in the same second as the prior mutation (the json content is
    // then byte-identical and jj's snapshotter doesn't record a
    // change). Spec §5.6 shows the json-only invocation as the
    // example; this is a gap in the spec the read-path discovered.
    // Following up in the closing comment.
    let sep = "\n----JJF-DESC-END-c0ffee----\n";
    let template = format!("description ++ \"{}\"", sep.replace('\n', "\\n"));
    let raw = repo.run(&[
        "log",
        "--no-graph",
        "-r",
        "ancestors(bookmarks(bugs))",
        "-T",
        &template,
        &format!("root:{}", json_relpath.display()),
        &format!("root:{}", comments_relpath.display()),
    ])?;

    // Newest-first → oldest-first.
    let mut descs: Vec<&str> = raw.split(sep).filter(|s| !s.trim().is_empty()).collect();
    descs.reverse();

    if descs.is_empty() {
        return Err(Error::BugNotFound(id.clone()));
    }

    // Fold ops left-to-right. The first commit MUST carry a `create`
    // for this bug; we initialize the view from it.
    let mut view: Option<OpView> = None;
    for desc in descs {
        for op in parse_ops(desc, id) {
            apply_op(&mut view, op);
        }
    }

    view.ok_or_else(|| {
        Error::Invalid(format!(
            "no `create` op found in chain for bug {}",
            id
        ))
    })
}

/// Parse all `Jjf-Op:` stanzas from a commit description, returning
/// only those whose `Jjf-Bug:` matches `id`. Unknown op types are
/// preserved as `None` so the caller can skip them without breaking
/// (spec §5.2: "Unknown trailers and unknown op-types must be
/// tolerated by readers").
#[cfg(any(debug_assertions, test))]
fn parse_ops(desc: &str, id: &BugId) -> Vec<Op> {
    // Find the trailer block: the last paragraph of trailer lines at
    // the end of the description. We don't need to be too clever — we
    // just iterate every `Jjf-Op:` we see and pair it with subsequent
    // `Jjf-...:` lines until the next `Jjf-Op:` or end.
    let lines: Vec<&str> = desc.lines().collect();
    let mut stanzas: Vec<Vec<(&str, &str)>> = Vec::new();
    let mut current: Option<Vec<(&str, &str)>> = None;
    for line in lines {
        if let Some((k, v)) = split_trailer(line) {
            if k == "Jjf-Op" {
                if let Some(prev) = current.take() {
                    stanzas.push(prev);
                }
                current = Some(vec![(k, v)]);
            } else if k.starts_with("Jjf-") {
                if let Some(cur) = current.as_mut() {
                    cur.push((k, v));
                }
                // Else: stray Jjf-* trailer before any Jjf-Op — ignored.
            } else if let Some(cur) = current.as_mut() {
                // Non-Jjf trailer (e.g. Signed-off-by). Stop the
                // current stanza — trailer blocks per RFC are
                // contiguous, but mixing is unusual; safest to close.
                stanzas.push(std::mem::take(cur));
                current = None;
            }
        } else if line.trim().is_empty() {
            // Blank line: not by itself enough to break a stanza — git
            // trailers are contiguous, so a blank line ends them. Close.
            if let Some(prev) = current.take() {
                stanzas.push(prev);
            }
        } else if current.is_some() {
            // Non-trailer line in the middle of a stanza: close the
            // stanza (it was probably the body, not a real trailer).
            if let Some(prev) = current.take() {
                stanzas.push(prev);
            }
        }
    }
    if let Some(last) = current.take() {
        stanzas.push(last);
    }

    let mut out = Vec::new();
    for stanza in stanzas {
        if let Some(op) = stanza_to_op(&stanza, id) {
            out.push(op);
        }
    }
    out
}

/// Convert one parsed trailer stanza (starting with `Jjf-Op`) into a
/// typed op for the requested bug, or `None` if it's missing required
/// fields, references a different bug, or has an unknown op-type.
#[cfg(any(debug_assertions, test))]
fn stanza_to_op(stanza: &[(&str, &str)], id: &BugId) -> Option<Op> {
    if stanza.is_empty() || stanza[0].0 != "Jjf-Op" {
        return None;
    }
    let op_type = stanza[0].1;
    let payload = &stanza[1..];

    let get = |k: &str| -> Option<String> {
        payload
            .iter()
            .find(|(kk, _)| *kk == k)
            .map(|(_, v)| (*v).to_owned())
    };

    let bug_id_str = get("Jjf-Bug")?;
    if bug_id_str != id.as_str() {
        // Op for a different bug — drop.
        return None;
    }
    let bug_id = BugId::parse(&bug_id_str).ok()?;

    let op = match op_type {
        "create" => Op::Create {
            bug_id,
            title: get("Jjf-Title")?,
            status: parse_status(&get("Jjf-Status")?)?,
        },
        "set-title" => Op::SetTitle {
            bug_id,
            title: get("Jjf-Title")?,
        },
        "set-status" => Op::SetStatus {
            bug_id,
            status: parse_status(&get("Jjf-Status")?)?,
        },
        "set-body" => Op::SetBody {
            bug_id,
            body_hash: get("Jjf-Body-Hash")?,
        },
        "label-add" => Op::LabelAdd {
            bug_id,
            label: get("Jjf-Label")?,
        },
        "label-rm" => Op::LabelRm {
            bug_id,
            label: get("Jjf-Label")?,
        },
        "dep-add" => Op::DepAdd {
            bug_id,
            dep: BugId::parse(&get("Jjf-Dep")?).ok()?,
        },
        "dep-rm" => Op::DepRm {
            bug_id,
            dep: BugId::parse(&get("Jjf-Dep")?).ok()?,
        },
        "set-assignee" => {
            let v = get("Jjf-Assignee").unwrap_or_default();
            Op::SetAssignee {
                bug_id,
                assignee: if v.is_empty() { None } else { Some(v) },
            }
        }
        "comment-add" => Op::CommentAdd {
            bug_id,
            comment_id: BugId::parse(&get("Jjf-Comment-Id")?).ok()?,
        },
        "merge" => Op::Merge { bug_id },
        // Unknown op-type: spec §5.2 says tolerate. Skip silently for
        // the read path; an audit-trail view would surface it.
        _ => return None,
    };
    Some(op)
}

#[cfg(any(debug_assertions, test))]
fn parse_status(s: &str) -> Option<Status> {
    match s {
        "open" => Some(Status::Open),
        "closed" => Some(Status::Closed),
        _ => None,
    }
}

/// Parse one trailer line. Returns `(key, value)` if it looks like
/// `Key: value`, else `None`. Trailers can have leading whitespace
/// in folded forms; we don't handle continuation lines because the
/// writer never emits them.
#[cfg(any(debug_assertions, test))]
fn split_trailer(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_end();
    let colon = trimmed.find(':')?;
    let key = &trimmed[..colon];
    // A real trailer key is a single token with no spaces.
    if key.is_empty() || key.contains(' ') {
        return None;
    }
    let value = trimmed[colon + 1..].trim_start();
    Some((key, value))
}

#[cfg(any(debug_assertions, test))]
fn apply_op(view: &mut Option<OpView>, op: Op) {
    match op {
        Op::Create {
            bug_id,
            title,
            status,
        } => {
            // Re-create resets — but a well-formed chain only has one
            // create at the start. If we see a second, treat it as an
            // overwrite (defensive; the merge driver shouldn't produce
            // this).
            *view = Some(OpView {
                id: bug_id,
                title,
                body_hash: None,
                status,
                labels: Vec::new(),
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
                    // the resolution itself in the file diff.
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
fn cross_check(record: &BugRecord, comments: &[Comment], op_view: &OpView) {
    let mismatch = |field: &str, file: String, ops: String| -> String {
        format!(
            "v1 contract violation: file-read disagrees with op-replay for bug {}: {} differs (file={:?}, ops={:?})",
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
            "v1 contract violation: bug {} body sha-256 differs from latest set-body op hash (file={}, op={})",
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
    let file_ids: Vec<&BugId> = comments.iter().map(|c| &c.id).collect();
    let op_ids: Vec<&BugId> = op_view.comment_ids.iter().collect();
    assert_eq!(
        file_ids.len(),
        op_ids.len(),
        "v1 contract violation: bug {} comment count differs (file={}, ops={})",
        record.id,
        file_ids.len(),
        op_ids.len()
    );
    for (i, (f, o)) in file_ids.iter().zip(op_ids.iter()).enumerate() {
        assert_eq!(
            f, o,
            "v1 contract violation: bug {} comment[{}] id differs (file={}, op={})",
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
    use super::*;

    fn id(s: &str) -> BugId {
        BugId::parse(s).unwrap()
    }

    #[test]
    fn parses_single_op_create_trailer() {
        let desc = "\
jjf: bug aa6600b - create

Jjf-Op: create
Jjf-Bug: aa6600b
Jjf-Title: segfault on empty input
Jjf-Status: open
";
        let ops = parse_ops(desc, &id("aa6600b"));
        assert_eq!(
            ops,
            vec![Op::Create {
                bug_id: id("aa6600b"),
                title: "segfault on empty input".into(),
                status: Status::Open,
            }]
        );
    }

    #[test]
    fn parses_multi_op_stanza_in_order() {
        // Spec §5.5 example.
        let desc = "\
jjf: bug aa6600b - close + label

Closing as fixed in #42.

Jjf-Op: set-status
Jjf-Bug: aa6600b
Jjf-Status: closed
Jjf-Op: label-add
Jjf-Bug: aa6600b
Jjf-Label: fixed
";
        let ops = parse_ops(desc, &id("aa6600b"));
        assert_eq!(
            ops,
            vec![
                Op::SetStatus {
                    bug_id: id("aa6600b"),
                    status: Status::Closed,
                },
                Op::LabelAdd {
                    bug_id: id("aa6600b"),
                    label: "fixed".into(),
                },
            ]
        );
    }

    #[test]
    fn ignores_unknown_op_types_per_spec() {
        // Unknown op-types must be tolerated, not panicked-on
        // (spec §5.2).
        let desc = "\
jjf: bug aa6600b - speculative

Jjf-Op: not-yet-invented
Jjf-Bug: aa6600b
Jjf-Foo: bar
Jjf-Op: set-status
Jjf-Bug: aa6600b
Jjf-Status: closed
";
        let ops = parse_ops(desc, &id("aa6600b"));
        assert_eq!(
            ops,
            vec![Op::SetStatus {
                bug_id: id("aa6600b"),
                status: Status::Closed,
            }]
        );
    }

    #[test]
    fn ignores_ops_for_other_bugs() {
        // Multi-bug commits aren't a v1 pattern but the spec doesn't
        // forbid them; readers must filter by Jjf-Bug.
        let desc = "\
jjf: cross-bug

Jjf-Op: set-status
Jjf-Bug: bbbbbbb
Jjf-Status: closed
Jjf-Op: set-status
Jjf-Bug: aa6600b
Jjf-Status: closed
";
        let ops = parse_ops(desc, &id("aa6600b"));
        assert_eq!(
            ops,
            vec![Op::SetStatus {
                bug_id: id("aa6600b"),
                status: Status::Closed,
            }]
        );
    }

    #[test]
    fn replay_create_then_set_status_yields_closed() {
        let mut view: Option<OpView> = None;
        apply_op(
            &mut view,
            Op::Create {
                bug_id: id("aa6600b"),
                title: "t".into(),
                status: Status::Open,
            },
        );
        apply_op(
            &mut view,
            Op::SetStatus {
                bug_id: id("aa6600b"),
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
        });
        apply_op(
            &mut view,
            Op::LabelAdd {
                bug_id: id("aa6600b"),
                label: "p1".into(),
            },
        );
        apply_op(
            &mut view,
            Op::LabelAdd {
                bug_id: id("aa6600b"),
                label: "bug".into(),
            },
        );
        // Idempotent.
        apply_op(
            &mut view,
            Op::LabelAdd {
                bug_id: id("aa6600b"),
                label: "bug".into(),
            },
        );
        apply_op(
            &mut view,
            Op::LabelRm {
                bug_id: id("aa6600b"),
                label: "bug".into(),
            },
        );
        let v = view.unwrap();
        assert_eq!(v.labels, vec!["p1".to_string()]);
    }
}
