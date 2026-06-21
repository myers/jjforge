//! Shared `Jjf-Op:` trailer parser.
//!
//! Both the read path (`read.rs`, debug-build cross-check that re-derives
//! a bug's structural state from its op chain) and the history path
//! (`history.rs`, the per-bug op-by-op timeline) need to parse trailer
//! stanzas out of a commit description and turn them into typed `Op`
//! values. This module is the single source of truth for that parse so
//! the two callers can't drift.
//!
//! See `docs/storage-format.md` §5 for the trailer schema, §5.3 for the
//! multi-op ordering rule, and §5.7 for the create-time multi-op rule.

use crate::id::BugId;
use crate::op::Op;
use crate::record::Status;

/// Parse all `Jjf-Op:` stanzas from a commit description, returning
/// typed ops in trailer order. Stanzas whose `Jjf-Bug:` field doesn't
/// match `id` are dropped (spec allows multi-bug commits even though
/// the v1 writer doesn't emit them). Unknown op-types are tolerated
/// per spec §5.2 — they're skipped silently.
pub(crate) fn parse_ops(desc: &str, id: &BugId) -> Vec<Op> {
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
}
