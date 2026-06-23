//! Per-issue record schema for `issues/<id>.json` (spec §3) and comment
//! schema for `issues/<id>.comments.jsonl` (spec §4).
//!
//! Field ORDER on the JSON record matters: spec §3.3 requires the
//! writer to emit fields in the schema order so jj's textual
//! auto-merger gets stable line-by-line diffs. We get this for free
//! with serde's derive because struct fields serialize in declaration
//! order.

use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::ser::{SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};

use crate::id::IssueId;

/// Coarse classifier on a dependency edge (spec v2.4 §3.x, added in
/// `agent-dep-types`). Each edge between two issues carries one of
/// these kinds. The meaning of each:
///
/// - [`DepKind::Blocks`]: a hard prerequisite. The owning issue is
///   blocked until the target is closed. The v1 default — every
///   pre-v2.4 `dependencies: [<id>, ...]` entry reads as this kind.
/// - [`DepKind::ParentChild`]: hierarchical. The owning issue is
///   declared a CHILD of the target. Drives the parent-child cascade
///   in [`crate::Storage::list_ready`] — a child is blocked iff the
///   parent itself is blocked (the cascade follows blocked-ness, not
///   open-vs-closed status).
/// - [`DepKind::Related`]: soft cross-link. Reference only; never
///   contributes to ready computation.
/// - [`DepKind::DiscoveredFrom`]: "I was found while working on X."
///   Provenance only; never contributes to ready computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DepKind {
    /// Hard prereq — owner blocked until target closes.
    Blocks,
    /// Owner is child of target — cascade via parent's blocked-ness.
    ParentChild,
    /// Soft cross-link — no ready effect.
    Related,
    /// Provenance — no ready effect.
    DiscoveredFrom,
}

impl Default for DepKind {
    fn default() -> Self {
        DepKind::Blocks
    }
}

impl DepKind {
    /// Lowercase kebab-case wire spelling. Mirrors the trailer payload
    /// for `add-dep-edge` / `remove-dep-edge` ops and the CLI's `--kind`
    /// argument values.
    pub fn as_str(self) -> &'static str {
        match self {
            DepKind::Blocks => "blocks",
            DepKind::ParentChild => "parent-child",
            DepKind::Related => "related",
            DepKind::DiscoveredFrom => "discovered-from",
        }
    }

    /// Parse the kebab-case wire spelling. Returns `None` for any
    /// other value; the trailer parser uses this for `Jjf-Dep-Kind:`
    /// values; the CLI's clap `ValueEnum` provides its own mapping.
    pub fn parse_wire(s: &str) -> Option<DepKind> {
        match s {
            "blocks" => Some(DepKind::Blocks),
            "parent-child" => Some(DepKind::ParentChild),
            "related" => Some(DepKind::Related),
            "discovered-from" => Some(DepKind::DiscoveredFrom),
            _ => None,
        }
    }
}

/// One typed edge in an issue's `dependencies` field (spec v2.4 §3.x,
/// added in `agent-dep-types`). The owning issue points at `target`
/// with the semantic relation `kind`. See [`DepKind`] for what each
/// kind means.
///
/// Wire shape: `{"target": "abc1234", "kind": "blocks"}`. The v1 shape
/// (a bare 7-hex string) deserializes as `DepEdge { target, kind:
/// Blocks }` via the custom deserializer on [`IssueRecord`]'s
/// `dependencies` field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DepEdge {
    pub target: IssueId,
    #[serde(default)]
    pub kind: DepKind,
}

impl DepEdge {
    /// Construct a typed edge.
    pub fn new(target: IssueId, kind: DepKind) -> Self {
        DepEdge { target, kind }
    }

    /// Convenience: an old-style "blocks" edge for the v1 default.
    pub fn blocks(target: IssueId) -> Self {
        DepEdge {
            target,
            kind: DepKind::Blocks,
        }
    }
}

/// Custom serde adapter for the `dependencies` field. On read, accepts
/// BOTH the v1 shape (`["abc1234", "def5678"]` — bare strings) and the
/// v2.4 shape (`[{"target": "abc1234", "kind": "blocks"}, ...]` —
/// tagged objects). v1 entries materialize as
/// `DepEdge { kind: Blocks }` for backward compat. On write, always
/// emits the v2.4 shape.
pub(crate) mod dep_edges_serde {
    use super::*;

    pub(crate) fn serialize<S>(deps: &[DepEdge], ser: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = ser.serialize_seq(Some(deps.len()))?;
        for d in deps {
            seq.serialize_element(d)?;
        }
        seq.end()
    }

    pub(crate) fn deserialize<'de, D>(de: D) -> std::result::Result<Vec<DepEdge>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct DepEdgesVisitor;

        impl<'de> Visitor<'de> for DepEdgesVisitor {
            type Value = Vec<DepEdge>;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(
                    "an array of issue ids (v1) or tagged DepEdge objects (v2.4)",
                )
            }

            fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Vec<DepEdge>, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut out: Vec<DepEdge> = Vec::new();
                while let Some(item) = seq.next_element::<serde_json::Value>()? {
                    let edge = match item {
                        // v1: bare string id, no kind tag.
                        serde_json::Value::String(s) => {
                            let target = IssueId::parse(&s).map_err(|e| {
                                de::Error::custom(format!(
                                    "v1 dependency id parse error: {e}"
                                ))
                            })?;
                            DepEdge {
                                target,
                                kind: DepKind::Blocks,
                            }
                        }
                        // v2.4: tagged object. Deserialize via the
                        // derived path.
                        other => serde_json::from_value::<DepEdge>(other)
                            .map_err(de::Error::custom)?,
                    };
                    out.push(edge);
                }
                Ok(out)
            }
        }

        de.deserialize_seq(DepEdgesVisitor)
    }
}

/// Issue status. Spec §3 — `open`, `in-progress`, `closed`. v2.3
/// added [`Status::InProgress`] (spelled `in-progress` on the wire)
/// as the "claimed by some agent" state between [`Status::Open`]
/// (idle, available) and [`Status::Closed`] (terminal). `Open` and
/// `InProgress` are both "active"; `Closed` is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Open,
    /// Claimed by an agent / operator. Wire spelling: `in-progress`
    /// (hyphenated). v2.3 (`agent-claim-atomic`).
    #[serde(rename = "in-progress")]
    InProgress,
    Closed,
}

impl Status {
    /// The lowercase wire spelling used in trailer payloads
    /// (`Jjf-Status:`), `jjf` plain-text output, and CLI `--status`
    /// flag values. `InProgress` renders as `in-progress`
    /// (hyphenated), matching the serde rename.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::InProgress => "in-progress",
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
    /// Typed dependency edges (spec v2.4). Each edge carries a target
    /// id and a [`DepKind`]. Backward-compat: a v1 record (no kind
    /// tag, bare `[<id>, ...]` array) reads as a list of
    /// `DepEdge { kind: Blocks }` via the custom deserializer
    /// [`dep_edges_serde`].
    #[serde(with = "dep_edges_serde")]
    pub dependencies: Vec<DepEdge>,
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
    /// Typed dependency edges to seed at create time. v2.4 — a
    /// bare-id input from `jjf new -d <id>` defaults to
    /// [`DepKind::Blocks`] at the CLI layer before reaching here.
    pub dependencies: Vec<DepEdge>,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub id: IssueId,
    pub title: String,
    pub slug: Option<String>,
    pub body: String,
    pub status: Status,
    #[serde(rename = "type")]
    pub type_: IssueType,
    pub labels: Vec<String>,
    /// Typed dependency edges. Same shape as
    /// [`IssueRecord::dependencies`] but emitted directly on
    /// `jjf show --json` (this projection IS the JSON envelope).
    #[serde(with = "dep_edges_serde")]
    pub dependencies: Vec<DepEdge>,
    pub assignee: Option<String>,
    pub comments: Vec<Comment>,
    pub created_at: String,
    pub updated_at: String,
}
