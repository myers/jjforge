//! Per-issue record schema for `issues/<id>.json` (spec §3) and comment
//! schema for `issues/<id>.comments.jsonl` (spec §4).
//!
//! Field ORDER on the JSON record matters: spec §3.3 requires the
//! writer to emit fields in the schema order so jj's textual
//! auto-merger gets stable line-by-line diffs. We get this for free
//! with serde's derive because struct fields serialize in declaration
//! order.

use serde::{Deserialize, Serialize};

use crate::id::IssueId;

/// Issue status. v1 has exactly two values; spec §3 calls out the enum
/// as extensible later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Open,
    Closed,
}

impl Status {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Closed => "closed",
        }
    }
}

/// The full v2 record. Field declaration order == on-disk emission
/// order. Don't reorder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueRecord {
    pub version: u32,
    pub id: IssueId,
    pub title: String,
    pub body: String,
    pub status: Status,
    pub labels: Vec<String>,
    pub dependencies: Vec<IssueId>,
    pub assignee: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// User-supplied input to `Storage::create_issue`. The crate fills in
/// id, status, created_at, updated_at, version.
#[derive(Debug, Clone, Default)]
pub struct IssueDraft {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub dependencies: Vec<IssueId>,
    pub assignee: Option<String>,
}

/// One line of `issues/<id>.comments.jsonl`. Serialized one per line,
/// no surrounding array (spec §4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: IssueId,
    pub author: String,
    pub created_at: String,
    pub body: String,
}

/// The read-side view of an issue: the latest scalar state plus the full
/// chronological comment thread. Returned by `Storage::read`.
///
/// This is a flattened projection of the on-disk pair
/// (`issues/<id>.json` + `issues/<id>.comments.jsonl`) that callers (the
/// `jjf` CLI, the merge driver once it consumes records) can use
/// without knowing about the underlying file layout.
///
/// Fields mirror `IssueRecord` plus a `comments` vector. `labels` and
/// `dependencies` are sorted (the writer guarantees that already, but
/// the read path re-sorts defensively); `comments` are sorted by
/// `created_at` ascending (chronological).
///
/// The `Serialize` impl is the structured payload `jjf show --json`
/// emits — field declaration order doubles as on-the-wire JSON field
/// order, mirroring `IssueRecord`'s schema-stability rule (spec §3.3)
/// even though no merge ever sees this projection on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub id: IssueId,
    pub title: String,
    pub body: String,
    pub status: Status,
    pub labels: Vec<String>,
    pub dependencies: Vec<IssueId>,
    pub assignee: Option<String>,
    pub comments: Vec<Comment>,
    pub created_at: String,
    pub updated_at: String,
}
