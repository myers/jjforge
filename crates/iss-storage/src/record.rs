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

    /// Owner-perspective human label for the text-mode `iss show`
    /// renderer (fix for `show-deps-blocked-by`, fj#2). The wire
    /// spelling at [`DepKind::as_str`] reads inverted when used to
    /// label edges in `iss show <A>`: `blocks: B` reads as "A blocks
    /// B" but the storage semantics ("A is blocked until B closes")
    /// say the opposite. This label flips the perspective so the
    /// printed line scans correctly to a human.
    ///
    /// IMPORTANT: this is text-renderer-only. The wire spelling
    /// ([`DepKind::as_str`]) is still the canonical form for
    /// trailers, CLI `--kind` flags, JSON output, and `dep tree`.
    pub fn as_show_label(self) -> &'static str {
        match self {
            DepKind::Blocks => "blocked by",
            DepKind::ParentChild => "parent",
            DepKind::Related => "related",
            DepKind::DiscoveredFrom => "discovered from",
        }
    }
}

#[cfg(test)]
mod dep_kind_tests {
    use super::*;

    #[test]
    fn as_str_returns_wire_spelling() {
        // Wire spelling is part of the on-disk contract — covered
        // here so any accidental edit shows up in this crate's tests.
        assert_eq!(DepKind::Blocks.as_str(), "blocks");
        assert_eq!(DepKind::ParentChild.as_str(), "parent-child");
        assert_eq!(DepKind::Related.as_str(), "related");
        assert_eq!(DepKind::DiscoveredFrom.as_str(), "discovered-from");
    }

    #[test]
    fn as_show_label_returns_owner_perspective_label() {
        // The `iss show <id>` text renderer uses these; the labels
        // describe the owner's relationship to the target.
        assert_eq!(DepKind::Blocks.as_show_label(), "blocked by");
        assert_eq!(DepKind::ParentChild.as_show_label(), "parent");
        assert_eq!(DepKind::Related.as_show_label(), "related");
        assert_eq!(DepKind::DiscoveredFrom.as_show_label(), "discovered from");
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

/// Issue status. Spec §3 — `open`, `blocked`, `in-progress`,
/// `closed`, `abandoned`. v2.3 added [`Status::InProgress`]
/// (spelled `in-progress` on the wire) as the "claimed by some
/// agent" state. v2.5 added [`Status::Blocked`] (spelled
/// `blocked` on the wire) as the "parked on an external signal"
/// state — waiting on a PR, a timer, a human response. v2.7
/// added [`Status::Abandoned`] (`abandon-verb`) as the
/// "mis-filed — soft-deleted" state: the issue stays in
/// history (audit-trail friendly), the slug stays claimed
/// (per spec §3.4 all-statuses uniqueness), but the ticket
/// is excluded from `iss ls` (default) and `iss ready`
/// (unconditionally). `Open`, `Blocked`, and `InProgress`
/// are all "active" in the sense that the issue isn't
/// terminal; `Closed` and `Abandoned` are both terminal —
/// they don't count as work in flight for ready / dep
/// computation. `iss ready` excludes `Blocked` and
/// `InProgress` by default (overridable via flags), and
/// excludes `Abandoned` unconditionally (no override —
/// abandoning means "never come up again"). Variant
/// declaration order matches the natural lifecycle: open →
/// blocked → in-progress → closed → abandoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Open,
    /// Parked on an external signal (PR landing, timer, human).
    /// Wire spelling: `blocked`. v2.5 (`agent-await-gates-impl`).
    /// The accompanying `block_reason` field on
    /// [`IssueRecord`] carries the free-text rationale.
    Blocked,
    /// Claimed by an agent / operator. Wire spelling: `in-progress`
    /// (hyphenated). v2.3 (`agent-claim-atomic`).
    #[serde(rename = "in-progress")]
    InProgress,
    Closed,
    /// Soft-deleted: mis-filed and intentionally hidden from
    /// `iss ls` (default) and `iss ready` (unconditional).
    /// Wire spelling: `abandoned`. v2.7 (`abandon-verb`,
    /// issue `c1ffea7`). Slug stays claimed (spec §3.4
    /// all-statuses uniqueness). Dep targets in this state
    /// behave like `Closed` for blocked-set computation — an
    /// abandoned target neither blocks dependents nor cascades.
    Abandoned,
}

impl Status {
    /// The lowercase wire spelling used in trailer payloads
    /// (`Jjf-Status:`), `jjf` plain-text output, and CLI `--status`
    /// flag values. `InProgress` renders as `in-progress`
    /// (hyphenated), matching the serde rename.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Blocked => "blocked",
            Status::InProgress => "in-progress",
            Status::Closed => "closed",
            Status::Abandoned => "abandoned",
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

/// Why a priority value failed validation (spec v2.8).
///
/// Today the only failure mode is "out of range" — the integer
/// landed outside the documented `0..=4` window. The enum is left
/// open-ended so future rules (e.g. a per-status floor) can extend
/// it without changing the validator signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityInvalidReason {
    /// Priority integer was outside the documented `0..=4` window.
    /// `got` carries the rejected value verbatim so a CLI envelope
    /// can echo it back to the operator.
    OutOfRange { got: u8 },
}

impl PriorityInvalidReason {
    /// Stable lowercase snake_case name. Used by the CLI to surface
    /// the rejection reason in the JSON error envelope's
    /// `details.reason` slot.
    pub fn as_str(self) -> &'static str {
        match self {
            PriorityInvalidReason::OutOfRange { .. } => "out_of_range",
        }
    }
}

impl std::fmt::Display for PriorityInvalidReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PriorityInvalidReason::OutOfRange { got } => {
                write!(f, "priority must be in 0..=4 (got {got})")
            }
        }
    }
}

/// Validate a priority value per spec v2.8: `None` always passes
/// (unspecified is allowed); `Some(n)` requires `n <= 4`. Returns
/// `Ok(())` on pass; otherwise the typed rejection variant.
///
/// Exposed publicly so the CLI can pre-validate before calling
/// `Storage::create_issue` / `Storage::update` / `Storage::set_priority`
/// and surface a typed `invalid_priority` exit-2 error before any
/// IO kicks off.
pub fn validate_priority(p: Option<u8>) -> std::result::Result<(), PriorityInvalidReason> {
    if let Some(n) = p {
        if n > 4 {
            return Err(PriorityInvalidReason::OutOfRange { got: n });
        }
    }
    Ok(())
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
///
/// **v2.5 (`agent-await-gates-impl`):** new `block_reason` field
/// sits immediately after `status`. Carries the free-text reason
/// an issue was set to [`Status::Blocked`]; `None` (the default,
/// emitted as `null`) for every other status. The reason is a
/// scalar under the op-space resolver — LWW by `Jjf-At:` just
/// like title / body / assignee. Serde-default on read so any
/// pre-v2.5 record (which lacks the field) deserializes cleanly.
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
    /// Free-text reason for the current [`Status::Blocked`] state.
    /// `None` (serialized as `null`) when the issue isn't blocked or
    /// the operator declined to record a reason. v2.5
    /// (`agent-await-gates-impl`). Per spec §3.1 the field always
    /// appears in the record; serde-default on read so pre-v2.5
    /// records (which lack the field) deserialize cleanly.
    #[serde(default)]
    pub block_reason: Option<String>,
    /// Coarse classifier. Default `Unspecified`; the field always
    /// appears in the record (no `#[serde(skip)]`).
    #[serde(default, rename = "type")]
    pub type_: IssueType,
    /// Priority bucket (v2.8 `priority-field`). Integer `0..=4`
    /// with `None` (serialized as `null`) meaning unspecified.
    /// Lower = higher priority (P0 ship-stopping, P4 whenever).
    /// Per spec §3.1 the field always appears in the record (no
    /// `#[serde(skip)]`); serde-default on read so pre-v2.8
    /// records (which lack the field) deserialize cleanly as
    /// `priority: None`. The value-space invariant (`0..=4`) is
    /// enforced at the write boundary by
    /// [`validate_priority`]; the read path trusts the on-disk
    /// integer (a hand-edited out-of-range value would surface
    /// here unchanged, the same way an unknown `IssueType`
    /// variant does).
    #[serde(default)]
    pub priority: Option<u8>,
    pub labels: Vec<String>,
    /// Arbitrary string→string metadata map (e.g. `gc.*` orchestration
    /// keys used by external work-source consumers). Mirrors `labels`
    /// but key/value; set via `set-metadata` ops with last-write-wins
    /// per key. serde-default so pre-metadata records read as an empty
    /// map. Emitted after `labels` (BTreeMap → sorted keys in JSON).
    #[serde(default)]
    // TODO(41c2e4a): when docs/storage-format.md is revived, add this field to §3.
    pub metadata: std::collections::BTreeMap<String, String>,
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
    /// bare-id input from `iss new -d <id>` defaults to
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
    /// Priority bucket (v2.8). `None` writes a record with
    /// `priority: null`; `Some(n)` (n in 0..=4) emits a
    /// `Jjf-Op: set-priority` stanza in the create-time multi-op
    /// commit. Out-of-range integers are rejected at the boundary
    /// by `validate_priority` before write.
    pub priority: Option<u8>,
    /// Seed-time metadata. Each (k, v) emits a `Jjf-Op: set-metadata`
    /// stanza in the create-time multi-op commit, atomically with the
    /// create. Keys and values are validated via `validate_metadata_key`
    /// and `validate_metadata_value` before the op is emitted. Duplicate
    /// keys from `--meta k=v1 --meta k=v2` collapse to "last wins" via
    /// BTreeMap insertion semantics (v2 wins in the example).
    pub metadata: std::collections::BTreeMap<String, String>,
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
/// `iss` CLI, the merge driver once it consumes records) can use
/// without knowing about the underlying file layout.
///
/// Fields mirror `IssueRecord` plus a `comments` vector. `labels` and
/// `dependencies` are sorted (the writer guarantees that already, but
/// the read path re-sorts defensively); `comments` are sorted by
/// `created_at` ascending (chronological).
///
/// The `Serialize` impl is the structured payload `iss show --json`
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
    /// Free-text reason for [`Status::Blocked`]. Mirrors
    /// [`IssueRecord::block_reason`]; serialized between `status`
    /// and `type` to match the on-disk record's emission order.
    /// v2.5 (`agent-await-gates-impl`).
    #[serde(default)]
    pub block_reason: Option<String>,
    #[serde(rename = "type")]
    pub type_: IssueType,
    /// Priority bucket (v2.8). Mirrors [`IssueRecord::priority`];
    /// serialized between `type` and `labels` to match the on-disk
    /// record's emission order. `None` renders as `null`.
    #[serde(default)]
    pub priority: Option<u8>,
    pub labels: Vec<String>,
    /// Arbitrary string→string metadata map. Mirrors
    /// [`IssueRecord::metadata`]; emitted after `labels` on
    /// `iss show --json`. Empty map when the issue has no metadata.
    #[serde(default)]
    pub metadata: std::collections::BTreeMap<String, String>,
    /// Typed dependency edges. Same shape as
    /// [`IssueRecord::dependencies`] but emitted directly on
    /// `iss show --json` (this projection IS the JSON envelope).
    #[serde(with = "dep_edges_serde")]
    pub dependencies: Vec<DepEdge>,
    pub assignee: Option<String>,
    pub comments: Vec<Comment>,
    pub created_at: String,
    pub updated_at: String,
}
