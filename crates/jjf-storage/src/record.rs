//! Per-bug record schema for `bugs/<id>.json` (spec §3) and comment
//! schema for `bugs/<id>.comments.jsonl` (spec §4).
//!
//! Field ORDER on the JSON record matters: spec §3.3 requires the
//! writer to emit fields in the schema order so jj's textual
//! auto-merger gets stable line-by-line diffs. We get this for free
//! with serde's derive because struct fields serialize in declaration
//! order.

use serde::{Deserialize, Serialize};

use crate::id::BugId;

/// Bug status. v1 has exactly two values; spec §3 calls out the enum
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

/// The full v1 record. Field declaration order == on-disk emission
/// order. Don't reorder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BugRecord {
    pub version: u32,
    pub id: BugId,
    pub title: String,
    pub body: String,
    pub status: Status,
    pub labels: Vec<String>,
    pub dependencies: Vec<BugId>,
    pub assignee: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// User-supplied input to `Storage::create_bug`. The crate fills in
/// id, status, created_at, updated_at, version.
#[derive(Debug, Clone, Default)]
pub struct BugDraft {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub dependencies: Vec<BugId>,
    pub assignee: Option<String>,
}

/// One line of `bugs/<id>.comments.jsonl`. Serialized one per line,
/// no surrounding array (spec §4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: BugId,
    pub author: String,
    pub created_at: String,
    pub body: String,
}
