//! Shared `Jjf-Op:` trailer parser.
//!
//! Both the read path (`read.rs`, debug-build cross-check that re-derives
//! an issue's structural state from its op chain) and the history path
//! (`history.rs`, the per-issue op-by-op timeline) need to parse trailer
//! stanzas out of a commit description and turn them into typed `Op`
//! values. This module is the single source of truth for that parse so
//! the two callers can't drift.
//!
//! See `docs/storage-format.md` §5 for the trailer schema, §5.3 for the
//! multi-op ordering rule, and §5.7 for the create-time multi-op rule.
//!
//! ## v1 → v2 forward compatibility
//!
//! v1 trailers used `Jjf-Bug:` for the issue id; v2 emits `Jjf-Issue:`.
//! The parser accepts BOTH spellings transparently. This is the
//! load-bearing forward-compat seam — repos written under v1 (the
//! original `bugs` bookmark) continue to op-replay through this
//! parser even after the v2 cutover, so the inline-detect migration in
//! `Storage::open` can run safely against existing data.

use crate::id::IssueId;
use crate::op::Op;
use crate::record::Status;

/// A parsed op stanza plus its trailer-level metadata.
///
/// `jjf_at` is the value of the optional `Jjf-At:` trailer (spec §5,
/// the op-time field added when the v1 spec was extended for op-space
/// merge). Stanzas without that field surface `None` here; the merge
/// driver's ordering tuple treats them as "older than any stamped op
/// at the same commit-time second."
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedOp {
    pub op: Op,
    pub jjf_at: Option<String>,
}

/// Parse all `Jjf-Op:` stanzas from a commit description, returning
/// typed ops in trailer order. Stanzas whose `Jjf-Issue:` (or legacy
/// `Jjf-Bug:`) field doesn't match `id` are dropped (spec allows
/// multi-issue commits even though the v1 writer doesn't emit them).
/// Unknown op-types are tolerated per spec §5.2 — they're skipped
/// silently.
///
/// Convenience wrapper around `parse_ops_with_meta` that drops the
/// trailer-level metadata; preserved so call sites that only care
/// about the typed op (the debug-only read-path cross-check, the
/// per-issue history view's payload) don't have to thread the meta
/// they don't use.
pub(crate) fn parse_ops(desc: &str, id: &IssueId) -> Vec<Op> {
    parse_ops_with_meta(desc, id)
        .into_iter()
        .map(|p| p.op)
        .collect()
}

/// Parse all `Jjf-Op:` stanzas from a commit description, returning
/// each typed op alongside the `Jjf-At:` value (if present).
///
/// Stanzas whose `Jjf-Issue:`/`Jjf-Bug:` field doesn't match `id` are
/// dropped, matching `parse_ops`'s semantics. Unknown op-types are
/// tolerated per spec §5.2 — skipped silently.
pub(crate) fn parse_ops_with_meta(desc: &str, id: &IssueId) -> Vec<ParsedOp> {
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
            let jjf_at = stanza
                .iter()
                .find(|(k, _)| *k == "Jjf-At")
                .map(|(_, v)| (*v).to_owned());
            out.push(ParsedOp { op, jjf_at });
        }
    }
    out
}

/// Convert one parsed trailer stanza (starting with `Jjf-Op`) into a
/// typed op for the requested issue, or `None` if it's missing
/// required fields, references a different issue, or has an unknown
/// op-type.
fn stanza_to_op(stanza: &[(&str, &str)], id: &IssueId) -> Option<Op> {
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

    // v2 emits `Jjf-Issue:`; v1 emitted `Jjf-Bug:`. Read either —
    // forward-compat with pre-v2 commits is load-bearing for the
    // inline-detect migration. New name preferred when both are
    // present (defensive — should never happen, but if it does the
    // v2 name wins).
    let issue_id_str = get("Jjf-Issue").or_else(|| get("Jjf-Bug"))?;
    if issue_id_str != id.as_str() {
        // Op for a different issue — drop.
        return None;
    }
    let issue_id = IssueId::parse(&issue_id_str).ok()?;

    let op = match op_type {
        "create" => Op::Create {
            issue_id,
            title: get("Jjf-Title")?,
            status: parse_status(&get("Jjf-Status")?)?,
        },
        "set-title" => Op::SetTitle {
            issue_id,
            title: get("Jjf-Title")?,
        },
        "set-status" => Op::SetStatus {
            issue_id,
            status: parse_status(&get("Jjf-Status")?)?,
        },
        "set-body" => Op::SetBody {
            issue_id,
            body_hash: get("Jjf-Body-Hash")?,
        },
        "label-add" => Op::LabelAdd {
            issue_id,
            label: get("Jjf-Label")?,
        },
        "label-rm" => Op::LabelRm {
            issue_id,
            label: get("Jjf-Label")?,
        },
        "dep-add" => Op::DepAdd {
            issue_id,
            dep: IssueId::parse(&get("Jjf-Dep")?).ok()?,
        },
        "dep-rm" => Op::DepRm {
            issue_id,
            dep: IssueId::parse(&get("Jjf-Dep")?).ok()?,
        },
        "set-assignee" => {
            let v = get("Jjf-Assignee").unwrap_or_default();
            Op::SetAssignee {
                issue_id,
                assignee: if v.is_empty() { None } else { Some(v) },
            }
        }
        "comment-add" => Op::CommentAdd {
            issue_id,
            comment_id: IssueId::parse(&get("Jjf-Comment-Id")?).ok()?,
        },
        "merge" => Op::Merge { issue_id },
        // Unknown op-type: spec §5.2 says tolerate. Skip silently;
        // history callers that care about the unknown can see the
        // raw trailer chain via the underlying `jj log`.
        _ => return None,
    };
    Some(op)
}

fn parse_status(s: &str) -> Option<Status> {
    match s {
        "open" => Some(Status::Open),
        "closed" => Some(Status::Closed),
        _ => None,
    }
}

/// Parse one trailer line. Returns `(key, value)` if it looks like
/// `Key: value`, else `None`. Trailers can have leading whitespace in
/// folded forms; we don't handle continuation lines because the writer
/// never emits them.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> IssueId {
        IssueId::parse(s).unwrap()
    }

    #[test]
    fn parses_single_op_create_trailer() {
        let desc = "\
jjf: issue aa6600b - create

Jjf-Op: create
Jjf-Issue: aa6600b
Jjf-Title: segfault on empty input
Jjf-Status: open
";
        let ops = parse_ops(desc, &id("aa6600b"));
        assert_eq!(
            ops,
            vec![Op::Create {
                issue_id: id("aa6600b"),
                title: "segfault on empty input".into(),
                status: Status::Open,
            }]
        );
    }

    #[test]
    fn parses_v1_legacy_jjf_bug_trailer() {
        // v1 trailers used `Jjf-Bug:`; the parser must accept that
        // spelling so pre-v2 repo data continues to op-replay.
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
                issue_id: id("aa6600b"),
                title: "segfault on empty input".into(),
                status: Status::Open,
            }]
        );
    }

    #[test]
    fn jjf_issue_takes_precedence_over_jjf_bug_when_both_present() {
        // Defensive: should never happen, but if a hand-built stanza
        // carries both, the v2 name wins so the cutover semantics are
        // clear (v2 forward-compat reads v1; v2's name is authoritative
        // when there's a conflict).
        let desc = "\
jjf: issue aa6600b - create

Jjf-Op: create
Jjf-Issue: aa6600b
Jjf-Bug: bbbbbbb
Jjf-Title: t
Jjf-Status: open
";
        let ops = parse_ops(desc, &id("aa6600b"));
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            Op::Create { issue_id, .. } => assert_eq!(issue_id, &id("aa6600b")),
            other => panic!("expected Create, got {:?}", other),
        }
    }

    #[test]
    fn parses_multi_op_stanza_in_order() {
        // Spec §5.5 example.
        let desc = "\
jjf: issue aa6600b - close + label

Closing as fixed in #42.

Jjf-Op: set-status
Jjf-Issue: aa6600b
Jjf-Status: closed
Jjf-Op: label-add
Jjf-Issue: aa6600b
Jjf-Label: fixed
";
        let ops = parse_ops(desc, &id("aa6600b"));
        assert_eq!(
            ops,
            vec![
                Op::SetStatus {
                    issue_id: id("aa6600b"),
                    status: Status::Closed,
                },
                Op::LabelAdd {
                    issue_id: id("aa6600b"),
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
jjf: issue aa6600b - speculative

Jjf-Op: not-yet-invented
Jjf-Issue: aa6600b
Jjf-Foo: bar
Jjf-Op: set-status
Jjf-Issue: aa6600b
Jjf-Status: closed
";
        let ops = parse_ops(desc, &id("aa6600b"));
        assert_eq!(
            ops,
            vec![Op::SetStatus {
                issue_id: id("aa6600b"),
                status: Status::Closed,
            }]
        );
    }

    #[test]
    fn parses_jjf_at_when_present() {
        // Stanza carries the optional Jjf-At trailer — surface it.
        let desc = "\
jjf: issue aa6600b - close

Jjf-Op: set-status
Jjf-Issue: aa6600b
Jjf-At: 2026-06-22T12:34:56.123456789Z
Jjf-Status: closed
";
        let parsed = parse_ops_with_meta(desc, &id("aa6600b"));
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].jjf_at.as_deref(),
            Some("2026-06-22T12:34:56.123456789Z")
        );
        assert_eq!(
            parsed[0].op,
            Op::SetStatus {
                issue_id: id("aa6600b"),
                status: Status::Closed,
            }
        );
    }

    #[test]
    fn jjf_at_absence_is_none_for_forward_compat() {
        // Older fixtures and pre-spec-bump data have no Jjf-At; spec
        // §5 says parsers MUST tolerate that. Surface as None.
        let desc = "\
jjf: issue aa6600b - close

Jjf-Op: set-status
Jjf-Issue: aa6600b
Jjf-Status: closed
";
        let parsed = parse_ops_with_meta(desc, &id("aa6600b"));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].jjf_at, None);
    }

    #[test]
    fn ignores_ops_for_other_issues() {
        // Multi-issue commits aren't a v1 pattern but the spec doesn't
        // forbid them; readers must filter by Jjf-Issue.
        let desc = "\
jjf: cross-issue

Jjf-Op: set-status
Jjf-Issue: bbbbbbb
Jjf-Status: closed
Jjf-Op: set-status
Jjf-Issue: aa6600b
Jjf-Status: closed
";
        let ops = parse_ops(desc, &id("aa6600b"));
        assert_eq!(
            ops,
            vec![Op::SetStatus {
                issue_id: id("aa6600b"),
                status: Status::Closed,
            }]
        );
    }
}
