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

/// Coarse issue classifier (spec v2.1 §3.1, added in the
/// `issue-type-and-slug-fields` ticket). Orthogonal to `Status` —
/// `type` says "what is this" (bug, feature, epic, …), `Status` says
/// "where is it in the lifecycle" (open, closed). The default
/// (`Unspecified`) is what every pre-existing record reads as.
///
/// Wire spelling is lowercase (`bug`, `feature`, …). Adding a new
/// variant is a v2.x bump; the parser tolerates unknown values by
/// surfacing them through the standard serde-derive deserialize
/// failure, which `Storage::read` translates into `Error::Json` —
/// future readers should add the variant when older repos surface it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueType {
    /// Defect / regression in already-shipped behavior.
    Bug,
    /// New capability to be built.
    Feature,
    /// Multi-ticket milestone (parent of `epic:<slug>`-labeled work).
    Epic,
    /// Investigation / spike whose closing comment pins a verdict.
    Research,
    /// The standing roadmap ticket. One per repo by convention.
    Roadmap,
    /// Default for any record whose creator didn't choose a type.
    #[default]
    Unspecified,
}

impl IssueType {
    /// The lowercase wire spelling used in trailer payloads
    /// (`Jjf-Type:`), `jjf` plain-text output, and CLI `--type`
    /// flag values. Mirrors [`Status::as_str`] in spirit.
    pub fn as_str(self) -> &'static str {
        match self {
            IssueType::Bug => "bug",
            IssueType::Feature => "feature",
            IssueType::Epic => "epic",
            IssueType::Research => "research",
            IssueType::Roadmap => "roadmap",
            IssueType::Unspecified => "unspecified",
        }
    }

    /// Parse the lowercase wire spelling. Returns `None` for any
    /// other value; callers translate to their own error type. The
    /// trailer parser uses this for `Jjf-Type:` values; the CLI's
    /// clap `ValueEnum` provides its own mapping.
    pub(crate) fn parse_wire(s: &str) -> Option<IssueType> {
        match s {
            "bug" => Some(IssueType::Bug),
            "feature" => Some(IssueType::Feature),
            "epic" => Some(IssueType::Epic),
            "research" => Some(IssueType::Research),
            "roadmap" => Some(IssueType::Roadmap),
            "unspecified" => Some(IssueType::Unspecified),
            _ => None,
        }
    }
}

/// The full v2 record. Field declaration order == on-disk emission
/// order. Don't reorder.
///
/// **v2.1 (`issue-type-and-slug-fields`):** the new `type` field
/// sits after `status` and before `labels`; the new `slug` field
/// sits after `title` and before `body`. Both fields are serde-default
/// on read so any pre-v2.1 record (which lacks them) deserializes
/// cleanly. The on-disk emission order matches the declaration order
/// here per spec §3.3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueRecord {
    pub version: u32,
    pub id: IssueId,
    pub title: String,
    /// Optional kebab-case orientation handle. `None` (serialized as
    /// `null`) for issues without a slug. Per spec §3.1 the field
    /// always appears in the record (no `#[serde(skip)]`); a record
    /// without an explicit slug carries `"slug": null`.
    #[serde(default)]
    pub slug: Option<String>,
    pub body: String,
    pub status: Status,
    /// Coarse classifier. Default `Unspecified`; the field always
    /// appears in the record (no `#[serde(skip)]`).
    #[serde(default, rename = "type")]
    pub type_: IssueType,
    pub labels: Vec<String>,
    pub dependencies: Vec<IssueId>,
    pub assignee: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// User-supplied input to `Storage::create_issue`. The crate fills in
/// id, status, created_at, updated_at, version.
///
/// `type_` and `slug` (added in the `issue-type-and-slug-fields`
/// ticket) are `None` by default; an unspecified `type_` becomes
/// [`IssueType::Unspecified`] on disk.
#[derive(Debug, Clone, Default)]
pub struct IssueDraft {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub dependencies: Vec<IssueId>,
    pub assignee: Option<String>,
    /// Coarse classifier. `None` becomes `IssueType::Unspecified` at
    /// write time; non-default values emit a `Jjf-Op: set-type`
    /// stanza in the create-time multi-op commit.
    pub type_: Option<IssueType>,
    /// Kebab-case orientation handle. `None` leaves the slug empty.
    /// Non-`None` values are validated by `Storage::validate_slug`
    /// at write time; collisions across OPEN issues surface as
    /// [`crate::Error::SlugCollision`].
    pub slug: Option<String>,
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

/// One persistent memory: a short declarative fact keyed by a
/// kebab-case slug, kept on the `issues` bookmark in
/// `memories/<key>.json`. Spec v2.2 §10.
///
/// Memories travel with the bookmark just like issue records do, so
/// every operator who pulls inherits them automatically. Field
/// declaration order doubles as on-disk emission order, matching
/// `IssueRecord`'s schema-stability rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    pub key: String,
    pub value: String,
    pub created_at: String,
    pub updated_at: String,
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
    pub slug: Option<String>,
    pub body: String,
    pub status: Status,
    #[serde(rename = "type")]
    pub type_: IssueType,
    pub labels: Vec<String>,
    pub dependencies: Vec<IssueId>,
    pub assignee: Option<String>,
    pub comments: Vec<Comment>,
    pub created_at: String,
    pub updated_at: String,
}
