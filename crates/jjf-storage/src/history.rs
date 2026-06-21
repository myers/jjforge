//! Per-bug op-by-op timeline reconstructed from the `Jjf-Op:` trailer
//! chain on the `bugs` bookmark.
//!
//! Given a bug id, returns one [`HistoryEntry`] per op, in commit order
//! (oldest first). A commit that carries multiple ops (e.g. the
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
//! This is the per-bug stream. A whole-bookmark audit log
//! (every bug, every op) is a separate ticket.

use crate::id::BugId;
use crate::jj::JjRepo;
use crate::op::Op;
use crate::trailer::parse_ops;
use crate::{bug_comments_relpath, bug_json_relpath, Error, Result};

/// One row of the op-by-op timeline.
///
/// `commit`, `author`, and `timestamp` come from `jj log` metadata on
/// the commit that carried the op. `op` is the parsed `Jjf-Op:` stanza
/// for *this* row — commits with N ops produce N rows that share the
/// commit-level fields and differ only in `op`.
///
/// `timestamp` is the commit's **author timestamp** formatted as
/// `YYYY-MM-DDTHH:MM:SSZ` (UTC, second resolution). This is what jj
/// stamps on the commit when the writer's `jj new bookmarks(bugs)`
/// lands; it's distinct from (and may differ by a fraction of a second
/// from) the writer's own `now_rfc3339()` that lands in the on-disk
/// record's `created_at` / `updated_at`. See spec §5 and the closing
/// comment on `b650d74` for why the two clocks don't always agree.
///
/// `author` is rendered as `Name <email>` by jj's `author` template
/// field, matching git's standard. May be empty if no jj user is
/// configured (e.g. throwaway test repos with no `user.name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub commit: String,
    pub author: String,
    pub timestamp: String,
    pub op: Op,
}

/// Walk the per-bug op chain on the `bugs` bookmark and return one
/// entry per op, oldest first.
///
/// Errors:
/// - `BugNotFound` if the chain is empty (no commit on the bookmark
///   touches `bugs/<id>.json` or `bugs/<id>.comments.jsonl`).
/// - `Jj` if the underlying `jj log` shell-out fails.
pub(crate) fn read_history(repo: &JjRepo, id: &BugId) -> Result<Vec<HistoryEntry>> {
    let json_relpath = bug_json_relpath(id);
    let comments_relpath = bug_comments_relpath(id);

    // Same path filter as `read.rs`'s replay query: filter on BOTH the
    // json record AND the comments jsonl. A naïve filter on just the
    // json record misses commits whose only change was an append to
    // the comments jsonl — which happens whenever the writer's
    // `updated_at` rewrite lands in the same second as the prior
    // mutation (the json content is then byte-identical and jj's
    // snapshotter doesn't record a change). See spec §5.6 and the
    // closing comment on `b650d74`.
    //
    // We emit one record per commit, packing four fields delimited by
    // a per-field sentinel and a per-record terminator. The sentinels
    // are deliberately ugly so they can't collide with anything the
    // writer might put in a commit description.
    let field_sep = "----JJF-HISTORY-FIELD-c0ffee----";
    let record_sep = "\n----JJF-HISTORY-REC-c0ffee----\n";

    // jj's template language supports `\n`, `\t`, `\\`, `\"`, `\0` as
    // string escapes — not `\x..`. The sentinels above are plain ASCII
    // so they go in as-is; only the leading/trailing newlines of
    // `record_sep` need escaping.
    let template = format!(
        "commit_id ++ \"{f}\" ++ author ++ \"{f}\" ++ \
         author.timestamp().utc().format(\"%Y-%m-%dT%H:%M:%SZ\") ++ \"{f}\" ++ \
         description ++ \"{r}\"",
        f = field_sep,
        r = record_sep.replace('\n', "\\n"),
    );

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

    // jj emits records newest-first; we want oldest-first.
    let mut records: Vec<&str> = raw
        .split(record_sep)
        .filter(|s| !s.is_empty())
        .collect();
    records.reverse();

    if records.is_empty() {
        return Err(Error::BugNotFound(id.clone()));
    }

    let mut out = Vec::new();
    for record in records {
        let parts: Vec<&str> = record.splitn(4, field_sep).collect();
        if parts.len() != 4 {
            // A record missing fields means the template / split
            // contract is broken — surface it loudly. This is a bug in
            // the storage crate, not user data.
            return Err(Error::Invalid(format!(
                "history record has {} fields, expected 4: {:?}",
                parts.len(),
                record,
            )));
        }
        let commit = parts[0].to_owned();
        let author = parts[1].to_owned();
        let timestamp = parts[2].to_owned();
        let description = parts[3];

        // One commit, possibly many ops. parse_ops already filters to
        // this bug and drops unknown op-types per spec §5.2; we trust
        // its order (trailer order = spec §5.3 application order).
        for op in parse_ops(description, id) {
            out.push(HistoryEntry {
                commit: commit.clone(),
                author: author.clone(),
                timestamp: timestamp.clone(),
                op,
            });
        }
    }

    Ok(out)
}
