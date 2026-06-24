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
use crate::jj::JjRepo;
use crate::op::Op;
use crate::trailer::parse_ops_with_meta;
use crate::v3_write;
use crate::{
    issue_comments_relpath, issue_json_relpath, v1_issue_comments_relpath,
    v1_issue_json_relpath, Error, Result,
};

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

/// Walk the per-issue op chain on the `issues` bookmark and return one
/// entry per op, oldest first.
///
/// Errors:
/// - `IssueNotFound` if the chain is empty (no commit on the bookmark
///   touches `issues/<id>.json` or `issues/<id>.comments.jsonl`).
/// - `Jj` if the underlying `jj log` shell-out fails.
pub(crate) fn read_history(repo: &JjRepo, id: &IssueId) -> Result<Vec<HistoryEntry>> {
    read_history_at(repo, "bookmarks(issues)", id)
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
    let walked = git
        .walk_commits(&ref_name)
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

/// Walk the per-issue op chain rooted at `rev` and return one entry per
/// op, oldest first. The default `read_history` is this with `rev =
/// bookmarks(issues)`; pass an explicit commit (e.g. a change_id short
/// from `issues_heads`) to walk one head of a divergent bookmark
/// independently of the others.
///
/// Errors:
/// - `IssueNotFound` if no commit reachable from `rev` touches this
///   issue's files.
/// - `Jj` if the underlying `jj log` shell-out fails.
pub(crate) fn read_history_at(
    repo: &JjRepo,
    rev: &str,
    id: &IssueId,
) -> Result<Vec<HistoryEntry>> {
    let json_relpath = issue_json_relpath(id);
    let comments_relpath = issue_comments_relpath(id);
    let v1_json_relpath = v1_issue_json_relpath(id);
    let v1_comments_relpath = v1_issue_comments_relpath(id);

    // Path filter spans four files for v1+v2 coverage:
    //   - Current v2 paths (`issues/<id>.json`, `issues/<id>.comments.jsonl`).
    //   - Pre-migration v1 paths (`bugs/<id>.json`, `bugs/<id>.comments.jsonl`).
    //
    // The v1 paths are needed because the inline v1→v2 migration
    // commit renames the file; ancestor commits still touched the old
    // path. Without the v1 filter entry, the walker drops every
    // pre-migration op out of the chain and `read.rs`'s replay can't
    // find the issue's `create` op.
    //
    // Filtering on BOTH json AND comments-jsonl at each version is
    // load-bearing — see spec §5.6 and the regression test
    // `read_history_walks_same_second_comment_appends` in
    // `crates/jjf-storage/tests/integration.rs`. The case: two
    // `add_comment` calls land within the same wall-clock second.
    // `add_comment` stamps `record.updated_at = now_rfc3339()` at
    // second resolution, so both writes produce a byte-identical
    // JSON file (nothing else in the record changes). jj snapshots
    // by content; with no JSON delta on the second commit, only the
    // comments-jsonl file changes. A filter that names only the
    // JSON path would miss that second commit entirely — verified
    // in issue 004dd23: dropping the comments-jsonl path entry
    // dropped 9 of 12 ops in the regression test.
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

    let ancestors_rev = format!("ancestors({rev})");
    let raw = repo.run(&[
        "log",
        "--no-graph",
        "-r",
        &ancestors_rev,
        "-T",
        &template,
        &format!("root:{}", json_relpath.display()),
        &format!("root:{}", comments_relpath.display()),
        &format!("root:{}", v1_json_relpath.display()),
        &format!("root:{}", v1_comments_relpath.display()),
    ])?;

    // jj emits records newest-first; we want oldest-first.
    let mut records: Vec<&str> = raw
        .split(record_sep)
        .filter(|s| !s.is_empty())
        .collect();
    records.reverse();

    if records.is_empty() {
        return Err(Error::IssueNotFound(id.clone()));
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

        // One commit, possibly many ops. parse_ops_with_meta already
        // filters to this issue and drops unknown op-types per spec §5.2;
        // we trust its order (trailer order = spec §5.3 application
        // order). `trailer_index` is the 0-based stanza position within
        // this commit — used by the op-space merge driver as the
        // final tiebreaker in the LWW ordering tuple.
        for (idx, parsed) in parse_ops_with_meta(description, id).into_iter().enumerate()
        {
            out.push(HistoryEntry {
                commit: commit.clone(),
                author: author.clone(),
                timestamp: timestamp.clone(),
                jjf_at: parsed.jjf_at,
                trailer_index: idx as u32,
                op: parsed.op,
            });
        }
    }

    Ok(out)
}
