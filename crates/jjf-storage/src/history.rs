//! Per-issue op-by-op timeline reconstructed from the `Jjf-Op:` trailer
//! chain on the `issues` bookmark.
//!
//! Given an issue id, returns one [`HistoryEntry`] per op, in commit
//! order (oldest first). A commit that carries multiple ops (e.g. the
//! create-time multi-op stanza of spec §5.7, or a single-call close +
//! label-add) emits one entry per op — all sharing the same `commit`,
//! `author`, and `timestamp`, with the op-specific payload in `op`.
//!
//! The trailer parser is shared with `read.rs`'s debug-build
//! cross-check (see `trailer.rs`) — the consolidation is the whole
//! point of this module.
//!
//! # Scope
//!
//! This is the per-issue stream. A whole-bookmark audit log
//! (every issue, every op) is a separate ticket.

use crate::git::GitRepo;
use crate::id::IssueId;
use crate::op::Op;
use crate::trailer::parse_ops_with_meta;
use crate::v3_write;
use crate::{Error, Result};

/// One row of the op-by-op timeline.
///
/// `commit`, `author`, and `timestamp` come from `jj log` metadata on
/// the commit that carried the op. `op` is the parsed `Jjf-Op:` stanza
/// for *this* row — commits with N ops produce N rows that share the
/// commit-level fields and differ only in `op`.
///
/// `timestamp` is the commit's **author timestamp** formatted as
/// `YYYY-MM-DDTHH:MM:SSZ` (UTC, second resolution). This is what jj
/// stamps on the commit when the writer's `jj new bookmarks(issues)`
/// lands; it's distinct from (and may differ by a fraction of a second
/// from) the writer's own `now_rfc3339()` that lands in the on-disk
/// record's `created_at` / `updated_at`. See spec §5 and the closing
/// comment on `b650d74` for why the two clocks don't always agree.
///
/// `jjf_at` is the value of the optional `Jjf-At:` trailer (RFC 3339
/// with nanosecond precision, UTC, set by the writer at the moment of
/// the op). Stanzas predating the spec §5 op-time bump return `None`;
/// the op-space merge driver's ordering tuple treats those as older
/// than any stamped op at the same `timestamp` second.
///
/// `author` is rendered as `Name <email>` by jj's `author` template
/// field, matching git's standard. May be empty if no jj user is
/// configured (e.g. throwaway test repos with no `user.name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub commit: String,
    pub author: String,
    pub timestamp: String,
    pub jjf_at: Option<String>,
    /// 0-based position of this op within its commit's trailer stanza
    /// block. Used as the deterministic tiebreaker when two ops share
    /// `(jjf_at, commit)` — every multi-op commit has at least two
    /// stanzas distinguishable only by this index.
    pub trailer_index: u32,
    pub op: Op,
}

/// Walk the per-issue op chain on the v3 per-issue ref
/// (`refs/jjf/issues/<id>`) and return one entry per op, oldest first.
///
/// V3 counterpart of `read_history_at`. The v3 storage shape stores
/// each issue's op log as the commit chain on its own ref — there's
/// no bookmark, no `ancestors()` revset, no v1/v2 path-filter dance.
/// Just `git log refs/jjf/issues/<id>`, oldest-first, parse the
/// trailer block off each commit's full message.
///
/// Errors:
/// - `IssueNotFound` if the ref doesn't exist OR exists but its
///   commit chain has no Jjf-Op trailer.
/// - `Git` if the underlying `git log` shell-out fails.
pub(crate) fn read_history_at_v3(
    git: &GitRepo,
    id: &IssueId,
) -> Result<Vec<HistoryEntry>> {
    let ref_name = v3_write::refs::issue_ref(id);
    read_history_at_v3_rev(git, &ref_name, id)
}

/// Walk the per-issue op chain rooted at an arbitrary revision (commit
/// oid or ref name). The pull-merge driver uses this with each parent's
/// commit oid so it can reduce both sides' ops without re-pointing the
/// local ref first. Same contract as [`read_history_at_v3`] otherwise.
pub(crate) fn read_history_at_v3_rev(
    git: &GitRepo,
    rev: &str,
    id: &IssueId,
) -> Result<Vec<HistoryEntry>> {
    let walked = git
        .walk_commits(rev)
        .map_err(Error::Git)?;

    if walked.is_empty() {
        return Err(Error::IssueNotFound(id.clone()));
    }

    let mut out = Vec::new();
    for w in walked {
        // Trailer parser filters to this issue's stanzas and drops
        // unknown op kinds — same contract as the v2 path. The order
        // within a multi-op commit is preserved (the parser walks the
        // trailer block top-to-bottom); we re-emit `trailer_index`
        // for the LWW ordering tuple's final tiebreaker.
        for (idx, parsed) in
            parse_ops_with_meta(&w.message, id).into_iter().enumerate()
        {
            out.push(HistoryEntry {
                commit: w.commit.clone(),
                author: w.author.clone(),
                timestamp: w.timestamp.clone(),
                jjf_at: parsed.jjf_at,
                trailer_index: idx as u32,
                op: parsed.op,
            });
        }
    }

    if out.is_empty() {
        // The ref exists but no Jjf-Op trailer for this issue was
        // found — treat as not found, mirroring v2's behavior when
        // `parse_ops_with_meta` filters everything away.
        return Err(Error::IssueNotFound(id.clone()));
    }
    Ok(out)
}

