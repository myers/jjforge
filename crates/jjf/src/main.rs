//! `jjf` — the jjforge CLI binary.
//!
//! This crate is the user-facing entry point for jjforge: a thin
//! clap-derive harness over the typed APIs in `jjf-storage`. Each
//! sub-verb (`new`, `show`, `ls`, `update`, `comment`, `close`,
//! `open`, `label`, `init`) maps to one storage call (or, for stubs,
//! a `not yet implemented` placeholder so the parser surface is
//! visible from day one).
//!
//! # Exit-code convention
//!
//! Every verb honors the same exit codes — later verbs MUST follow
//! the same rule:
//!
//! - `0` — success.
//! - `1` — runtime failure (storage error, IO error, anything that's
//!   "we tried, it didn't work").
//! - `2` — argument or preflight failure (bad flags, missing input,
//!   "this isn't a jj repo"). Surfaces with a clear stderr line so a
//!   shell pipeline can react to it without parsing stdout.
//!
//! `--json` is a global flag accepted by every verb. For verbs that
//! haven't been implemented yet, the flag is parsed but ignored
//! (they error out the same way regardless). For `init`, the JSON
//! output is `{"ok": true, "bookmark": "issues"}` per the
//! `cli-skeleton` ticket.
//!
//! # What lives here vs. `jjf-storage`
//!
//! All the actual work — the git-ref write path, the trailers,
//! the merge policy — lives in `jjf-storage` (and, for
//! conflict-resolution, `jjf-merge`). This crate's only jobs
//! are: parse args, hand the parsed shape to storage, render the
//! result, map errors to exit codes. No business logic.

mod preflight;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use jjf_storage::{
    DEFAULT_SNIPPET_CONTEXT, ISSUES_BOOKMARK, ClaimResult, DepEdge, DepKind, DepTreeNode,
    Error as StorageError, IdError, BodyInvalidReason, Issue, IssueDraft, IssueId, IssueType,
    Memory, PriorityInvalidReason, ReadyFilter, SearchHit, SlugInvalidReason, StaleHit, Status,
    Storage, TitleInvalidReason,
    UnreadableRef, UpdateFields, validate_priority,
};

/// Top-level CLI shape. Subcommands live on the `Commands` enum; the
/// `--json` flag is global so every verb sees it without restating
/// the option on each subcommand.
#[derive(Debug, Parser)]
#[command(
    name = "jjf",
    version,
    about = "jjforge — a jj-native, agent-first issue tracker",
    long_about = None,
)]
struct Cli {
    /// Emit machine-readable JSON instead of human-readable text.
    /// Honored by every verb (or will be once each verb lands).
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

/// What `jjf ls --status <X>` accepts. Distinct from `Status` because
/// `all` (no filter) is a CLI-only affordance with no storage-layer
/// equivalent. v2.3 added `in-progress` mirroring `Status::InProgress`;
/// v2.5 added `blocked` mirroring `Status::Blocked`. v2.7 added
/// `abandoned` mirroring `Status::Abandoned` (`abandon-verb`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum StatusFilter {
    Open,
    Blocked,
    #[value(name = "in-progress")]
    InProgress,
    Closed,
    Abandoned,
    All,
}

/// Clap-side mirror of [`jjf_storage::Status`] used for the `--status`
/// flag on `jjf update`. We declare it here (rather than deriving
/// `ValueEnum` directly on `Status` in the storage crate) so the
/// storage crate doesn't pick up a `clap` dependency just for a
/// derive — the binary is the only `ValueEnum` site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum StatusArg {
    Open,
    Blocked,
    #[value(name = "in-progress")]
    InProgress,
    Closed,
    Abandoned,
}

impl From<StatusArg> for Status {
    fn from(s: StatusArg) -> Self {
        match s {
            StatusArg::Open => Status::Open,
            StatusArg::Blocked => Status::Blocked,
            StatusArg::InProgress => Status::InProgress,
            StatusArg::Closed => Status::Closed,
            StatusArg::Abandoned => Status::Abandoned,
        }
    }
}

/// Clap-side mirror of [`jjf_storage::IssueType`] (less the
/// `Unspecified` variant — the operator picks one of the named types
/// with `--type`, and omitting the flag leaves the field at its
/// `Unspecified` default). Same crate-isolation rationale as
/// `StatusArg`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum TypeArg {
    Bug,
    Feature,
    Epic,
    Research,
    Roadmap,
}

impl From<TypeArg> for IssueType {
    fn from(t: TypeArg) -> Self {
        match t {
            TypeArg::Bug => IssueType::Bug,
            TypeArg::Feature => IssueType::Feature,
            TypeArg::Epic => IssueType::Epic,
            TypeArg::Research => IssueType::Research,
            TypeArg::Roadmap => IssueType::Roadmap,
        }
    }
}

/// Clap-side mirror of [`jjf_storage::DepKind`] for the `--kind` flag
/// on `jjf dep add|rm`. Same crate-isolation rationale as `StatusArg`
/// / `TypeArg`. Wire spelling matches the storage layer's kebab-case
/// (`blocks`, `parent-child`, `related`, `discovered-from`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum DepKindArg {
    Blocks,
    #[value(name = "parent-child")]
    ParentChild,
    Related,
    #[value(name = "discovered-from")]
    DiscoveredFrom,
}

impl From<DepKindArg> for DepKind {
    fn from(k: DepKindArg) -> Self {
        match k {
            DepKindArg::Blocks => DepKind::Blocks,
            DepKindArg::ParentChild => DepKind::ParentChild,
            DepKindArg::Related => DepKind::Related,
            DepKindArg::DiscoveredFrom => DepKind::DiscoveredFrom,
        }
    }
}

/// Parse one `-d <spec>` value from the CLI. Accepts:
///
/// - A bare 7-char hex id (`abc1234`): interpreted as
///   `blocks:abc1234` for v1 / pre-v2.4 muscle memory.
/// - `<kind>:<id>` for explicit kinds, where `<kind>` is one of
///   `blocks`, `parent-child`, `related`, `discovered-from`.
///
/// On parse error returns the appropriate `CliError` variant — bad
/// kind or bad id surface as `BadDepId` / `BadDepKind`.
fn parse_dep_spec(raw: String) -> Result<DepEdge, CliError> {
    if let Some((kind_str, id_str)) = raw.split_once(':') {
        let kind = DepKind::parse_wire(kind_str).ok_or_else(|| CliError::BadDepKind {
            value: raw.clone(),
            kind: kind_str.to_owned(),
        })?;
        let target = IssueId::parse(id_str).map_err(|error| CliError::BadDepId {
            value: raw.clone(),
            error,
        })?;
        Ok(DepEdge { target, kind })
    } else {
        let target = IssueId::parse(&raw).map_err(|error| CliError::BadDepId {
            value: raw.clone(),
            error,
        })?;
        Ok(DepEdge {
            target,
            kind: DepKind::Blocks,
        })
    }
}

/// Every verb the epic body (`c4f7fcb`) calls out, plus `init`. Stubs
/// exist so `--help` lists the full surface from day one; later
/// per-verb tickets replace the stubs with real implementations.
#[derive(Debug, Subcommand)]
enum Commands {
    /// Initialize the `issues` bookmark on the current jj repo.
    /// Idempotent — running twice in the same repo is a no-op.
    Init,

    /// Create a new issue on the `issues` bookmark. Requires `jjf init`
    /// to have been run first. Prints the new issue's id on stdout
    /// (or the `{"ok": true, "id": "..."}` object under `--json`);
    /// exits 0.
    New {
        /// Title of the new issue. Required, non-empty.
        #[arg(short = 't', long)]
        title: String,

        /// Source for the issue body. Path to read, or `-` to read
        /// stdin. Omit to leave the body empty (the epic's "no
        /// prompts ever" rule — no editor pop-up).
        #[arg(short = 'F', long)]
        file: Option<PathBuf>,

        /// Attach a label. Repeatable (`-l bug -l p1`).
        #[arg(short = 'l', long = "label")]
        labels: Vec<String>,

        /// Declare a dependency on another issue id. Repeatable.
        /// Accepts a bare 7-char lowercase-hex id (interpreted as
        /// `blocks:<id>` for v1 muscle memory) OR a `<kind>:<id>`
        /// spec where `<kind>` is one of
        /// `blocks` / `parent-child` / `related` / `discovered-from`.
        /// A bad id or unknown kind is a preflight failure (exit 2).
        /// For the common "child of an epic" case, see `--parent`.
        #[arg(short = 'd', long = "dep")]
        deps: Vec<String>,

        /// Declare a `parent-child` edge: this issue is a child of
        /// `<id>`. Repeatable. Shorthand for `-d parent-child:<id>`;
        /// composes with `-d` to mix kinds in one create. The common
        /// "file a ticket under an epic" case:
        /// `jjf new -t "..." --parent <epic-id>`.
        #[arg(long = "parent")]
        parents: Vec<String>,

        /// Set the assignee. Optional; omit to leave the field unset
        /// (creates a record with `assignee: null`).
        #[arg(short = 'a', long)]
        assignee: Option<String>,

        /// Set the coarse issue type. Optional; omit to leave the
        /// type at `unspecified`. One of `bug`, `feature`, `epic`,
        /// `research`, `roadmap`.
        #[arg(long, value_enum)]
        r#type: Option<TypeArg>,

        /// Set the kebab-case slug (orientation handle). Optional;
        /// omit to leave the field empty. Validated per spec v2.1
        /// §3.1; collision with an existing OPEN issue's slug is a
        /// preflight failure (exit 2).
        #[arg(long)]
        slug: Option<String>,

        /// Set the priority bucket (0–4; lower = higher priority).
        /// v2.8 (`priority-field`). Optional; omit to leave the
        /// field at `null` (unspecified). Clap rejects values
        /// outside `0..=4` as a preflight failure (exit 2).
        #[arg(short = 'p', long, value_parser = clap::value_parser!(u8).range(0..=4))]
        priority: Option<u8>,

        /// Attach a metadata key=value pair at create time. Repeatable.
        /// Format: `key=value` (first `=` splits key from value; values
        /// may contain `=`). Bare keys with no `=` are rejected at parse
        /// time. Duplicate keys: last wins (`--meta k=v1 --meta k=v2`
        /// seeds `k=v2`).
        #[arg(long = "meta", value_parser = parse_meta_kv)]
        meta: Vec<(String, String)>,
    },

    /// Print a single issue from the `issues` bookmark — title,
    /// status, labels, assignee, body, and comment thread. Plain-text
    /// by default; `--json` emits the structured `Issue` record
    /// verbatim (no envelope — the issue IS the payload). Requires
    /// `jjf init` to have been run first.
    Show {
        /// Issue handle: a full 7-char hex id or a slug. Slug lookup
        /// scans both open and closed issues. A 7-char hex id that
        /// matches no issue surfaces as exit 1 (`issue_not_found`);
        /// a non-hex handle with no matching slug is exit 2
        /// (`slug_not_found`).
        id: String,

        /// Append a `## Persistent Memories (N)` block after the
        /// issue body, listing every memory at the bookmark tip
        /// alphabetically by key. v2.2 — primarily intended for
        /// `jjf show roadmap --include-memories` at session start.
        /// Has no effect on `--json` output (memories are reachable
        /// via `jjf memories --json` for machine consumers).
        #[arg(long = "include-memories")]
        include_memories: bool,
    },

    /// List issues from the `issues` bookmark, with optional filters.
    /// Default: every open issue. Plain-text output is one row per
    /// issue, tab-separated columns
    /// (`<id-7>\t<status>\t<labels>L\t<title>`), no header, sorted
    /// newest-first by `created_at`. `--json` emits a JSON array of
    /// `Issue` records (the same shape `show --json` emits per
    /// element). Empty result is exit 0 with no output.
    Ls {
        /// Filter by status. `open` is the default (the "lists are
        /// about what's actionable" convention). `all` shows every
        /// issue regardless of status.
        #[arg(long, value_enum, default_value_t = StatusFilter::Open)]
        status: StatusFilter,

        /// Filter by label. Repeatable. Semantics: AND — an issue
        /// must carry every listed label to match.
        #[arg(short = 'l', long = "label")]
        labels: Vec<String>,

        /// Filter by metadata key=value. Repeatable. Semantics:
        /// AND — an issue must carry every listed key with the exact
        /// value to match. Format is `key=value`; the first `=`
        /// splits key from value (values may contain `=`). Bare keys
        /// (no `=`) are rejected at parse time.
        #[arg(long = "meta", value_parser = parse_meta_kv)]
        meta: Vec<(String, String)>,

        /// Filter by issue type. Repeatable. Semantics: OR — an
        /// issue matches if its type equals any of the listed
        /// types. Omit the flag to include every type.
        #[arg(long = "type", value_enum)]
        types: Vec<TypeArg>,

        /// Filter by slug substring (case-sensitive). An issue
        /// matches if its `slug` field contains the pattern.
        /// Issues with no slug never match.
        #[arg(long)]
        slug: Option<String>,

        /// Filter by priority bucket. Repeatable. Semantics: OR —
        /// an issue matches if its priority equals any of the
        /// listed values. Issues with `null` priority never match
        /// any explicit value. v2.8 (`priority-field`).
        #[arg(short = 'p', long = "priority", value_parser = clap::value_parser!(u8).range(0..=4))]
        priorities: Vec<u8>,

        /// Filter to issues carrying a `parent-child` dep edge to
        /// `<handle>`. `<handle>` is an issue id (7-char hex) or
        /// slug. AND-composed with `--label` / `--type` /
        /// `--status` / `--slug`. Unknown handle exits 2.
        #[arg(long)]
        parent: Option<String>,
    },

    /// List the unblocked open issues — the agent-ready set.
    ///
    /// Returns every OPEN issue whose every dependency is closed
    /// (open deps block; closed and dangling deps don't), filtered
    /// by optional `--label` (AND) and `--type` (OR) flags,
    /// sorted by type priority (bug > feature > research > epic >
    /// unspecified — roadmap excluded entirely) with `created_at`
    /// ascending as the tiebreaker. `--limit N` truncates after
    /// sorting.
    ///
    /// Plain-text output is the same tab-separated row shape as
    /// `ls` (`<id>\t<status>\t<labelN>L\t<title>`); `--json` emits
    /// an array of `Issue` records.
    ///
    /// The headline agent-ergonomics primitive: `jjf ready --limit 1
    /// --json` returns one unblocked issue to feed into the next
    /// action of an automation loop.
    Ready {
        /// Filter by label. Repeatable. Semantics: AND — an issue
        /// must carry every listed label to match. Mirrors
        /// `jjf ls --label`.
        #[arg(short = 'l', long = "label")]
        labels: Vec<String>,

        /// Filter by issue type. Repeatable. Semantics: OR — an
        /// issue matches if its type equals any of the listed
        /// types. Omit the flag to include every type. Mirrors
        /// `jjf ls --type`. Note: `Roadmap`-typed issues are
        /// excluded from the ready set entirely (the roadmap
        /// ticket isn't work to do), regardless of this filter.
        #[arg(long = "type", value_enum)]
        types: Vec<TypeArg>,

        /// Truncate the result to the first N entries after the
        /// priority sort. Omit for unlimited. The canonical
        /// agent-loop call is `jjf ready --limit 1 --json`.
        #[arg(long)]
        limit: Option<usize>,

        /// Include `in-progress` (claimed) issues in the ready
        /// set. Off by default so an idle agent doesn't see
        /// another agent's claimed work as available. Useful for
        /// "what's in flight" views. v2.3 (`agent-claim-atomic`).
        #[arg(long = "include-claimed")]
        include_claimed: bool,

        /// Include `blocked` (parked) issues in the ready set.
        /// Off by default so an idle agent doesn't see an issue
        /// that's parked on an external signal. Useful for
        /// "what's parked" views. v2.5 (`agent-await-gates-impl`).
        #[arg(long = "include-blocked")]
        include_blocked: bool,

        /// Atomically claim the top result and emit its id. Only
        /// makes sense with `--limit 1` (claiming multiple at once
        /// would be ambiguous); other values are rejected at exit 2.
        /// Equivalent to `jjf ready --limit 1` followed by
        /// `jjf update <id> --claim`, but as one atomic compound:
        /// the same `jj` rejection that blocks two parallel claims
        /// of the same id rolls this call back too. v2.3
        /// (`agent-claim-atomic`).
        #[arg(long = "claim")]
        claim: bool,

        /// Filter by priority bucket. Repeatable. Semantics: OR —
        /// an issue matches if its priority equals any of the
        /// listed values. Issues with `null` priority never match
        /// any explicit value. v2.8 (`priority-field`). Composes
        /// AND with `--label` / `--type`; the sort order (priority
        /// first, then type, then created_at) is independent of
        /// the filter.
        #[arg(short = 'p', long = "priority", value_parser = clap::value_parser!(u8).range(0..=4))]
        priorities: Vec<u8>,

        /// Filter to issues carrying a `parent-child` dep edge to
        /// `<handle>`. `<handle>` is an issue id (7-char hex) or
        /// slug. AND-composed with `--label` / `--type`. Unknown
        /// handle exits 2 (`slug_not_found`).
        #[arg(long)]
        parent: Option<String>,

        /// Filter by metadata key=value pair. Repeatable; AND-
        /// composed — every listed pair must match exactly on the
        /// issue's metadata map. Useful for GC-routing queries
        /// such as `jjf ready --meta gc.routed_to=worker-1`.
        /// v2.12 (`issue-metadata`).
        #[arg(long = "meta", value_parser = parse_meta_kv)]
        meta: Vec<(String, String)>,
    },

    /// Mutate one or more scalar fields of an issue in a single commit.
    ///
    /// Every populated field flag lands as a `Jjf-Op:` trailer on ONE
    /// new commit on the `issues` bookmark (spec §5.5
    /// multi-op-per-commit). So `update <id> --title T --status closed
    /// --body-file -` ships three trailers (`set-title`,
    /// `set-status`, `set-body`) on one commit — distinct from
    /// running three sibling verbs back-to-back, which would fragment
    /// into three commits.
    ///
    /// At least one of `--title` / `--status` / `--body-file` /
    /// `--assignee` / `--unset-assignee` is required; running with
    /// none is an exit-2 preflight failure (clap can't enforce the
    /// at-least-one rule for us). `--assignee` and `--unset-assignee`
    /// are mutually exclusive (clap `conflicts_with`).
    ///
    /// `--status` overlaps with `jjf close` / `jjf open` by design —
    /// use the standalone verbs for the single-shot ergonomic path,
    /// this verb for the multi-field case.
    Update {
        /// Issue handle (7-char hex id OR a slug). Resolved via
        /// `Storage::resolve` — a bad id-or-slug surfaces as exit 2,
        /// a valid one that doesn't exist on the bookmark is exit 1.
        id: String,

        /// Replace the title. Must be non-empty (after trim) at the
        /// storage layer.
        #[arg(long)]
        title: Option<String>,

        /// Replace the status. Use `open` or `closed`.
        #[arg(long, value_enum)]
        status: Option<StatusArg>,

        /// Replace the issue type. One of `bug`, `feature`, `epic`,
        /// `research`, `roadmap`.
        #[arg(long = "type", value_enum)]
        r#type: Option<TypeArg>,

        /// Replace the slug. Validated per spec v2.1 §3.1; collision
        /// with another open issue is exit 2. Mutually exclusive
        /// with `--unset-slug`.
        #[arg(long, conflicts_with = "unset_slug")]
        slug: Option<String>,

        /// Clear the slug (writes `null`). Mutually exclusive with
        /// `--slug`.
        #[arg(long = "unset-slug")]
        unset_slug: bool,

        /// Replace the priority bucket (0–4; lower = higher priority).
        /// Mutually exclusive with `--unset-priority`. v2.8
        /// (`priority-field`).
        #[arg(short = 'p', long, value_parser = clap::value_parser!(u8).range(0..=4), conflicts_with = "unset_priority")]
        priority: Option<u8>,

        /// Clear the priority (writes `null`). Mutually exclusive
        /// with `--priority`. v2.8 (`priority-field`).
        #[arg(long = "unset-priority")]
        unset_priority: bool,

        /// Replace the body. Source is a path, or `-` to read stdin.
        /// Mirrors the `cli-new` / `cli-comment` body-source convention;
        /// there is no inline `--body <STRING>` flag in v1.
        #[arg(long = "body-file", value_name = "PATH")]
        body_file: Option<PathBuf>,

        /// Set the assignee. Mutually exclusive with `--unset-assignee`.
        #[arg(long, conflicts_with = "unset_assignee")]
        assignee: Option<String>,

        /// Clear the assignee (writes `null`). Mutually exclusive with
        /// `--assignee`.
        #[arg(long = "unset-assignee")]
        unset_assignee: bool,

        /// Atomically claim the issue: set assignee = current jj
        /// `user.name` AND set status = `in-progress` in one
        /// multi-op commit. Two parallel `--claim` calls on the
        /// same id are race-free — bookmark ordering decides the
        /// winner, the loser sees a `Jj` error and re-reads ready.
        /// Mutually exclusive with `--unclaim`, `--assignee`,
        /// `--unset-assignee`, `--status`. v2.3
        /// (`agent-claim-atomic`).
        #[arg(
            long,
            conflicts_with_all = [
                "unclaim",
                "assignee",
                "unset_assignee",
                "status",
            ],
        )]
        claim: bool,

        /// Atomically unclaim the issue: clear the assignee AND
        /// set status back to `open` in one multi-op commit.
        /// Inverse of `--claim`. Mutually exclusive with
        /// `--claim`, `--assignee`, `--unset-assignee`, `--status`.
        /// v2.3 (`agent-claim-atomic`).
        #[arg(
            long,
            conflicts_with_all = [
                "assignee",
                "unset_assignee",
                "status",
            ],
        )]
        unclaim: bool,

        /// Override the actor used by `--claim` (the identity that
        /// lands in the `assignee` field). Precedence:
        /// `--actor <name>` > `JJF_ACTOR` env > `jj config get
        /// user.name`. Empty string falls through to the next slot
        /// rather than writing an empty assignee. Intended for
        /// multi-agent orchestrators that fan out N processes from
        /// one machine and need each to claim under a distinct
        /// name; everyone else should just set `jj user.name`.
        /// Mutually exclusive with `--unclaim`, `--assignee`, and
        /// `--unset-assignee` (those don't take an implicit
        /// identity). v2.12 (`actor-override-chain`, ticket
        /// `ae0866b`).
        #[arg(
            long,
            conflicts_with_all = [
                "unclaim",
                "assignee",
                "unset_assignee",
            ],
        )]
        actor: Option<String>,
    },

    /// Append a comment to an existing issue on the `issues` bookmark.
    /// Body source is REQUIRED — pass `-F <path>` or `-F -` for stdin.
    /// Author defaults to the jj user identity (`Name <email>` per
    /// jj's `author` template); `--author <NAME>` overrides. Empty
    /// bodies are rejected at the CLI layer (exit 2) because an empty
    /// comment is almost certainly a user mistake.
    Comment {
        /// Full 7-char hex issue id. Bad parse → exit 2; valid id
        /// that doesn't exist on the bookmark → exit 1.
        id: String,

        /// Source for the comment body. Path to read, or `-` to read
        /// stdin. REQUIRED — the epic's "no prompts ever" rule means
        /// we do NOT launch an editor when this is omitted. Empty body
        /// (after read) is a preflight failure (exit 2).
        #[arg(short = 'F', long, required = true)]
        file: PathBuf,

        /// Override the comment author. Free-form string written
        /// verbatim into the comment record. When omitted, the author
        /// is sourced from `jj config get user.name` + `user.email`
        /// in the `Name <email>` format that matches jj's commit-author
        /// template. If no jj `user.name` is configured and no
        /// override is given, the verb exits 2 with a hint to set one.
        #[arg(long)]
        author: Option<String>,
    },

    /// Close an issue. Lands a `set-status` op on a new commit on the
    /// `issues` bookmark. Not idempotent per the spec — closing an
    /// already-closed issue still writes a fresh trailer so the audit
    /// log records the intent. Requires `jjf init` to have been run
    /// first.
    Close {
        /// Full 7-char hex issue id. A bad parse is a preflight
        /// failure (exit 2); a well-formed id that doesn't exist on
        /// the bookmark is a runtime failure (exit 1).
        id: String,
    },

    /// Reopen an issue. Same shape and same non-idempotency rules as
    /// `close`, just lands `set-status=open`.
    Open {
        /// Full 7-char hex issue id. A bad parse is a preflight
        /// failure (exit 2); a well-formed id that doesn't exist on
        /// the bookmark is a runtime failure (exit 1).
        id: String,
    },

    /// Abandon an issue: soft-delete via `set-status=abandoned`.
    /// v2.7 (`abandon-verb`, issue `c1ffea7`).
    ///
    /// Abandoned issues stay in history (audit-trail friendly), their
    /// slug stays claimed (spec §3.4 — slug uniqueness spans every
    /// status), but they're hidden from `jjf ls` by default
    /// (`--status all` or `--status abandoned` to see them) and
    /// excluded from `jjf ready` unconditionally (no override flag,
    /// unlike `--include-blocked` / `--include-claimed`).
    ///
    /// Use this for mis-filed issues (typo, wrong type, test
    /// ticket) instead of `close` — `close` keeps the issue
    /// cluttering `--status all` and the ready set still sees it
    /// shape-wise (as a closed dep). Abandoning is "never come up
    /// again."
    ///
    /// Same shape and non-idempotency rules as `close`: each call
    /// lands a fresh `set-status` trailer so the audit log records
    /// every intent. To revive an abandoned issue, use
    /// `jjf update <id> --status open` (no inverse `unabandon` —
    /// the asymmetry is deliberate; abandon is meant as soft-
    /// delete, not a parking lot).
    Abandon {
        /// Full 7-char hex issue id. A bad parse is a preflight
        /// failure (exit 2); a well-formed id that doesn't exist on
        /// the bookmark is a runtime failure (exit 1).
        id: String,
    },

    /// Set (or clear) an issue's assignee. Thin shorthand for
    /// `jjf update <id> --assignee <name>` and `jjf update <id>
    /// --unset-assignee`. Modeled on beads' `bd assign <id> <name>`
    /// (`reference/beads/cmd/bd/assign.go`).
    ///
    /// `jjf assign <id> <name>` sets the assignee; an empty `name`
    /// (e.g. `jjf assign <id> ""`) clears it. Same preflight
    /// (issues_bookmark probe, handle resolution) and the same
    /// typed errors as `update --assignee`: `issue_not_found` /
    /// `slug_not_found` on unknown handles, `invalid_input` from
    /// storage when the name contains a newline.
    ///
    /// This is sugar only — it doesn't flip status. Use `jjf update
    /// <id> --claim` (or its `--claim` shorthand on `ready`) when
    /// you want assignee + `in-progress` in one atomic commit.
    Assign {
        /// Issue handle (7-char hex id OR a slug).
        id: String,

        /// New assignee name. Pass an empty string (`""`) to clear
        /// the assignee. Newlines are rejected at the storage layer
        /// (`invalid_input`, exit 1) — single-line names only.
        name: String,
    },

    /// Park an issue: set status to `blocked` and record a free-text
    /// reason, in ONE multi-op commit. v2.5 (`agent-await-gates-impl`).
    ///
    /// Blocked issues are excluded from `jjf ready` by default — an
    /// idle agent shouldn't see them as workable. Use this when an
    /// issue is parked on an external signal (a PR landing, a
    /// timer, a human response) that the orchestrator (or a separate
    /// script) is responsible for clearing. The companion verb
    /// `jjf unblock <id>` flips the status back to `open` and clears
    /// the reason.
    ///
    /// Inverse: `jjf unblock <id>`. (`jjf open <id>` also clears the
    /// status but does NOT clear the reason — use `unblock` for the
    /// canonical round-trip.)
    Block {
        /// Issue handle (7-char hex id OR a slug).
        id: String,

        /// Free-text reason recorded on the issue's `block_reason`
        /// field. Single-line; newlines are rejected at exit 2.
        /// Optional, but strongly recommended — without a reason
        /// the operator who finds the issue later has no signal
        /// for why it's parked.
        #[arg(long)]
        reason: Option<String>,
    },

    /// Unpark an issue: clear status back to `open` and clear the
    /// `block_reason` in ONE multi-op commit. Inverse of `jjf block`.
    /// v2.5 (`agent-await-gates-impl`).
    Unblock {
        /// Issue handle (7-char hex id OR a slug).
        id: String,
    },

    /// Add or remove a single label on an issue. Lands a fresh
    /// `label-add` or `label-rm` op on a new commit on the `issues`
    /// bookmark.
    ///
    /// Per the spec (§5.2) and matching `close`/`open`'s twin-mutator
    /// shape: the call is NOT idempotent — re-adding an
    /// already-present label, or removing one that isn't there, still
    /// writes a fresh trailer so the audit log records the intent.
    /// The in-memory label set is dedup'd, so `show` reports a clean
    /// list either way.
    ///
    /// v1 is single-label-per-call. Bulk (`label add <id> a b c`) is
    /// out of scope; repeat the command in a loop for now.
    Label {
        #[command(subcommand)]
        action: LabelAction,
    },

    /// Manage per-issue string→string metadata (last-write-wins per
    /// key). Mirrors `label` but stores a key/value map instead of a
    /// set. Emitted on `jjf show --json` / `jjf ls --json` as a
    /// `"metadata"` object.
    Metadata {
        #[command(subcommand)]
        action: MetadataAction,
    },

    /// Manage typed dependency edges between issues (v2.4
    /// `agent-dep-types`). Four edge kinds with distinct semantics:
    ///
    /// - `blocks`: hard prerequisite. The owning issue is blocked
    ///   until the target closes; `jjf ready` honors this.
    /// - `parent-child`: hierarchical. The owning issue is a CHILD
    ///   of the target; `jjf ready` cascades the parent's blocked
    ///   state to its children via fixpoint.
    /// - `related`: soft cross-link. Reference only.
    /// - `discovered-from`: provenance. "Found while working on X."
    ///
    /// Per-verb help: `jjf dep add|rm|tree --help`.
    Dep {
        #[command(subcommand)]
        action: DepAction,
    },

    /// Manage git remotes on the underlying jj repo. Thin wrapper over
    /// `jj git remote add|list|remove` — jj already supports git
    /// transport for bookmarks (and bookmarks ARE the unit `issues`
    /// travels as), so this verb does NOT need to write per-bookmark
    /// refspec config. Verified in `experiments/sync-remote/`.
    ///
    /// Preflight is jj-repo-only (no `issues` bookmark required) —
    /// adding a remote is meaningful BEFORE `jjf init` runs, and the
    /// soon-to-come `jjf push` will be how the bookmark first reaches
    /// a remote.
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
    },

    /// Store a persistent memory on the `issues` bookmark.
    ///
    /// Memories are short declarative facts (operational rules,
    /// codebase folklore, architectural decisions) that travel with the
    /// planner data via `jjf push` / `jjf pull`. v2.2 spec §10.
    ///
    /// Examples:
    ///
    ///   jjf remember "always run tests with -race flag"
    ///   jjf remember "Dolt phantom DBs hide in three places" --key dolt-phantoms
    ///   jjf remember --key big-note -F notes.md
    ///
    /// When `--key` is omitted, the key is derived from the value via
    /// the slugify rule (first ~8 hyphen-separated tokens, lowercase,
    /// capped at 60 chars). When a memory with the key already exists,
    /// `remember` upserts in place (updates `value` and `updated_at`).
    Remember {
        /// The memory's value. Positional argument; omit when reading
        /// from `-F`. Mutually exclusive with `-F`.
        #[arg(conflicts_with = "file")]
        value: Option<String>,

        /// Explicit key (kebab-case). Optional; when absent, the key
        /// is derived from `value` via [`jjf_storage::slugify`].
        /// Required when `-F -` reads the value from stdin and the
        /// value's slugify would surprise the operator.
        #[arg(long)]
        key: Option<String>,

        /// Source for the memory value when the positional argument is
        /// omitted. Path to read, or `-` to read stdin. Mutually
        /// exclusive with the positional `value`.
        #[arg(short = 'F', long)]
        file: Option<PathBuf>,
    },

    /// List or search persistent memories.
    ///
    /// With no argument, prints every memory. With a positional
    /// `<search>`, filters case-insensitively by substring match
    /// across keys AND values. Plain-text output is `<key>\n  <value
    /// truncated>\n` per memory, alphabetical by key. `--json` emits a
    /// JSON array of `Memory` records.
    Memories {
        /// Substring to filter by (case-insensitive). Matches if the
        /// substring appears in either the key or the value.
        search: Option<String>,
    },

    /// Print the full value of one memory by key.
    ///
    /// Exits 0 with the value on stdout when found, 1 with no output
    /// (or `{"found": false}` under `--json`) when absent. Useful in
    /// scripts: `value=$(jjf recall some-key)`.
    Recall {
        /// Memory key to look up.
        key: String,
    },

    /// Remove a persistent memory by key.
    ///
    /// Exits 0 with a confirmation when found+removed, 1 when the key
    /// doesn't exist. Per spec §5.2-style audit semantics, the
    /// `unset-memory` op lands on the bookmark even though the file
    /// gets deleted.
    Forget {
        /// Memory key to remove.
        key: String,
    },

    /// Push the `issues` bookmark to a git remote. Wraps
    /// `jj git push --bookmark issues --remote <remote>`.
    ///
    /// Preflight: full `issues_bookmark` probe (the bookmark must
    /// exist locally — there's nothing to push otherwise). Unknown
    /// remote surfaces as `remote_not_found` (exit 2); network /
    /// auth / non-fast-forward failures are runtime (exit 1) under
    /// typed kinds so scripts can branch.
    Push {
        /// Remote name (must already be configured via
        /// `jjf remote add <name> <url>`).
        remote: String,
    },

    /// Substring search across issue titles, bodies, and (optionally)
    /// comment bodies. Returns one row per matching issue with a
    /// snippet preview around the first hit.
    ///
    /// Match semantics: case-insensitive substring, NOT regex. The
    /// match priority for `matched_field` is `title > body >
    /// comments` — an issue that hits in multiple fields reports the
    /// most-specific surface (title beats body, body beats comments).
    ///
    /// Plain-text output is one row per match,
    /// `<id>\t<title>\t<matched_field>\t<snippet>`, sorted by `score`
    /// descending (most hits first) then `created_at` ascending
    /// (stable tiebreak). `--json` emits the
    /// `{"ok":true,"results":[...]}` envelope; the empty case is
    /// `{"ok":true,"results":[]}`. Plain text is silent on empty,
    /// mirroring `ls`.
    ///
    /// The empty query (`""`) returns no results — match-everything
    /// is `jjf ls`'s job. Filters (`--status`, `--label`, `--type`)
    /// compose with AND semantics against the search hits.
    Search {
        /// Substring to search for. Case-insensitive. Empty query
        /// returns no results (use `jjf ls` to list every issue).
        query: String,

        /// Filter the search hits by status. Mirrors `jjf ls
        /// --status`. Default `all` — search is fundamentally a
        /// "find anything containing X" verb, so we don't pre-restrict
        /// to open issues the way `ls` does.
        #[arg(long, value_enum, default_value_t = StatusFilter::All)]
        status: StatusFilter,

        /// Filter by label. Repeatable. Semantics: AND — an issue
        /// must carry every listed label to match. Mirrors `jjf ls
        /// --label`.
        #[arg(short = 'l', long = "label")]
        labels: Vec<String>,

        /// Filter by issue type. Repeatable. Semantics: OR — an
        /// issue matches if its type equals any of the listed
        /// types. Mirrors `jjf ls --type`.
        #[arg(long = "type", value_enum)]
        types: Vec<TypeArg>,

        /// Also search comment bodies. Off by default so the common
        /// "what's mentioned in titles/bodies" query stays cheap
        /// and unambiguous; the snapshot cache already materializes
        /// every comment, so this is a per-issue iteration toggle
        /// rather than an extra IO path.
        #[arg(long = "include-comments")]
        include_comments: bool,

        /// Truncate the result to the first N entries after the
        /// score sort. Default 20 (matches the ticket's contract).
        /// Pass `0` for unlimited.
        #[arg(long, default_value_t = 20)]
        limit: usize,

        /// Half-width of the snippet window — `±N` characters around
        /// the first hit on the matched field. Default 40 (per
        /// `DEFAULT_SNIPPET_CONTEXT`).
        #[arg(long = "snippet-context", default_value_t = DEFAULT_SNIPPET_CONTEXT)]
        snippet_context: usize,

        /// Filter to issues carrying a `parent-child` dep edge to
        /// `<handle>`. AND-composed with the search query and
        /// existing `--label` / `--type` / `--status` filters.
        /// Unknown handle exits 2.
        #[arg(long)]
        parent: Option<String>,

        /// Also search metadata values. Off by default so the common
        /// query stays cheap and unambiguous; metadata values are
        /// typically short GC keys, not natural-language text.
        /// Mirrors `--include-comments`. v2.12 (`issue-metadata`).
        #[arg(long = "include-metadata", default_value_t = false)]
        include_metadata: bool,

        /// Filter by metadata key=value pair. Repeatable; AND-composed
        /// — every listed pair must match exactly on the issue's
        /// metadata map. Applied after the substring search (composing
        /// AND with the query). v2.12 (`issue-metadata`).
        #[arg(long = "meta", value_parser = parse_meta_kv)]
        meta: Vec<(String, String)>,
    },

    /// Surface issues not touched in the last N days — orchestrator
    /// hygiene query. Walks the snapshot, compares each issue's
    /// `updated_at` against the configured threshold, returns oldest
    /// first.
    ///
    /// Default `--days 14`; pass `--days N` to widen or narrow the
    /// window. Default `--status open` because the orchestrator
    /// question is "what actionable work has gone quiet?" — pass
    /// `--status all` (or any specific status) to widen.
    ///
    /// Plain-text output is one row per stale issue,
    /// `<id>\t<age>\t<title>\t<status>`, sorted ascending by
    /// `updated_at` (oldest first). `<age>` is a human shape (`Nd`,
    /// `Nw`, `Nmo`) keyed off whole-day deltas — see the per-verb
    /// section of `docs/cli-json.md` for the exact rendering rule.
    /// `--json` emits a bare array of `{id, title, status,
    /// updated_at, days_since_update}` records (no envelope —
    /// structural cousin of `jjf ls --json`, which the ticket
    /// explicitly mirrors). Empty result is `[]` under `--json`,
    /// silence under plain text.
    ///
    /// Filters (`--status`, `--label`, `--type`) compose with AND
    /// semantics against the stale set.
    ///
    /// Caveat carried from the ticket's "Out of scope": comments do
    /// NOT bump `updated_at` today (only mutating verbs do). A
    /// commented-on but otherwise-untouched issue still shows as
    /// stale. That's deliberate and tracked separately in the
    /// storage spec.
    Stale {
        /// Staleness window in whole days. An issue is stale when
        /// `now - updated_at > days * 86400` (strict `>`; an issue
        /// touched exactly at the threshold tick is NOT stale).
        /// Default 14 per the ticket spec.
        #[arg(long, default_value_t = 14)]
        days: u64,

        /// Filter by status. Mirrors `jjf ls --status`. Default
        /// `open` — the orchestrator question is "what actionable
        /// work has gone quiet?".
        #[arg(long, value_enum, default_value_t = StatusFilter::Open)]
        status: StatusFilter,

        /// Filter by label. Repeatable. Semantics: AND — an issue
        /// must carry every listed label to match.
        #[arg(short = 'l', long = "label")]
        labels: Vec<String>,

        /// Filter by issue type. Repeatable. Semantics: OR — an
        /// issue matches if its type equals any of the listed
        /// types.
        #[arg(long = "type", value_enum)]
        types: Vec<TypeArg>,

        /// Truncate the result after the oldest-first sort. Default
        /// 0 (unlimited); mirrors `search`'s convention of
        /// `--limit 0` == no cap. Typical orchestrator use is
        /// `--limit 10` to skim the top of the stale list.
        #[arg(long, default_value_t = 0)]
        limit: usize,

        /// Filter by metadata key=value pair. Repeatable; AND-
        /// composed — every listed pair must match exactly on the
        /// issue's metadata map. Useful for GC-routing queries
        /// such as `jjf stale --meta gc.routed_to=worker-1`.
        /// v2.12 (`issue-metadata`).
        #[arg(long = "meta", value_parser = parse_meta_kv)]
        meta: Vec<(String, String)>,
    },

    /// Pull the `issues` bookmark from a git remote, then merge any
    /// divergence into a single commit via the jjforge merge driver.
    ///
    /// Sequence:
    ///
    /// 1. `jj git fetch --remote <remote>`. Network / auth failures
    ///    bubble up as typed runtime errors (exit 1).
    /// 2. If the remote bookmark `issues@<remote>` exists but the
    ///    local `issues` doesn't yet track it, run
    ///    `jj bookmark track issues --remote=<remote>` so subsequent
    ///    fetches see new commits as bookmark moves rather than as new
    ///    untracked remote bookmarks.
    /// 3. If the bookmark is now in a divergent ("conflicted") state —
    ///    `heads(bookmarks(issues))` resolves to >1 commit — run the
    ///    merge driver: for each conflicted `issues/<id>.json`, call
    ///    `jjf_merge::resolve` and write the result back. Lands a
    ///    single merge commit on `issues` with one `Jjf-Op: merge`
    ///    trailer per resolved issue (spec §5.2 / §5.5).
    /// 4. If the remote has no `issues` bookmark yet (the other side
    ///    hasn't pushed), exit 0 with `remote_present: false` in the
    ///    JSON envelope. Not an error.
    Pull {
        /// Remote name (must already be configured via
        /// `jjf remote add <name> <url>`).
        remote: String,
    },
}

/// Inner enum for `jjf label <action>`. Separating the action from the
/// outer verb keeps the clap-derive `--help` clean (one help page per
/// add/rm rather than two flag combinations on one verb) and gives
/// `cli-update`'s scalar fan-out a pattern to copy if it wants nested
/// subcommands instead of flags.
#[derive(Debug, Subcommand)]
enum LabelAction {
    /// Add a label to an issue. Idempotent at the record level (the
    /// label set dedupes) but NOT at the commit level — a fresh
    /// `label-add` op lands either way per spec §5.2.
    Add {
        /// Full 7-char hex issue id. Bad parse → exit 2; valid id
        /// that doesn't exist on the bookmark → exit 1.
        id: String,

        /// Label to add. Must be non-empty; an empty string is a
        /// preflight failure (exit 2) at the CLI layer because the
        /// storage layer doesn't validate it.
        label: String,
    },

    /// Remove a label from an issue. No-op at the record level if the
    /// label isn't present, but a fresh `label-rm` op lands either way
    /// per spec §5.2.
    Rm {
        /// Full 7-char hex issue id. Bad parse → exit 2; valid id
        /// that doesn't exist on the bookmark → exit 1.
        id: String,

        /// Label to remove. Must be non-empty (same rule as `add`).
        label: String,
    },
}

/// Inner enum for `jjf metadata <action>`. Same shape rationale as
/// `LabelAction` (one help page per subcommand). `set` writes a
/// key/value (overwriting any prior value — last-write-wins per key);
/// `unset` removes a key.
#[derive(Debug, Subcommand)]
enum MetadataAction {
    /// Set a metadata key to a value. Overwrites any existing value
    /// for the key (last-write-wins). Is a no-op if the key already
    /// has this value (no commit lands).
    Set {
        /// Full 7-char hex issue id (or slug). Bad parse → exit 2;
        /// valid id that doesn't exist → exit 1.
        id: String,

        /// Metadata key. Must be non-empty; an empty string is a
        /// preflight failure (exit 2) at the CLI layer.
        key: String,

        /// Metadata value. May be empty; must not contain newlines
        /// (the storage layer rejects newlines in key or value).
        value: String,
    },

    /// Remove a metadata key. Is a no-op if the key is already absent
    /// (no commit lands).
    Unset {
        /// Full 7-char hex issue id (or slug). Bad parse → exit 2;
        /// valid id that doesn't exist → exit 1.
        id: String,

        /// Metadata key to remove. Must be non-empty (same rule as
        /// `set`).
        key: String,
    },
}

/// Inner enum for `jjf dep <action>` — v2.4 (`agent-dep-types`).
/// Same shape rationale as `LabelAction` (one help page per
/// subcommand, clean clap-derive output). The three verbs are
/// `add` / `rm` / `tree`; `add` and `rm` take a `--kind` flag
/// defaulting to `blocks` for v1 muscle memory.
#[derive(Debug, Subcommand)]
enum DepAction {
    /// Add a typed dependency edge from `<child>` to `<parent>`.
    /// Default kind is `blocks` (the v1 default). Lands one
    /// `dep-add` op with the `Jjf-Dep-Kind:` trailer carrying the
    /// chosen kind. Idempotent at the record level (the edge set
    /// dedupes on `(target, kind)`) but NOT at the commit level —
    /// a fresh op lands either way per spec §5.2.
    ///
    /// Both arguments accept `id`-or-`slug` per the v2.1 resolver.
    Add {
        /// The owning issue (the "child" in parent-child terminology).
        child: String,
        /// The target issue (the "parent" in parent-child terminology;
        /// the blocker in blocks terminology).
        parent: String,
        /// Edge kind. Defaults to `blocks` for v1 muscle memory.
        #[arg(long, value_enum, default_value_t = DepKindArg::Blocks)]
        kind: DepKindArg,
    },
    /// Remove a typed dependency edge of the given kind. Only edges
    /// with the matching `(target, kind)` are removed, leaving
    /// other-kind edges to the same target intact.
    Rm {
        /// The owning issue (the "child" in parent-child terminology).
        child: String,
        /// The target issue.
        parent: String,
        /// Edge kind. Defaults to `blocks` for v1 muscle memory.
        #[arg(long, value_enum, default_value_t = DepKindArg::Blocks)]
        kind: DepKindArg,
    },
    /// Print the parent-child tree rooted at `<id>`. Walks the
    /// `parent-child` edges in the CHILD direction (X is a child of
    /// Y iff X carries a `parent-child` edge with target Y).
    /// Cycles surface as a `(cycle)` marker; depth is unbounded.
    /// `--json` emits the structured `DepTree` envelope.
    Tree {
        /// Root issue (id-or-slug).
        id: String,
    },
}

/// Inner enum for `jjf remote <action>`. Same shape rationale as
/// `LabelAction` — one help page per subcommand, clean clap-derive
/// `--help` output, plus distinct positional shapes per arm.
#[derive(Debug, Subcommand)]
enum RemoteAction {
    /// Add a git remote to the underlying jj repo. Wraps `jj git
    /// remote add <name> <url>`. URL is whatever git accepts; jj
    /// validates it and we surface its error verbatim. Adding a name
    /// that already exists is exit 2 (`remote_already_exists`).
    Add {
        /// Remote name (e.g. `origin`, `upstream`). Free-form string,
        /// jj decides what's legal.
        name: String,

        /// Remote URL or local path. Local paths are resolved to
        /// absolute form by jj.
        url: String,
    },

    /// List configured git remotes. Plain-text output is one
    /// `<name>\t<url>` per line (tab-separated, no header — matches
    /// the `ls`-style convention every other read verb in jjforge
    /// uses). `--json` emits a JSON array of `{name, url}` objects.
    Ls,

    /// Remove a git remote from the underlying jj repo. Wraps `jj git
    /// remote remove <name>` — note that jj also forgets bookmarks
    /// tracked from that remote (its own behavior, not jjforge's).
    /// Removing a name that doesn't exist is exit 2 (`remote_not_found`).
    Rm {
        /// Remote name to remove.
        name: String,
    },
}

/// What the binary can fail with. Kept narrow so `main` can fan a
/// `Result<_, CliError>` out to the three-tier exit-code convention
/// in one match.
#[derive(Debug, thiserror::Error)]
enum CliError {
    /// Bubbled up from `jjf-storage`. The `NotAJjRepo` variant gets
    /// special treatment in `exit_code_for`; everything else is a
    /// generic runtime failure.
    #[error(transparent)]
    Storage(#[from] StorageError),

    /// `std::env::current_dir` failed. Vanishingly rare in practice,
    /// but we surface it as a preflight failure rather than letting
    /// it panic.
    #[error("could not determine current working directory: {0}")]
    Cwd(std::io::Error),

    /// Reading the issue body from `-F <path>` (or `-F -`) failed.
    /// Preflight failure: the user gave us a path we couldn't open
    /// (or stdin closed in a way we couldn't drain).
    #[error("could not read body from {from}: {error}")]
    BodyRead {
        from: String,
        error: std::io::Error,
    },

    /// A `-d / --dep` value didn't parse as a valid `IssueId`.
    /// Preflight failure (exit 2) — the user typed something wrong;
    /// no point in starting the dance only to fail mid-write.
    #[error("invalid issue id for --dep {value:?}: {error}")]
    BadDepId { value: String, error: IdError },

    /// A `-d / --dep <kind>:<id>` value carried an unknown kind token
    /// (i.e. not one of `blocks`, `parent-child`, `related`,
    /// `discovered-from`). v2.4 (`agent-dep-types`). Preflight failure
    /// (exit 2).
    #[error("invalid dep kind {kind:?} in spec {value:?}")]
    BadDepKind { value: String, kind: String },

    /// A positional issue id (e.g. `jjf show <id>`) didn't parse as
    /// a valid `IssueId`. Preflight failure (exit 2) — the user typed
    /// something the storage layer can never resolve.
    ///
    /// **As of v2.1 (`issue-type-and-slug-fields`)** every id-taking
    /// verb routes through `Storage::resolve`, which falls through to
    /// a slug lookup before declaring failure — so a bad-shape input
    /// now surfaces as `SlugNotFound` (the operator might have meant
    /// a slug). The variant stays defined for `--dep` parsing (where
    /// only ids are accepted) and for shape stability in the error
    /// kind table; the positional-id path no longer constructs it.
    #[allow(dead_code)]
    #[error("invalid issue id {value:?}: {error}")]
    BadIssueId { value: String, error: IdError },

    /// We're inside a jj repo, but the `issues` bookmark doesn't
    /// exist yet. Surfaced as a preflight (exit 2) so the user gets
    /// a typed signal that they need to run `jjf init` rather than
    /// the raw jj-stderr we'd get from trying to write against an
    /// empty `bookmarks(issues)` revset.
    #[error("the `issues` bookmark does not exist in {0}; run `jjf init` first")]
    MissingIssuesBookmark(PathBuf),

    /// Probing for the `issues` bookmark (or for jj-repo-presence)
    /// failed for a reason other than absence — e.g. the `jj`
    /// binary isn't on PATH, or returned an unexpected error. This
    /// is a runtime failure, not a preflight one.
    #[error("could not probe jj state: {0}")]
    Probe(std::io::Error),

    /// The user piped (or pointed `-F` at) an empty body for `jjf
    /// comment`. An empty comment is almost certainly a mistake; we
    /// reject at the CLI layer (exit 2) rather than let the storage
    /// layer record a zero-byte comment.
    #[error("comment body is empty; pipe non-empty content via -F - or pass -F <path>")]
    EmptyCommentBody,

    /// The user passed an empty string for `jjf label add|rm <id>
    /// <label>`. The storage layer doesn't validate this — it would
    /// happily land a `label-add`/`label-rm` op with `label=""` — so
    /// we reject at the CLI layer (exit 2). An empty label is almost
    /// certainly a shell-quoting mistake (`jjf label add $ID $L` with
    /// `$L` unset) rather than intent.
    #[error("label must not be empty")]
    EmptyLabel,

    /// The user passed an empty key for `jjf metadata set|unset <id>
    /// <key> …`. Same rationale as `EmptyLabel`: the storage layer
    /// doesn't validate emptiness, and an empty key is almost
    /// certainly a shell-quoting mistake. Preflight failure (exit 2).
    #[error("metadata key must not be empty")]
    EmptyMetadataKey,

    /// `jjf comment` couldn't resolve a comment author. Either jj's
    /// `user.name` isn't configured AND no `--author` override was
    /// supplied, or the override itself is empty/whitespace. Preflight
    /// failure (exit 2) — there's nothing for the storage layer to do
    /// without an author.
    #[error(
        "no comment author available; set jj user.name (e.g. `jj config set --user user.name 'Your Name'`) or pass --author <NAME>"
    )]
    MissingAuthor,

    /// `jjf update <id>` ran with no field flags. Clap can't enforce
    /// the at-least-one rule for us (all the field flags are
    /// `Option<_>` or bool), so we check in the run fn and surface a
    /// typed exit-2 hint pointing at the available flags.
    #[error(
        "nothing to update; pass at least one of --title / --status / --body-file / --assignee / --unset-assignee / --claim / --unclaim"
    )]
    NoUpdateFields,

    /// `jjf remote add <name> <url>` was asked to add a remote whose
    /// name is already taken. jj surfaces this via stderr containing
    /// "already exists"; we translate that one phrase to a typed
    /// preflight error (exit 2) so callers get a stable `kind` to
    /// branch on rather than having to grep jj's stderr themselves.
    #[error("git remote already exists: {0}")]
    RemoteAlreadyExists(String),

    /// `jjf remote rm <name>` was asked to remove a remote that
    /// doesn't exist. jj surfaces this via stderr containing "No git
    /// remote named"; we translate to a typed preflight error
    /// (exit 2) for the same reason as `RemoteAlreadyExists`.
    #[error("git remote not found: {0}")]
    RemoteNotFound(String),

    /// `jjf remote *` shelled out to `jj git remote ...` and got a
    /// non-zero exit that wasn't one of the two typed cases above.
    /// Runtime failure (exit 1) — surfaces jj's stderr verbatim so
    /// the operator can see what jj said. URL syntax errors, network-
    /// adjacent failures, and anything else jj rejects land here.
    #[error("jj git remote failed: {0}")]
    JjGitRemote(String),

    /// `jjf push` could not reach the remote — network failure,
    /// hostname unresolvable, TCP closed, etc. Runtime (exit 1): the
    /// command was well-formed, the network just wasn't.
    #[error("push to {remote} failed (network): {stderr}")]
    PushNetworkFailure { remote: String, stderr: String },

    /// `jjf push` reached the remote but the remote rejected our
    /// credentials. Runtime (exit 1).
    #[error("push to {remote} failed (auth): {stderr}")]
    PushAuthFailure { remote: String, stderr: String },

    /// `jjf push` reached the remote but the remote rejected the
    /// update (non-fast-forward, hook rejection, etc.). Runtime
    /// (exit 1).
    ///
    /// The Display impl renders a short, deterministic, single-line
    /// message — no raw git stderr, no version-dependent advisory
    /// tokens (e.g. "fetch first", git's own multi-line `hint:`
    /// preamble). The structured fields (`refs_rejected`, `hint`,
    /// `stderr_raw`) go into the `--json` envelope's `details`. This
    /// way the contract for scripts is the typed `details` keys —
    /// not the message text and not raw git output.
    ///
    /// `refs_rejected` is the parsed list of refs git rejected
    /// (e.g. `refs/jjf/issues/bfcfe03`) extracted from stderr lines
    /// of the form `! [rejected]   <src> -> <dst> (fetch first)`.
    /// Empty if parsing didn't recognise any line — surfaces as
    /// `null` in the JSON envelope so callers can distinguish "no
    /// rejected lines parsed" from "parsing succeeded but the list
    /// happens to be empty". See `parse_rejected_refs`.
    ///
    /// `stderr_raw` keeps the original git stderr around for
    /// debugging callers (and operators reading the human message)
    /// without putting it on the contract surface.
    #[error("push to {remote} rejected (non-fast-forward); the remote moved since you last pulled")]
    PushRejected {
        remote: String,
        stderr: String,
        refs_rejected: Vec<String>,
    },

    /// `jjf push` shelled out and got a non-zero exit that wasn't
    /// one of the typed cases above. Runtime (exit 1).
    #[error("jj git push failed: {0}")]
    JjGitPush(String),

    /// `jjf pull` could not reach the remote. Runtime (exit 1).
    #[error("pull from {remote} failed (network): {stderr}")]
    PullNetworkFailure { remote: String, stderr: String },

    /// `jjf pull` reached the remote but credentials were rejected.
    /// Runtime (exit 1).
    #[error("pull from {remote} failed (auth): {stderr}")]
    PullAuthFailure { remote: String, stderr: String },

    /// `jjf pull` shelled out to `jj git fetch` and got a non-zero
    /// exit that wasn't one of the typed cases above. Runtime
    /// (exit 1).
    #[error("jj git fetch failed: {0}")]
    JjGitFetch(String),

    /// Legacy v1 file-bytes merge driver failure: the issue record's
    /// body field had free-text conflicts the LWW/union policy
    /// couldn't dispatch. Runtime (exit 1). **As of the
    /// `sync-conflict-fallback` switch (`bfc732b`), this variant is
    /// unreachable from `jjf pull`** — the op-space resolver has no
    /// "unmergeable" failure mode, body-text divergence resolves
    /// LWW by `Jjf-At:` timestamp. The variant stays defined so the
    /// JSON envelope's error-kind enum, the exit-code table, and any
    /// external caller of `jjf_merge::resolve` still see a stable
    /// shape. See `docs/cli-json.md` `pull` section for the contract.
    #[allow(dead_code)]
    #[error("merge driver could not auto-resolve issue {issue_id}: {detail}\nworking copy left with conflict markers for manual resolution")]
    Unmergeable { issue_id: String, detail: String },

    /// Legacy v1 file-bytes merge driver failure: an
    /// `issues/<id>.comments.jsonl` file had conflict markers the v1
    /// driver couldn't handle. Runtime (exit 1). **As of
    /// `sync-conflict-fallback` (`bfc732b`), this variant is
    /// unreachable from `jjf pull`** — the op-space resolver builds
    /// the merged comments file as a union of each head's pristine
    /// `.comments.jsonl` (read via `jj file show -r <head>`), so
    /// jj's conflict markers never appear in the working copy on
    /// the operator path. Same rationale as `Unmergeable` above for
    /// keeping the variant defined.
    #[allow(dead_code)]
    #[error("merge driver does not handle conflicted comment file for issue {issue_id} (v1 limitation)\nworking copy left with conflict markers for manual resolution")]
    CommentFileConflict { issue_id: String },

    /// `jjf update --slug` / `jjf new --slug` was handed a slug that
    /// failed validation (charset, length, hyphen rules). Preflight
    /// failure (exit 2). The `reason` field is the typed
    /// rejection variant; `slug` is what the operator supplied.
    #[error("invalid slug {slug:?}: {reason}")]
    InvalidSlug {
        slug: String,
        reason: SlugInvalidReason,
    },

    /// `jjf new -t` / `jjf update --title` was handed a title that
    /// failed validation (empty, embedded newline, embedded null
    /// byte, other control character). Preflight failure (exit 2).
    /// The `reason` field is the typed rejection variant; `title`
    /// is what the operator supplied. Added in
    /// `qa-title-validation` (issue `e4e483b`).
    #[error("invalid title: {reason}")]
    InvalidTitle {
        title: String,
        reason: TitleInvalidReason,
    },

    /// `jjf new -F`, `jjf update --body-file`, or `jjf comment -F`
    /// was handed a body that exceeded the documented cap
    /// (`BODY_MAX_BYTES` = 65,536 bytes, matching GitHub's issue-
    /// body limit). Preflight failure (exit 2). The CLI envelope
    /// kind is `body_too_large`. Added in issue `679444a` (QA
    /// red-team 2026-06-25 sub-pass 4 C3).
    #[error("invalid body: {reason}")]
    InvalidBody { reason: BodyInvalidReason },

    /// `jjf new -p` or `jjf update --priority` was handed an
    /// integer outside the documented `0..=4` window. Preflight
    /// failure (exit 2). The CLI envelope kind is
    /// `invalid_priority`. Added in `priority-field` (ticket
    /// `326bbf7`).
    #[error("invalid priority: {reason}")]
    InvalidPriority { reason: PriorityInvalidReason },

    /// `jjf dep add <X> <X>` (or the inline `jjf new -d <self-id>`)
    /// was asked to land an edge from an issue to itself. Self-deps
    /// make the child permanently blocked by itself, so the
    /// boundary rejects them. Preflight failure (exit 2). The
    /// `id` field is the offending issue id, echoed back so the
    /// operator can correct the call. Added in
    /// `qa-dep-validation` (issue `d1a01f0`).
    ///
    /// In practice the storage layer's
    /// [`StorageError::SelfDependency`] surfaces this case — the
    /// CLI-side variant stays defined so future callers (the
    /// upcoming MCP server, scripted batch creators) can construct
    /// it directly without going through `Storage`.
    #[allow(dead_code)]
    #[error("issue {id} cannot depend on itself")]
    SelfDependency { id: String },

    /// `jjf dep add <source> <target>` would close a cycle in the
    /// `blocks`-edge graph. Issues caught in a `blocks` cycle are
    /// permanently invisible to `jjf ready` (every node has at
    /// least one active blocks-dep), so the boundary rejects the
    /// write rather than land the silent landmine. Preflight failure
    /// (exit 2). v2.6 (`dep-cycle-undetected`, issue `43c7615`).
    ///
    /// `cycle` is the chain `[target, ..., source]` — the existing
    /// path that, combined with the proposed `source -> target`
    /// edge, would close. Echoed back to the operator under
    /// `details.cycle` so they can pinpoint which edges to break.
    ///
    /// In practice the storage layer's
    /// [`StorageError::DependencyCycle`] surfaces this case — the
    /// CLI-side variant stays defined so future callers (MCP
    /// server, scripted batch creators) can construct it directly.
    #[allow(dead_code)]
    #[error(
        "adding blocks-edge {from} -> {target} would close a dependency cycle"
    )]
    DependencyCycle {
        from: String,
        target: String,
        cycle: Vec<String>,
    },

    /// A slug write would collide with an existing issue (open,
    /// in-progress, blocked, or closed — spec v2.6, issue
    /// `a105e0b`). Preflight failure (exit 2). `conflicts_with`
    /// is the id of the issue already holding the slug.
    ///
    /// In practice the storage layer's `Error::SlugCollision`
    /// surfaces this case — the CLI-side variant stays defined so
    /// that future callers can construct it directly without going
    /// through `Storage` (e.g. if the CLI grows pre-flight
    /// uniqueness checks).
    #[allow(dead_code)]
    #[error(
        "slug {slug:?} already in use by issue {conflicts_with}"
    )]
    SlugCollision {
        slug: String,
        conflicts_with: String,
    },

    /// `Storage::resolve` couldn't translate the handle the operator
    /// supplied: it wasn't a parseable 7-hex id, and no open-or-closed
    /// issue carries that slug. Preflight failure (exit 2).
    #[error("no issue with handle {handle:?}")]
    SlugNotFound { handle: String },

    /// `jjf remember` ran with no value source — neither a positional
    /// arg nor `-F`. Preflight failure (exit 2).
    #[error("no memory value supplied; pass a positional argument or `-F <path|->`")]
    MissingMemoryValue,

    /// `jjf remember` was unable to derive a key from the value (the
    /// value contained no alphanumeric characters). Preflight failure
    /// (exit 2). The operator should pass `--key`.
    #[error("could not derive memory key from {value:?}; pass --key <slug>")]
    EmptyMemoryKey { value: String },

    /// `jjf recall <key>` or `jjf forget <key>` looked up a memory key
    /// that doesn't exist at the bookmark tip. Runtime failure
    /// (exit 1) — the input was well-formed, the answer is "no such
    /// memory."
    #[error("no memory with key {key:?}")]
    MemoryNotFound { key: String },

    /// `jjf update --claim` (or `jjf ready --claim`) couldn't find
    /// a current user. Preflight failure (exit 2) — claims require
    /// an identity to assign to. The chain is `--actor <name>` >
    /// `JJF_ACTOR` env > `jj config user.name`; this error fires
    /// when every slot is empty. v2.3 (`agent-claim-atomic`); chain
    /// extended v2.12 (`actor-override-chain`).
    #[error(
        "no current user available; set jj user.name (e.g. `jj config set --user user.name 'Your Name'`) OR export `JJF_ACTOR=<name>` to claim issues"
    )]
    NoCurrentUser,

    /// `jjf ready --claim` was used with `--limit` other than 1.
    /// Atomically claiming multiple issues at once doesn't compose
    /// — agents work one ticket at a time. Preflight failure
    /// (exit 2). v2.3 (`agent-claim-atomic`).
    #[error("--claim requires --limit 1; claiming multiple at once doesn't compose")]
    ClaimRequiresLimitOne,

    /// `jjf update --claim` was asked to claim an issue already in
    /// the InProgress state with a different assignee. Preflight
    /// failure (exit 2) so the orchestrator can branch on
    /// `already_claimed`. The `by` field carries the existing
    /// assignee for the operator's hint.
    ///
    /// In practice the storage layer's
    /// [`StorageError::AlreadyClaimed`] surfaces this case — the
    /// CLI-side variant stays defined so future callers can
    /// construct it directly without going through `Storage`. v2.3
    /// (`agent-claim-atomic`).
    #[allow(dead_code)]
    #[error("issue already claimed by {by:?}")]
    AlreadyClaimed { by: String },

    /// A concurrent jjforge writer landed first; the storage layer's
    /// CAS on the per-issue ref lost the race (v3) or jj's
    /// "Concurrent checkout" conflict fired (v2). Runtime
    /// failure (exit 1): the command was well-formed, the loser just
    /// has to re-run. Storage already auto-retried once for non-
    /// slug-claim mutations before surfacing this — if you see it,
    /// the race repeated, or the variant was a fail-fast slug-claim
    /// create (where retry wouldn't help). The `hint` field is the
    /// operator-facing message rendered verbatim by the text
    /// renderer; the JSON envelope surfaces it as `details.hint`.
    /// v2.x (`qa-concurrent-write-ux`, issue `277f559`).
    ///
    /// In practice the storage layer's
    /// [`StorageError::ConcurrentWrite`] surfaces this case — the
    /// CLI-side variant stays defined so future callers can
    /// construct it directly without going through `Storage`.
    #[allow(dead_code)]
    #[error("concurrent write conflict; {hint}")]
    ConcurrentWrite { hint: String },

    /// `jjf ready --claim` raced another claimer for the same id and
    /// the storage layer's CAS-loss retry found that the id was
    /// already claimed by the SAME user (i.e., another parallel
    /// `ready --claim` of ours took the slot). The orchestrator
    /// should re-run `ready --claim` to pick the next available id.
    /// Runtime failure (exit 1) — the input was well-formed; the
    /// race is the issue. v3-fix (`a6b8fb7`).
    #[error("claim raced another claimer for issue {id}; re-run `ready --claim` to pick the next available id")]
    ClaimRaceLost { id: String },
}

impl CliError {
    /// Per the top-of-file convention:
    ///
    /// - `2` — preflight / argument failure (this includes "not a jj
    ///   repo", since the verb can't proceed without one).
    /// - `1` — runtime failure.
    fn exit_code(&self) -> u8 {
        match self {
            CliError::Storage(StorageError::NotAJjRepo(_)) => 2,
            // Runtime failure: the operator typed a well-formed
            // command; the on-disk repo is in an inconsistent state
            // (sentinel ref hand-wired to a non-commit object).
            CliError::Storage(StorageError::CorruptSentinel { .. }) => 1,
            CliError::Storage(StorageError::InvalidSlug { .. }) => 2,
            CliError::Storage(StorageError::InvalidTitle { .. }) => 2,
            CliError::Storage(StorageError::InvalidBody { .. }) => 2,
            CliError::Storage(StorageError::InvalidPriority { .. }) => 2,
            CliError::Storage(StorageError::SlugCollision { .. }) => 2,
            CliError::Storage(StorageError::SlugNotFound { .. }) => 2,
            CliError::Storage(StorageError::AlreadyClaimed { .. }) => 2,
            CliError::Storage(StorageError::SelfDependency { .. }) => 2,
            CliError::Storage(StorageError::DependencyCycle { .. }) => 2,
            CliError::Storage(StorageError::ConcurrentWrite { .. }) => 1,
            CliError::Cwd(_) => 2,
            CliError::BodyRead { .. } => 2,
            CliError::BadDepId { .. } => 2,
            CliError::BadDepKind { .. } => 2,
            CliError::BadIssueId { .. } => 2,
            CliError::MissingIssuesBookmark(_) => 2,
            CliError::EmptyCommentBody => 2,
            CliError::EmptyLabel => 2,
            CliError::EmptyMetadataKey => 2,
            CliError::MissingAuthor => 2,
            CliError::NoUpdateFields => 2,
            CliError::RemoteAlreadyExists(_) => 2,
            CliError::RemoteNotFound(_) => 2,
            CliError::InvalidSlug { .. } => 2,
            CliError::InvalidTitle { .. } => 2,
            CliError::InvalidBody { .. } => 2,
            CliError::InvalidPriority { .. } => 2,
            CliError::SelfDependency { .. } => 2,
            CliError::DependencyCycle { .. } => 2,
            CliError::SlugCollision { .. } => 2,
            CliError::SlugNotFound { .. } => 2,
            CliError::MissingMemoryValue => 2,
            CliError::EmptyMemoryKey { .. } => 2,
            CliError::MemoryNotFound { .. } => 1,
            CliError::NoCurrentUser => 2,
            CliError::ClaimRequiresLimitOne => 2,
            CliError::AlreadyClaimed { .. } => 2,
            CliError::ConcurrentWrite { .. } => 1,
            CliError::ClaimRaceLost { .. } => 1,
            CliError::Probe(_) => 1,
            CliError::JjGitRemote(_) => 1,
            // Sync verbs: the user typed a well-formed command; the
            // network / remote / merge layer told us "no." Runtime
            // failures (exit 1), not preflight.
            CliError::PushNetworkFailure { .. } => 1,
            CliError::PushAuthFailure { .. } => 1,
            CliError::PushRejected { .. } => 1,
            CliError::JjGitPush(_) => 1,
            CliError::PullNetworkFailure { .. } => 1,
            CliError::PullAuthFailure { .. } => 1,
            CliError::JjGitFetch(_) => 1,
            CliError::Unmergeable { .. } => 1,
            CliError::CommentFileConflict { .. } => 1,
            // `IssueNotFound` is the user typing a valid id that just
            // doesn't exist — runtime failure, not preflight (the input
            // was well-formed; we tried to honor it and it wasn't there).
            CliError::Storage(_) => 1,
        }
    }

    /// Stable, machine-greppable identifier for the error variant. Used
    /// as the `kind` field in the `--json` error envelope; scripts and
    /// the upcoming MCP server pattern-match on these strings rather
    /// than on the human-readable `message`. Adding a new variant?
    /// Pick a lowercase snake_case name and document it in
    /// `docs/cli-json.md`'s error-kind table.
    fn kind(&self) -> &'static str {
        match self {
            CliError::Storage(StorageError::NotAJjRepo(_)) => "not_a_jj_repo",
            CliError::Storage(StorageError::CorruptSentinel { .. }) => "corrupt_sentinel",
            CliError::Storage(StorageError::IssueNotFound(_)) => "issue_not_found",
            CliError::Storage(StorageError::Invalid(_)) => "invalid_input",
            CliError::Storage(StorageError::Clock(_)) => "clock_error",
            CliError::Storage(StorageError::Io(_)) => "io_error",
            CliError::Storage(StorageError::Json(_)) => "json_error",
            CliError::Storage(StorageError::Jj(_)) => "jj_error",
            // v3 storage (`docs/storage-out-of-tree.md`, ticket
            // `eb42f50`): the v3 write path spawns `git` directly
            // rather than `jj`. A non-CAS git failure surfaces here.
            CliError::Storage(StorageError::Git(_)) => "git_error",
            CliError::Storage(StorageError::InvalidSlug { .. }) => "invalid_slug",
            CliError::Storage(StorageError::InvalidTitle { .. }) => "invalid_title",
            CliError::Storage(StorageError::InvalidBody { .. }) => "body_too_large",
            CliError::Storage(StorageError::InvalidPriority { .. }) => "invalid_priority",
            CliError::Storage(StorageError::SlugCollision { .. }) => "slug_collision",
            CliError::Storage(StorageError::SlugNotFound { .. }) => "slug_not_found",
            CliError::Storage(StorageError::AlreadyClaimed { .. }) => "already_claimed",
            CliError::Storage(StorageError::SelfDependency { .. }) => "self_dependency",
            CliError::Storage(StorageError::DependencyCycle { .. }) => "dependency_cycle",
            CliError::Storage(StorageError::ConcurrentWrite { .. }) => "concurrent_write",
            CliError::InvalidSlug { .. } => "invalid_slug",
            CliError::InvalidTitle { .. } => "invalid_title",
            CliError::InvalidBody { .. } => "body_too_large",
            CliError::InvalidPriority { .. } => "invalid_priority",
            CliError::SelfDependency { .. } => "self_dependency",
            CliError::DependencyCycle { .. } => "dependency_cycle",
            CliError::SlugCollision { .. } => "slug_collision",
            CliError::SlugNotFound { .. } => "slug_not_found",
            CliError::MissingMemoryValue => "missing_memory_value",
            CliError::EmptyMemoryKey { .. } => "empty_memory_key",
            CliError::MemoryNotFound { .. } => "memory_not_found",
            CliError::Cwd(_) => "cwd_error",
            CliError::BodyRead { .. } => "body_read_error",
            CliError::BadDepId { .. } => "bad_id",
            CliError::BadDepKind { .. } => "bad_dep_kind",
            CliError::BadIssueId { .. } => "bad_id",
            CliError::MissingIssuesBookmark(_) => "missing_issues_bookmark",
            CliError::EmptyCommentBody => "empty_body",
            CliError::EmptyLabel => "empty_label",
            CliError::EmptyMetadataKey => "empty_metadata_key",
            CliError::MissingAuthor => "missing_author",
            CliError::NoUpdateFields => "no_update_fields",
            CliError::RemoteAlreadyExists(_) => "remote_already_exists",
            CliError::RemoteNotFound(_) => "remote_not_found",
            CliError::JjGitRemote(_) => "jj_git_remote_error",
            CliError::Probe(_) => "probe_error",
            CliError::PushNetworkFailure { .. } => "push_network_failure",
            CliError::PushAuthFailure { .. } => "push_auth_failure",
            CliError::PushRejected { .. } => "push_rejected",
            CliError::JjGitPush(_) => "jj_git_push_error",
            CliError::PullNetworkFailure { .. } => "pull_network_failure",
            CliError::PullAuthFailure { .. } => "pull_auth_failure",
            CliError::JjGitFetch(_) => "jj_git_fetch_error",
            CliError::Unmergeable { .. } => "unmergeable",
            CliError::CommentFileConflict { .. } => "comment_file_conflict",
            CliError::NoCurrentUser => "no_current_user",
            CliError::ClaimRequiresLimitOne => "claim_requires_limit_one",
            CliError::AlreadyClaimed { .. } => "already_claimed",
            CliError::ConcurrentWrite { .. } => "concurrent_write",
            CliError::ClaimRaceLost { .. } => "claim_race_lost",
        }
    }

    /// Optional structured per-variant context that goes into the
    /// `details` field of the error envelope. Returns `Value::Null` if
    /// the variant has nothing structured to add beyond the kind and
    /// message — callers should treat null as "no details" and not as
    /// a meaningful payload.
    ///
    /// Fields are chosen for what an automated caller can act on: the
    /// issue id it asked about, the path it tried to read, the bad
    /// argument value. Free-form strings live in `message`.
    fn details(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            CliError::Storage(StorageError::NotAJjRepo(path)) => {
                json!({ "path": path.display().to_string() })
            }
            CliError::Storage(StorageError::CorruptSentinel { oid, kind }) => {
                json!({ "oid": oid, "object_type": kind })
            }
            CliError::Storage(StorageError::IssueNotFound(id)) => {
                json!({ "id": id.as_str() })
            }
            CliError::BodyRead { from, .. } => json!({ "from": from }),
            CliError::BadDepId { value, .. } => json!({ "value": value, "field": "dep" }),
            CliError::BadDepKind { value, kind } => json!({
                "value": value,
                "kind": kind,
                "field": "dep",
            }),
            CliError::BadIssueId { value, .. } => json!({ "value": value, "field": "id" }),
            CliError::MissingIssuesBookmark(path) => {
                json!({ "path": path.display().to_string() })
            }
            CliError::RemoteAlreadyExists(name) => json!({ "name": name }),
            CliError::RemoteNotFound(name) => json!({ "name": name }),
            CliError::PushNetworkFailure { remote, .. }
            | CliError::PushAuthFailure { remote, .. }
            | CliError::PullNetworkFailure { remote, .. }
            | CliError::PullAuthFailure { remote, .. } => json!({ "remote": remote }),
            // `push_rejected` carries the structured advisory and
            // (where parsable) the list of refs git rejected. The
            // human-readable `hint` mirrors `concurrent_write`'s
            // `details.hint` shape — callers that handle either
            // path can read the same key. `refs_rejected` is the
            // parsed-from-stderr ref list; `null` means parsing
            // recognised no rejected lines (better to be honest
            // than to ship an empty array that looks definitive).
            // `stderr_raw` keeps the original git output for
            // debugging out of the contract-mandated `message`
            // field — see the `cli-json.md` push_rejected row.
            CliError::PushRejected {
                remote,
                stderr,
                refs_rejected,
            } => {
                let refs_value = if refs_rejected.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::Array(
                        refs_rejected
                            .iter()
                            .map(|s| serde_json::Value::String(s.clone()))
                            .collect(),
                    )
                };
                json!({
                    "remote": remote,
                    "hint": format!("run `jjf pull {remote}` first, then retry the push"),
                    "refs_rejected": refs_value,
                    "stderr_raw": stderr,
                })
            }
            CliError::Unmergeable { issue_id, detail } => {
                json!({ "issue_id": issue_id, "detail": detail })
            }
            CliError::CommentFileConflict { issue_id } => json!({ "issue_id": issue_id }),
            CliError::Storage(StorageError::InvalidSlug { slug, reason })
            | CliError::InvalidSlug { slug, reason } => {
                json!({ "slug": slug, "reason": reason.as_str() })
            }
            CliError::Storage(StorageError::InvalidTitle { title, reason })
            | CliError::InvalidTitle { title, reason } => {
                // `ControlChar` carries the offending codepoint; the
                // other reasons don't have additional structure. Expose
                // `codepoint` as a top-level key in `details` rather
                // than a nested object so the JSON envelope stays flat
                // (matches the slug envelope's pattern).
                let mut obj = serde_json::Map::new();
                obj.insert("title".into(), serde_json::Value::String(title.clone()));
                obj.insert(
                    "reason".into(),
                    serde_json::Value::String(reason.as_str().into()),
                );
                if let TitleInvalidReason::ControlChar { codepoint } = reason {
                    obj.insert(
                        "codepoint".into(),
                        serde_json::Value::Number((*codepoint).into()),
                    );
                }
                serde_json::Value::Object(obj)
            }
            // Flat `limit` + `got` mirrors the operator-facing
            // GitHub `mediumblob` cap shape. Both are integers, not
            // strings (so scripted callers can branch on them
            // directly without re-parsing). Today the only
            // `BodyInvalidReason` is `TooLong`; future reasons
            // would gain their own keys here.
            CliError::Storage(StorageError::InvalidBody { reason })
            | CliError::InvalidBody { reason } => match reason {
                BodyInvalidReason::TooLong { limit, got } => json!({
                    "limit": *limit,
                    "got": *got,
                }),
            },
            CliError::Storage(StorageError::InvalidPriority { reason })
            | CliError::InvalidPriority { reason } => match reason {
                PriorityInvalidReason::OutOfRange { got } => json!({
                    "reason": reason.as_str(),
                    "got": *got,
                }),
            },
            CliError::Storage(StorageError::SlugCollision { slug, conflicts_with }) => {
                json!({ "slug": slug, "conflicts_with": conflicts_with.as_str() })
            }
            CliError::SlugCollision { slug, conflicts_with } => {
                json!({ "slug": slug, "conflicts_with": conflicts_with })
            }
            CliError::Storage(StorageError::SlugNotFound { handle })
            | CliError::SlugNotFound { handle } => json!({ "handle": handle }),
            CliError::EmptyMemoryKey { value } => json!({ "value": value }),
            CliError::MemoryNotFound { key } => json!({ "key": key }),
            CliError::Storage(StorageError::AlreadyClaimed { by })
            | CliError::AlreadyClaimed { by } => json!({ "by": by }),
            CliError::Storage(StorageError::SelfDependency { id }) => {
                json!({ "id": id.as_str() })
            }
            CliError::SelfDependency { id } => json!({ "id": id }),
            CliError::Storage(StorageError::DependencyCycle {
                from,
                target,
                cycle,
            }) => json!({
                "source": from.as_str(),
                "target": target.as_str(),
                "cycle": cycle.iter().map(IssueId::as_str).collect::<Vec<_>>(),
            }),
            CliError::DependencyCycle { from, target, cycle } => json!({
                "source": from,
                "target": target,
                "cycle": cycle,
            }),
            CliError::Storage(StorageError::ConcurrentWrite { hint })
            | CliError::ConcurrentWrite { hint } => json!({ "hint": hint }),
            CliError::ClaimRaceLost { id } => json!({ "id": id }),
            _ => serde_json::Value::Null,
        }
    }
}

/// Whether the top-level `--json` flag was set. Captured into a
/// process-wide slot the moment `Cli::parse()` succeeds so the error
/// reporter can render the right shape without needing the (possibly
/// partially-constructed) `Cli` value threaded through.
///
/// Stays `None` if parsing failed — clap exits before we get here, so
/// arg-parse errors render through clap's own machinery and miss the
/// JSON envelope. That's the documented exception in `docs/cli-json.md`.
static JSON_OUTPUT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Stash the flag so `report_error` can find it. `set` returns Err
    // if the cell was already initialized; that only happens in tests
    // that re-enter `main`, which we don't have — ignore the result.
    let _ = JSON_OUTPUT.set(cli.json);
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            report_error(&e);
            ExitCode::from(e.exit_code())
        }
    }
}

/// Render a `CliError` to stderr in either the plain `jjf: <msg>` form
/// or the `--json` error envelope:
///
/// ```json
/// {"ok": false, "error": {"kind": "<kind>", "message": "<msg>", "details": {...}}}
/// ```
///
/// Always stderr, never stdout — stdout is reserved for the verb's
/// (now empty) success payload so a caller can `2>/dev/null` a verb
/// they expect might fail and still get a clean stdout. Exit code is
/// the caller's job; this function only does the rendering.
fn report_error(e: &CliError) {
    let json = JSON_OUTPUT.get().copied().unwrap_or(false);
    if json {
        let details = e.details();
        let mut error_obj = serde_json::Map::new();
        error_obj.insert("kind".into(), serde_json::Value::String(e.kind().into()));
        error_obj.insert("message".into(), serde_json::Value::String(e.to_string()));
        // Only attach `details` when it's actually structured — saves
        // callers from a `details: null` they have to guard against.
        // The contract documents this: details is either absent or an
        // object with variant-specific fields.
        if !details.is_null() {
            error_obj.insert("details".into(), details);
        }
        let envelope = serde_json::json!({
            "ok": false,
            "error": serde_json::Value::Object(error_obj),
        });
        eprintln!("{envelope}");
    } else {
        eprintln!("jjf: {e}");
    }
}

fn run(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        Commands::Init => run_init(cli.json),
        Commands::New {
            title,
            file,
            labels,
            deps,
            parents,
            assignee,
            r#type,
            slug,
            priority,
            meta,
        } => run_new(cli.json, title, file, labels, deps, parents, assignee, r#type, slug, priority, meta),
        Commands::Show { id, include_memories } => {
            run_show(cli.json, id, include_memories)
        }
        Commands::Remember { value, key, file } => {
            run_remember(cli.json, value, key, file)
        }
        Commands::Memories { search } => run_memories(cli.json, search),
        Commands::Recall { key } => run_recall(cli.json, key),
        Commands::Forget { key } => run_forget(cli.json, key),
        Commands::Ls {
            status,
            labels,
            meta,
            types,
            slug,
            priorities,
            parent,
        } => run_ls(cli.json, status, labels, meta, types, slug, priorities, parent),
        Commands::Ready {
            labels,
            types,
            limit,
            include_claimed,
            include_blocked,
            claim,
            priorities,
            parent,
            meta,
        } => run_ready(
            cli.json,
            labels,
            types,
            limit,
            include_claimed,
            include_blocked,
            claim,
            priorities,
            parent,
            meta,
        ),
        Commands::Close { id } => run_set_status(cli.json, id, Status::Closed),
        Commands::Open { id } => run_set_status(cli.json, id, Status::Open),
        Commands::Abandon { id } => run_set_status(cli.json, id, Status::Abandoned),
        Commands::Assign { id, name } => run_assign(cli.json, id, name),
        Commands::Block { id, reason } => run_block(cli.json, id, reason),
        Commands::Unblock { id } => run_unblock(cli.json, id),
        Commands::Comment { id, file, author } => run_comment(cli.json, id, file, author),
        Commands::Label { action } => match action {
            LabelAction::Add { id, label } => {
                run_label(cli.json, id, label, LabelOp::Add)
            }
            LabelAction::Rm { id, label } => run_label(cli.json, id, label, LabelOp::Rm),
        },
        Commands::Metadata { action } => match action {
            MetadataAction::Set { id, key, value } => {
                run_metadata(cli.json, id, key, Some(value), MetadataOp::Set)
            }
            MetadataAction::Unset { id, key } => {
                run_metadata(cli.json, id, key, None, MetadataOp::Unset)
            }
        },
        Commands::Dep { action } => match action {
            DepAction::Add { child, parent, kind } => {
                run_dep(cli.json, child, parent, kind.into(), DepOp::Add)
            }
            DepAction::Rm { child, parent, kind } => {
                run_dep(cli.json, child, parent, kind.into(), DepOp::Rm)
            }
            DepAction::Tree { id } => run_dep_tree(cli.json, id),
        },
        Commands::Remote { action } => match action {
            RemoteAction::Add { name, url } => run_remote_add(cli.json, name, url),
            RemoteAction::Ls => run_remote_ls(cli.json),
            RemoteAction::Rm { name } => run_remote_rm(cli.json, name),
        },
        Commands::Push { remote } => run_push(cli.json, remote),
        Commands::Pull { remote } => run_pull(cli.json, remote),
        Commands::Search {
            query,
            status,
            labels,
            types,
            include_comments,
            limit,
            snippet_context,
            parent,
            include_metadata,
            meta,
        } => run_search(
            cli.json,
            query,
            status,
            labels,
            types,
            include_comments,
            limit,
            snippet_context,
            parent,
            include_metadata,
            meta,
        ),
        Commands::Stale {
            days,
            status,
            labels,
            types,
            limit,
            meta,
        } => run_stale(cli.json, days, status, labels, types, limit, meta),
        Commands::Update {
            id,
            title,
            status,
            r#type,
            slug,
            unset_slug,
            priority,
            unset_priority,
            body_file,
            assignee,
            unset_assignee,
            claim,
            unclaim,
            actor,
        } => run_update(
            cli.json,
            id,
            title,
            status,
            r#type,
            slug,
            unset_slug,
            priority,
            unset_priority,
            body_file,
            assignee,
            unset_assignee,
            claim,
            unclaim,
            actor,
        ),
    }
}

/// Which storage mutator `run_label` should call. Kept as a tiny enum
/// (rather than passing a function pointer or matching on
/// `LabelAction` twice) so the helper can render the right past-tense
/// verb (`added` / `removed`) without re-matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LabelOp {
    Add,
    Rm,
}

/// Which storage mutator `run_metadata` should call. Mirrors
/// [`LabelOp`]; lets the helper render the right past-tense verb
/// (`set` / `unset`) without re-matching on `MetadataAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetadataOp {
    Set,
    Unset,
}

/// Which storage mutator `run_dep` should call. Same shape rationale
/// as `LabelOp` — v2.4 (`agent-dep-types`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepOp {
    Add,
    Rm,
}

/// `jjf init` — wrap `Storage::init` against the cwd. Idempotent;
/// emits either a one-line success message or, with `--json`, the
/// ticket-spec `{"ok": true, "bookmark": "issues"}`.
///
/// Post-v3 (`add0646`), init on a fresh repo plants the v3
/// `refs/jjf/meta/format-version` sentinel ref — no `issues`
/// bookmark, no jj working-copy mutation. The `bookmark` field in
/// the JSON envelope stays for backward compatibility (existing
/// scripts read it); on a v3-fresh repo it names the bookmark that
/// WOULD have been written under v2 init, which is also the
/// (pre-migration) name a v2-shape repo carries forward. The
/// post-migration v3 repo has no bookmark, but no caller is
/// expected to act on the value besides logging it.
fn run_init(json: bool) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    Storage::init(&cwd)?;

    // Back-fill the v3 fetch refspec for every git remote already
    // configured on this repo. If the user cloned first and is now
    // running `jjf init`, the standard `+refs/heads/*:refs/remotes/.../*`
    // refspec their `git clone` (or `jj git clone`) wrote does NOT
    // carry `refs/jjf/*`. Add the jjforge namespace so subsequent
    // `git fetch <remote>` round-trips it (ticket `eaf0674`).
    if let Ok(canonical) = std::fs::canonicalize(&cwd) {
        let _ = backfill_fetch_refspec_for_all_remotes(&canonical);
    }

    if json {
        // We hand-build this object rather than using `serde_json::json!`
        // so the dep surface stays as narrow as possible — one tiny
        // object, no macro pulled in, no derive overhead. Field order
        // is fixed by the ticket: `ok` first, `bookmark` second.
        let out = serde_json::json!({
            "ok": true,
            "bookmark": ISSUES_BOOKMARK,
        });
        println!("{out}");
    } else {
        println!("jjf: initialized");
    }
    Ok(())
}

/// `jjf new -t <title> [-F <path|->] [-l <label>...] [-d <id>...] [-a <name>]`
/// — create one issue on the `issues` bookmark via the storage write
/// path and emit its id.
///
/// The preflight order matters: we parse the dep ids and read the body
/// BEFORE shelling out to jj, so user-typo / stdin-empty failures don't
/// land any half-state on the bookmark. The bookmark-presence probe
/// then runs against the cwd; if the bookmark is missing we surface a
/// `run jjf init first` message rather than letting the storage layer
/// fail mid-write on an empty `bookmarks(issues)` revset.
fn run_new(
    json: bool,
    title: String,
    file: Option<PathBuf>,
    labels: Vec<String>,
    deps: Vec<String>,
    parents: Vec<String>,
    assignee: Option<String>,
    type_arg: Option<TypeArg>,
    slug: Option<String>,
    priority: Option<u8>,
    meta: Vec<(String, String)>,
) -> Result<(), CliError> {
    // 1. Parse `-d` dep specs first — purely-local validation, no IO.
    // v2.4 (`agent-dep-types`): each spec is either a bare 7-hex id
    // (interpreted as `blocks:<id>`) or `<kind>:<id>` for explicit
    // kinds. The `kind` token is one of
    // `blocks`/`parent-child`/`related`/`discovered-from`; unknown
    // kinds are a preflight failure (exit 2).
    //
    // `--parent <id-or-slug>` (fj#3) is the discoverable shorthand for
    // the dominant child-of-epic case. Its values are resolved AFTER
    // storage opens (step 4), so slugs work the same way they do for
    // `jjf ls --parent`, `jjf ready --parent`, etc. (b417864).
    //
    // We emit parent `DepAdd` ops first, but storage sorts
    // `dependencies` on each write — read-back order is
    // `(target, kind)`-sorted, not insertion-order.
    let mut all_deps: Vec<DepEdge> = Vec::with_capacity(parents.len() + deps.len());
    for raw in deps {
        all_deps.push(parse_dep_spec(raw)?);
    }
    let parent_handles = parents;

    // 2. Read the body. `-F -` is stdin; `-F <path>` is the file's
    // bytes; omitted is empty. We deliberately preserve raw bytes — no
    // trim, no newline normalization — so round-trip stays exact.
    let body = read_body(file.as_deref())?;

    // 2a. Pre-validate the title at the CLI boundary so the user
    // gets a typed exit-2 error before any IO kicks off. Storage
    // will re-validate. See `qa-title-validation` (issue
    // `e4e483b`): embedded `\n` corrupts `jjf ls` rows; embedded
    // `\0` was silently truncated before this guard landed.
    if let Err(reason) = jjf_storage::validate_title(&title) {
        return Err(CliError::InvalidTitle {
            title: title.clone(),
            reason,
        });
    }

    // 2a'. Pre-validate the body cap at the CLI boundary. Issue
    // `679444a` (QA red-team 2026-06-25 sub-pass 4 C3): pre-fix,
    // a multi-MB body landed silently. We now match GitHub's
    // 65,536-byte cap. Storage re-validates.
    if let Err(reason) = jjf_storage::validate_body(&body) {
        return Err(CliError::InvalidBody { reason });
    }

    // 2b. Pre-validate the slug at the CLI boundary so the user gets
    // a typed exit-2 error before any IO kicks off. Storage will
    // re-validate; the duplicate is cheap and the early surface is
    // the friendlier hint.
    if let Some(slug) = &slug {
        if let Err(reason) = jjf_storage::validate_slug(slug) {
            return Err(CliError::InvalidSlug {
                slug: slug.clone(),
                reason,
            });
        }
    }

    // 2c. Pre-validate the priority at the CLI boundary. Clap's
    // range parser already rejects values outside 0..=4 with exit 2,
    // but a second check keeps the storage-side contract honest
    // even if a future caller wires the field through differently.
    if let Err(reason) = validate_priority(priority) {
        return Err(CliError::InvalidPriority { reason });
    }

    // 3. Resolve the cwd as an absolute path. `Storage::open` requires
    // absolute; we canonicalize so symlinks in the path don't bite us.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 4. Preflight: we're inside a jj repo AND the `issues` bookmark
    // exists. The storage layer doesn't distinguish missing-bookmark
    // today (see follow-ups in the cli-new/cli-show closing comments);
    // doing the probe here keeps the user-facing error precise without
    // expanding the storage API. Implementation lives in `preflight`
    // so the read verbs share the same code.
    preflight::issues_bookmark(&cwd)?;

    // 5. Open storage. We need it before resolving `--parent` handles
    // so slugs work (b417864), and obviously before the write itself.
    let storage = Storage::open(&cwd)?;

    // 5a. Resolve every `--parent <handle>` value against the open
    // storage so slugs work the same way they do for `jjf ls --parent`
    // and friends (b417864). A 7-hex handle short-circuits inside
    // `resolve_handle` with no bookmark walk; a non-hex handle with
    // no matching slug surfaces as `slug_not_found` (exit 2).
    for raw in parent_handles {
        let target = resolve_handle(&storage, &raw)?;
        all_deps.push(DepEdge::new(target, DepKind::ParentChild));
    }
    let deps = all_deps;

    // 5b. Collect --meta pairs into a BTreeMap. Duplicate keys: last
    // wins (BTreeMap insertion order; documented in CLI help text).
    let metadata: std::collections::BTreeMap<String, String> =
        meta.into_iter().collect();

    // 6. Hand the draft to storage.
    let draft = IssueDraft {
        title,
        body,
        labels,
        dependencies: deps,
        assignee,
        type_: type_arg.map(IssueType::from),
        slug,
        priority,
        metadata,
    };
    let id = storage.create_issue(&draft)?;

    // 6. Emit. Plain text is just the id, one line; --json matches
    // init's `{"ok": true, ...}` shape.
    if json {
        let out = serde_json::json!({
            "ok": true,
            "id": id.as_str(),
        });
        println!("{out}");
    } else {
        println!("{id}");
    }
    Ok(())
}

/// Resolve a user-supplied handle (`id`-or-`slug`) to a concrete
/// [`IssueId`] using the open `Storage`. The CLI calls this at the
/// boundary of every id-taking verb (`show`, `update`, `close`,
/// `open`, `label add|rm`, `comment`).
///
/// Behavior:
///
/// - If `handle` parses as an `IssueId` (7-char lowercase hex),
///   return it directly with no bookmark lookup.
/// - Else, walk the bookmark and return the id whose slug matches.
///   If no slug matches, surface as `CliError::SlugNotFound` (exit 2).
///
/// We deliberately don't pre-check `IssueId::parse` here: a string
/// that's a 7-hex id but contains no slug-shaped characters will
/// return immediately; everything else proceeds to the storage-side
/// resolver. This keeps the CLI surface single-shape ("hand the
/// operator's string in, get an id out") and avoids fragmenting the
/// id-vs-slug logic across both layers.
fn resolve_handle(storage: &Storage, handle: &str) -> Result<IssueId, CliError> {
    storage.resolve(handle).map_err(|e| match e {
        StorageError::SlugNotFound { handle } => CliError::SlugNotFound { handle },
        other => CliError::Storage(other),
    })
}

/// Read the issue body per the `-F` flag's contract.
///
/// - `None` — empty body. The epic's "no prompts ever" rule means we do
///   NOT launch an editor when `-F` is omitted; users who want one can
///   pipe it in.
/// - `Some("-")` — read all of stdin, raw bytes. UTF-8 enforced because
///   issue bodies are serialized into a JSON string field.
/// - `Some(<path>)` — read the file, same UTF-8 rule.
fn read_body(file: Option<&Path>) -> Result<String, CliError> {
    let Some(path) = file else {
        return Ok(String::new());
    };
    if path == Path::new("-") {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|error| CliError::BodyRead {
                from: "<stdin>".into(),
                error,
            })?;
        return Ok(buf);
    }
    std::fs::read_to_string(path).map_err(|error| CliError::BodyRead {
        from: path.display().to_string(),
        error,
    })
}

/// `jjf show <id> [--json]` — fetch one issue's structured record from
/// the `issues` bookmark via `Storage::read` and render it.
///
/// The preflight order matches `run_new`: parse the id, resolve the
/// cwd, probe for the jj repo + `issues` bookmark, then hand off to
/// the storage layer. Issue-not-found is a runtime failure (exit 1) —
/// the user typed something well-formed, we tried to honor it, and
/// the answer is "no such issue at the bookmark tip."
fn run_show(json: bool, id: String, include_memories: bool) -> Result<(), CliError> {
    // 1. Resolve the cwd. `Storage::open` wants an absolute path;
    // canonicalize so symlinks don't bite.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 2. Preflight the same checks the write path runs. `run jjf
    // init first` is the right error when the bookmark is missing,
    // not a raw jj-stderr.
    preflight::issues_bookmark(&cwd)?;

    // 3. Open storage and resolve the handle (`id`-or-`slug`).
    let storage = Storage::open(&cwd)?;
    let issue_id = resolve_handle(&storage, &id)?;

    // 4. Hand off to storage. `IssueNotFound` flows out as a `Storage`
    // variant of `CliError`, which `exit_code` maps to 1.
    let issue = storage.read(&issue_id)?;

    // 5. Render.
    if json {
        // The `Issue` struct IS the structured payload — emit it
        // verbatim, no `{"ok": true, ...}` envelope. (`init` and `new`
        // use the envelope because they have no payload beyond a
        // success signal; `show`'s whole job is to expose the record.)
        // `--include-memories` is plain-text only — JSON consumers
        // call `jjf memories --json` for that.
        let s = serde_json::to_string_pretty(&issue)
            .map_err(|e| CliError::Storage(StorageError::Json(e)))?;
        println!("{s}");
    } else {
        print_issue_plain(&issue);
        if include_memories {
            let memories = storage.list_memories()?;
            print_memories_block(&memories);
        }
    }
    Ok(())
}

/// Render a `## Persistent Memories (N)` block after an issue body.
/// Format mirrors beads' non-compact `prime` output
/// (`reference/beads/cmd/bd/prime.go:387-393`): a header with the
/// count, a one-line usage hint, then per-memory `### <key>\n<value>\n`
/// sections in ASCII order by key. Empty memory list prints nothing.
fn print_memories_block(memories: &[Memory]) {
    if memories.is_empty() {
        return;
    }
    println!();
    println!("## Persistent Memories ({})", memories.len());
    println!();
    println!(
        "Stored via `jjf remember`. Update in place with `jjf remember --key <key> \"new content\"`. Search with `jjf memories <keyword>`. Remove with `jjf forget <key>`."
    );
    println!();
    for m in memories {
        println!("### {}", m.key);
        println!("{}", m.value);
        println!();
    }
}

/// `jjf remember "<value>" [--key <slug>] [-F <path|->]` — write a
/// persistent memory to the `issues` bookmark.
///
/// Body source rules mirror `jjf new`'s `-F` convention: a positional
/// `value` is the value verbatim; `-F <path>` reads from a file; `-F -`
/// reads from stdin. Exactly one source must be present; clap enforces
/// the `conflicts_with` between `value` and `file`.
///
/// When `--key` is absent, the key is derived from the value via
/// `slugify`. If the value contains no alphanumerics, slugify returns
/// `""` and we surface a typed `EmptyMemoryKey` error.
fn run_remember(
    json: bool,
    value: Option<String>,
    key: Option<String>,
    file: Option<PathBuf>,
) -> Result<(), CliError> {
    // 1. Resolve the value source.
    let value: String = match (value, file) {
        (Some(v), None) => v,
        (None, Some(path)) => read_body(Some(path.as_path()))?,
        (None, None) => return Err(CliError::MissingMemoryValue),
        (Some(_), Some(_)) => {
            // Clap's `conflicts_with` should prevent this; defensive.
            return Err(CliError::MissingMemoryValue);
        }
    };
    let trimmed = value.trim_end_matches('\n').to_owned();

    // 2. Resolve the key. Explicit --key wins; otherwise slugify the
    // value. An empty slugify result (the value had no alphanumerics)
    // is a typed exit-2 error pointing at --key.
    let key = match key {
        Some(k) => k,
        None => {
            let auto = jjf_storage::slugify(&trimmed);
            if auto.is_empty() {
                return Err(CliError::EmptyMemoryKey {
                    value: trimmed.clone(),
                });
            }
            auto
        }
    };

    // 3. Preflight cwd + bookmark.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;

    // 4. Hand off to storage.
    let storage = Storage::open(&cwd)?;
    let existed = storage.read_memory(&key)?.is_some();
    storage.set_memory(&key, &trimmed)?;

    // 5. Render. `action` is `"remembered"` for the create case and
    // `"updated"` for the upsert case — gives the operator a clear
    // signal which path ran.
    let action = if existed { "updated" } else { "remembered" };
    if json {
        let out = serde_json::json!({
            "ok": true,
            "key": key,
            "action": action,
        });
        println!("{out}");
    } else {
        // Single-line summary using a truncated value, matching beads'
        // shape.
        let preview = truncate_memory(&trimmed, 80);
        let verb = if existed { "Updated" } else { "Remembered" };
        println!("{verb} [{key}]: {preview}");
    }
    Ok(())
}

/// `jjf memories [<search>] [--json]` — list memories, optionally
/// filtered by a case-insensitive substring match across key + value.
///
/// Plain-text shape per the ticket: `<key>\n  <value-truncated>\n` per
/// memory, alphabetical by key, with a header line summarizing the
/// count (or the search term). `--json` emits the bare array of
/// `Memory` records (same envelope rule as `ls --json` / `show
/// --json`).
fn run_memories(json: bool, search: Option<String>) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;
    let storage = Storage::open(&cwd)?;
    let mut memories = storage.list_memories()?;
    if let Some(s) = &search {
        let s = s.to_lowercase();
        memories.retain(|m| {
            m.key.to_lowercase().contains(&s)
                || m.value.to_lowercase().contains(&s)
        });
    }
    if json {
        let payload = serde_json::to_string_pretty(&memories)
            .map_err(|e| CliError::Storage(StorageError::Json(e)))?;
        println!("{payload}");
        return Ok(());
    }
    if memories.is_empty() {
        if let Some(s) = &search {
            println!("no memories matching {s:?}");
        } else {
            println!(
                "no memories stored. Use `jjf remember \"insight\"` to add one."
            );
        }
        return Ok(());
    }
    if let Some(s) = &search {
        println!("memories matching {s:?}:");
    } else {
        println!("memories ({}):", memories.len());
    }
    println!();
    for m in &memories {
        println!("{}", m.key);
        println!("  {}", truncate_memory(&m.value, 120));
        println!();
    }
    Ok(())
}

/// `jjf recall <key> [--json]` — print the full value of one memory.
///
/// Plain-text shape: the value verbatim on stdout (newline-appended),
/// exit 1 with a stderr error if absent. `--json` shape: `{key, value,
/// found}` always, with exit 1 + `found: false` when absent so a
/// pipeline can `jq` either form.
fn run_recall(json: bool, key: String) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;
    let storage = Storage::open(&cwd)?;
    let mem = storage.read_memory(&key)?;
    match mem {
        Some(m) => {
            if json {
                let out = serde_json::json!({
                    "key": m.key,
                    "value": m.value,
                    "found": true,
                });
                println!("{out}");
            } else {
                println!("{}", m.value);
            }
            Ok(())
        }
        None => Err(CliError::MemoryNotFound { key }),
    }
}

/// `jjf forget <key> [--json]` — remove one memory by key.
///
/// Exit 0 with a confirmation on success; exit 1 with `memory_not_found`
/// when the key doesn't exist. The storage layer's `unset_memory`
/// surfaces the "no memory with key" message as `Error::Invalid`; we
/// translate that to the typed `MemoryNotFound` for kind stability.
fn run_forget(json: bool, key: String) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;
    let storage = Storage::open(&cwd)?;
    // Probe up-front so we can surface `MemoryNotFound` rather than
    // storage's generic `Invalid` message.
    if storage.read_memory(&key)?.is_none() {
        return Err(CliError::MemoryNotFound { key });
    }
    storage.unset_memory(&key)?;
    if json {
        let out = serde_json::json!({
            "ok": true,
            "key": key,
            "action": "forgot",
        });
        println!("{out}");
    } else {
        println!("forgot [{key}]");
    }
    Ok(())
}

/// Shorten a memory value to `max_len` for display. Newlines collapse
/// to spaces so the truncated line stays single-line.
fn truncate_memory(s: &str, max_len: usize) -> String {
    let one_line = s.replace('\n', " ");
    if one_line.chars().count() <= max_len {
        return one_line;
    }
    let prefix: String = one_line.chars().take(max_len.saturating_sub(3)).collect();
    format!("{prefix}...")
}

/// Render an issue as human-readable plain text. v1 shape per the
/// `cli-show` ticket — readable and stable, not a contract. If a
/// caller wants machine parsing they should pass `--json`.
fn print_issue_plain(issue: &Issue) {
    let status = issue.status.as_str();
    println!("{}  [{}]", issue.id, status);
    println!("{}", issue.title);
    // v2.5 (`agent-await-gates-impl`): when the issue is parked
    // (`Status::Blocked`), surface the recorded reason on its
    // own line. We show it even when the reason is `None` so the
    // operator gets a clear "(no reason recorded)" signal rather
    // than wondering whether the field is missing or just empty.
    // For non-Blocked statuses we drop the line entirely — a stale
    // reason on an Open issue would be misleading, and the storage
    // layer's `unblock` clears it as part of the transition.
    if issue.status == Status::Blocked {
        let reason = issue
            .block_reason
            .as_deref()
            .unwrap_or("(no reason recorded)");
        println!("block-reason: {reason}");
    }
    // type + slug rendered alongside the rest of the header. type
    // shows the lowercase wire spelling (matches CLI flag values
    // and storage trailers); slug renders as `(none)` when null so
    // it mirrors the other Optional fields' presentation.
    println!("type: {}", issue.type_.as_str());
    let slug = issue.slug.as_deref().unwrap_or("(none)");
    println!("slug: {slug}");
    let labels = if issue.labels.is_empty() {
        "(none)".to_owned()
    } else {
        issue.labels.join(", ")
    };
    println!("labels: {labels}");
    // Metadata block: render between labels and dependencies,
    // mirroring the JSON field ordering (metadata sits after labels in
    // the IssueRecord struct). Omitted entirely when the map is empty.
    if !issue.metadata.is_empty() {
        println!("metadata:");
        for (k, v) in &issue.metadata {
            // BTreeMap iterates in sorted key order — no re-sort needed.
            println!("  {k}={v}");
        }
    }
    // v2.8: priority renders as `P0`..`P4` when set, `(none)` when
    // null — mirrors slug / assignee's Optional-presentation
    // convention rather than the row-format dash (the show output
    // is human-readable; the row format is the awk-friendly one).
    let priority = match issue.priority {
        Some(n) => format!("P{n}"),
        None => "(none)".to_owned(),
    };
    println!("priority: {priority}");
    let assignee = issue.assignee.as_deref().unwrap_or("(none)");
    println!("assignee: {assignee}");
    // v2.4: the dependency section renders one line per kind so the
    // typed-edge model is visible at a glance. Empty kinds are
    // collapsed; an entirely empty dep set falls back to the v1 shape
    // `dependencies: (none)`.
    if issue.dependencies.is_empty() {
        println!("dependencies: (none)");
    } else {
        println!("dependencies:");
        for kind in [
            DepKind::Blocks,
            DepKind::ParentChild,
            DepKind::Related,
            DepKind::DiscoveredFrom,
        ] {
            let targets: Vec<String> = issue
                .dependencies
                .iter()
                .filter(|e| e.kind == kind)
                .map(|e| e.target.as_str().to_owned())
                .collect();
            if !targets.is_empty() {
                // Owner-perspective label here (fix for
                // `show-deps-blocked-by`, fj#2). The wire spelling
                // at `DepKind::as_str` reads inverted to a human in
                // this position: `blocks: B` under issue A reads as
                // "A blocks B" but the storage semantics say "A is
                // blocked until B closes". `as_show_label` flips it.
                // Wire spelling stays in JSON, trailers, CLI flags,
                // `dep tree`, and `dep add`/`dep rm` confirmations.
                println!("  {}: {}", kind.as_show_label(), targets.join(", "));
            }
        }
    }
    println!(
        "created: {}   updated: {}",
        issue.created_at, issue.updated_at
    );
    println!();
    // Body verbatim, no rewrap — the writer preserves bytes exactly,
    // and the reader's job is to show them. Add a trailing newline
    // only if the body doesn't already end with one, so two bodies
    // that differ only in trailing newline still render distinctly.
    if !issue.body.is_empty() {
        print!("{}", issue.body);
        if !issue.body.ends_with('\n') {
            println!();
        }
        println!();
    }
    let n = issue.comments.len();
    println!("--- comments ({n}) ---");
    for c in &issue.comments {
        println!("[{}] {}:", c.created_at, c.author);
        print!("{}", c.body);
        if !c.body.ends_with('\n') {
            println!();
        }
        println!();
    }
}

/// `jjf close <id>` / `jjf open <id>` — flip an issue's status via the
/// storage write path. Both verbs differ only in the `Status` value
/// they pass to `Storage::set_status`, so they share one helper.
///
/// Per the spec (and the `cli-status` ticket): closing an
/// already-closed issue (or opening an already-open one) is NOT a
/// no-op — it lands a fresh `set-status` trailer on a new commit. The
/// storage layer enforces this by always calling `mutate` regardless
/// of whether the record actually changed; we just pass the request
/// through.
///
/// Preflight order matches `run_show`: parse the id (exit 2 on bad
/// shape), resolve the cwd, probe for the jj repo + `issues` bookmark
/// (exit 2 with `run jjf init first` if absent), then hand off to
/// storage. A well-formed id that doesn't exist on the bookmark
/// surfaces as `IssueNotFound` and exits 1.
fn run_set_status(json: bool, id: String, status: Status) -> Result<(), CliError> {
    // 1. Resolve + canonicalize cwd.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 2. Preflight: jj repo + `issues` bookmark present.
    preflight::issues_bookmark(&cwd)?;

    // 3. Open storage, resolve the handle (`id`-or-`slug`), then
    // hand off the mutation.
    let storage = Storage::open(&cwd)?;
    let issue_id = resolve_handle(&storage, &id)?;
    storage.set_status(&issue_id, status)?;

    // 5. Render. The plain-text shape (`closed <id>` / `opened <id>`)
    // is intentionally minimal — one line, no decoration — so it slots
    // cleanly into a shell pipeline. The `--json` envelope mirrors
    // `init` / `new`: `{"ok": true, ...}` plus the verb-specific
    // payload (id + the resulting status).
    let status_word = status.as_str();
    if json {
        let out = serde_json::json!({
            "ok": true,
            "id": issue_id.as_str(),
            "status": status_word,
        });
        println!("{out}");
    } else {
        // Past tense for the human form: `closed <id>` / `opened <id>`.
        // The verb describes the action just performed, not the
        // resulting state — that's `status` in the JSON envelope.
        // `InProgress` is unreachable here (the `close`/`open` verbs
        // are the only callers and they only pass Open/Closed) but we
        // fall through to `as_str` for safety so a future verb that
        // routes through this helper renders sanely.
        let verb = match status {
            Status::Open => "opened",
            Status::Closed => "closed",
            Status::InProgress => "claimed",
            Status::Blocked => "blocked",
            Status::Abandoned => "abandoned",
        };
        println!("{verb} {issue_id}");
    }
    Ok(())
}

/// `jjf block <id> --reason <text>` — park an issue. Sets status to
/// `blocked` and records the (optional) reason in ONE multi-op
/// commit via [`Storage::block`]. v2.5 (`agent-await-gates-impl`).
///
/// Preflight order mirrors `run_set_status`: refuse-self-host, then
/// `issues_bookmark`, then resolve the handle, then hand off to
/// storage. Single-line reason validation is the storage layer's
/// responsibility — newlines in `--reason` come back as a typed
/// `invalid_input` error from storage.
fn run_block(json: bool, id: String, reason: Option<String>) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;

    let storage = Storage::open(&cwd)?;
    let issue_id = resolve_handle(&storage, &id)?;
    storage.block(&issue_id, reason.as_deref())?;

    if json {
        let mut obj = serde_json::Map::new();
        obj.insert("ok".into(), serde_json::Value::Bool(true));
        obj.insert(
            "id".into(),
            serde_json::Value::String(issue_id.as_str().to_owned()),
        );
        obj.insert(
            "status".into(),
            serde_json::Value::String(Status::Blocked.as_str().to_owned()),
        );
        obj.insert(
            "reason".into(),
            match reason.as_deref() {
                Some(r) if !r.trim().is_empty() => {
                    serde_json::Value::String(r.trim().to_owned())
                }
                _ => serde_json::Value::Null,
            },
        );
        obj.insert("blocked".into(), serde_json::Value::Bool(true));
        let out = serde_json::Value::Object(obj);
        println!("{out}");
    } else if let Some(r) = reason.as_deref() {
        let trimmed = r.trim();
        if trimmed.is_empty() {
            println!("blocked {issue_id}");
        } else {
            println!("blocked {issue_id}: {trimmed}");
        }
    } else {
        println!("blocked {issue_id}");
    }
    Ok(())
}

/// `jjf unblock <id>` — unpark an issue. Sets status back to `open`
/// AND clears the `block_reason` in ONE multi-op commit via
/// [`Storage::unblock`]. v2.5 (`agent-await-gates-impl`).
fn run_unblock(json: bool, id: String) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;

    let storage = Storage::open(&cwd)?;
    let issue_id = resolve_handle(&storage, &id)?;
    storage.unblock(&issue_id)?;

    if json {
        let out = serde_json::json!({
            "ok": true,
            "id": issue_id.as_str(),
            "status": Status::Open.as_str(),
            "blocked": false,
        });
        println!("{out}");
    } else {
        println!("unblocked {issue_id}");
    }
    Ok(())
}

/// `jjf assign <id> <name>` — sugar for `jjf update <id>
/// --assignee <name>` / `--unset-assignee`. Modeled on beads'
/// `bd assign` (`reference/beads/cmd/bd/assign.go`); the entire
/// heavy lift lives in [`Storage::set_assignee`].
///
/// Empty `name` (after trim) clears the assignee — same semantic
/// as `--unset-assignee`. A whitespace-only string is treated as
/// empty so the obvious shell-quoting mistake doesn't write a
/// blank-but-non-null assignee.
///
/// Preflight order mirrors `run_set_status`: canonicalize cwd,
/// `issues_bookmark` probe, resolve the handle, hand off to
/// storage. Newlines in `name` come back from the storage layer
/// as a typed `invalid_input` error (exit 1).
fn run_assign(json: bool, id: String, name: String) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;

    let storage = Storage::open(&cwd)?;
    let issue_id = resolve_handle(&storage, &id)?;

    let trimmed = name.trim();
    let assignee_payload: Option<&str> = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    };
    storage.set_assignee(&issue_id, assignee_payload)?;

    if json {
        let assignee_value = match assignee_payload {
            Some(a) => serde_json::Value::String(a.to_owned()),
            None => serde_json::Value::Null,
        };
        let out = serde_json::json!({
            "ok": true,
            "id": issue_id.as_str(),
            "assignee": assignee_value,
        });
        println!("{out}");
    } else if let Some(a) = assignee_payload {
        println!("assigned {issue_id} to {a}");
    } else {
        println!("unassigned {issue_id}");
    }
    Ok(())
}

/// `jjf label add|rm <id> <label>` — flip one label on an issue via
/// the storage write path. Both arms differ only in which `Storage`
/// mutator they call (`add_label` vs `remove_label`) and which
/// past-tense verb they render, so they share one helper.
///
/// Per spec §5.2 (and matching `set-status`'s shape): the call is NOT
/// idempotent at the commit level — re-adding an already-present
/// label, or removing a label that isn't there, still lands a fresh
/// `label-add`/`label-rm` trailer. The in-memory label set is dedup'd
/// by the storage layer so `show` reports a clean list either way.
///
/// Preflight order mirrors `run_set_status`: parse the id (exit 2),
/// reject an empty label (exit 2), canonicalize cwd, probe for the jj
/// repo + `issues` bookmark (exit 2 with `run jjf init first` if
/// absent), then hand off to storage. A well-formed id that doesn't
/// exist on the bookmark surfaces as `IssueNotFound` and exits 1.
fn run_label(json: bool, id: String, label: String, op: LabelOp) -> Result<(), CliError> {
    // 1. Reject empty labels at the CLI layer — storage doesn't
    // validate. We trim before the check because a whitespace-only
    // label is almost certainly the same shell-quoting mistake an
    // empty one would be.
    if label.trim().is_empty() {
        return Err(CliError::EmptyLabel);
    }

    // 2. Resolve + canonicalize cwd.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 3. Preflight: jj repo + `issues` bookmark present.
    preflight::issues_bookmark(&cwd)?;

    // 4. Open storage, resolve handle (`id`-or-`slug`), then hand off.
    let storage = Storage::open(&cwd)?;
    let issue_id = resolve_handle(&storage, &id)?;
    match op {
        LabelOp::Add => storage.add_label(&issue_id, &label)?,
        LabelOp::Rm => storage.remove_label(&issue_id, &label)?,
    }

    // 6. Render. Plain-text shape is `label added: <label> -> <id>` /
    // `label removed: <label> -> <id>` per the ticket — verb-first and
    // past-tense matches `closed <id>` / `opened <id>`. The arrow
    // visually separates the two values so a reader can scan
    // `<label>` and `<id>` without parsing word position.
    let action_word = match op {
        LabelOp::Add => "added",
        LabelOp::Rm => "removed",
    };
    if json {
        let out = serde_json::json!({
            "ok": true,
            "id": issue_id.as_str(),
            "label": &label,
            "action": action_word,
        });
        println!("{out}");
    } else {
        println!("label {action_word}: {label} -> {issue_id}");
    }
    Ok(())
}

/// `jjf metadata set|unset <id> <key> [<value>]` — wrap
/// `Storage::set_metadata` / `Storage::unset_metadata`. Mirrors
/// [`run_label`]: reject an empty key at the CLI layer (exit 2),
/// canonicalize cwd, probe for the `issues` bookmark, resolve the
/// handle (id-or-slug), then hand off to storage. The `value` is
/// `Some` for `set` and `None` for `unset`.
fn run_metadata(
    json: bool,
    id: String,
    key: String,
    value: Option<String>,
    op: MetadataOp,
) -> Result<(), CliError> {
    // 1. Reject empty keys at the CLI layer — storage doesn't validate
    // emptiness. Trim before the check (whitespace-only key is the
    // same shell-quoting mistake an empty one would be).
    if key.trim().is_empty() {
        return Err(CliError::EmptyMetadataKey);
    }

    // 2. Resolve + canonicalize cwd.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 3. Preflight: jj repo + `issues` bookmark present.
    preflight::issues_bookmark(&cwd)?;

    // 4. Open storage, resolve handle (`id`-or-`slug`), then hand off.
    let storage = Storage::open(&cwd)?;
    let issue_id = resolve_handle(&storage, &id)?;
    match op {
        MetadataOp::Set => {
            let v = value.as_deref().unwrap_or("");
            storage.set_metadata(&issue_id, &key, v)?
        }
        MetadataOp::Unset => storage.unset_metadata(&issue_id, &key)?,
    }

    // 5. Render. Same `{"ok":true,…}` envelope shape as `run_label`.
    let action_word = match op {
        MetadataOp::Set => "set",
        MetadataOp::Unset => "unset",
    };
    if json {
        let mut out = serde_json::json!({
            "ok": true,
            "id": issue_id.as_str(),
            "key": &key,
            "action": action_word,
        });
        if let Some(v) = &value {
            out["value"] = serde_json::Value::String(v.clone());
        }
        println!("{out}");
    } else {
        match &value {
            Some(v) => println!("metadata {action_word}: {key}={v} -> {issue_id}"),
            None => println!("metadata {action_word}: {key} -> {issue_id}"),
        }
    }
    Ok(())
}

/// `jjf dep add|rm <child> <parent> [--kind <kind>]` — wrap
/// `Storage::add_dep_edge` / `Storage::remove_dep_edge`. v2.4
/// (`agent-dep-types`). Preflight mirrors `run_label`: refuse to run
/// from the source repo, probe for the `issues` bookmark, resolve
/// both handles (id-or-slug). The edge kind defaults to `blocks` at
/// the clap layer; the storage call lands a `dep-add` / `dep-rm` op
/// with the `Jjf-Dep-Kind:` trailer.
fn run_dep(
    json: bool,
    child: String,
    parent: String,
    kind: DepKind,
    op: DepOp,
) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;

    let storage = Storage::open(&cwd)?;
    let child_id = resolve_handle(&storage, &child)?;
    let parent_id = resolve_handle(&storage, &parent)?;
    match op {
        DepOp::Add => storage.add_dep_edge(&child_id, &parent_id, kind)?,
        DepOp::Rm => storage.remove_dep_edge(&child_id, &parent_id, kind)?,
    }

    let action_word = match op {
        DepOp::Add => "added",
        DepOp::Rm => "removed",
    };
    if json {
        let out = serde_json::json!({
            "ok": true,
            "child": child_id.as_str(),
            "parent": parent_id.as_str(),
            "kind": kind.as_str(),
            "action": action_word,
        });
        println!("{out}");
    } else {
        println!(
            "dep {action_word}: {} {} -> {}",
            kind.as_str(),
            child_id,
            parent_id
        );
    }
    Ok(())
}

/// `jjf dep tree <id>` — print the parent-child tree rooted at `<id>`.
/// v2.4 (`agent-dep-types`). Read-only verb; no source-repo guard
/// needed.
fn run_dep_tree(json: bool, id: String) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;

    let storage = Storage::open(&cwd)?;
    let root_id = resolve_handle(&storage, &id)?;
    let tree = storage.dep_tree(&root_id)?;
    if json {
        let payload = serde_json::to_string(&tree)
            .expect("DepTree serializes — derive contract");
        println!("{payload}");
    } else {
        render_dep_tree_text(&tree.root, 0);
    }
    Ok(())
}

/// Indent-by-2-spaces text rendering of a `DepTree`. Each level
/// shows `<id> <status> <title>`; a cycled node carries a `(cycle)`
/// suffix and stops recursing.
fn render_dep_tree_text(node: &DepTreeNode, depth: usize) {
    let indent = "  ".repeat(depth);
    let cycle_suffix = if node.cycle { " (cycle)" } else { "" };
    println!(
        "{}{} [{}] {}{}",
        indent,
        node.id,
        node.status.as_str(),
        node.title,
        cycle_suffix
    );
    for child in &node.children {
        render_dep_tree_text(child, depth + 1);
    }
}

/// `jjf remote add <name> <url>` — wrap `git remote add <name> <url>`
/// against the cwd's git repo.
///
/// git does the actual remote-add work; we translate the two specific
/// error stderrs we recognize (`already exists`, anything else) into
/// typed `CliError` variants so `kind()` stays stable. URL syntax
/// validation is git's responsibility — we accept what it accepts and
/// surface its rejection unchanged.
///
/// Preflight is the repo-existence check only (no `issues` bookmark
/// required), because adding a remote is meaningful before `jjf init`
/// runs.
///
/// After git registers the remote we also add the v3 fetch refspec
/// (`+refs/jjf/*:refs/remotes/<name>/jjf/*`) to `.git/config`. Without
/// this, a plain `git fetch <name>` carries refs under `refs/heads/*`
/// only and leaves the jjforge namespace empty on the new clone (see
/// ticket `eaf0674`).
fn run_remote_add(json: bool, name: String, url: String) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::jj_repo(&cwd)?;

    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&cwd)
        .args(["remote", "add", &name, &url])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // git's phrase: `error: remote <name> already exists.`
        if stderr.contains("already exists") {
            return Err(CliError::RemoteAlreadyExists(name));
        }
        return Err(CliError::JjGitRemote(stderr.trim().to_owned()));
    }

    // Best-effort: append the v3 fetch refspec. Failures here are not
    // fatal (the remote IS added; the user can still `jjf pull` since
    // that uses an explicit refspec), but they're worth surfacing as a
    // hint.
    let _ = ensure_jjf_fetch_refspec(&cwd, &name);

    if json {
        let out = serde_json::json!({
            "ok": true,
            "name": &name,
            "url": &url,
        });
        println!("{out}");
    } else {
        println!("remote {name} added: {url}");
    }
    Ok(())
}

/// Iterate every git remote configured on `<cwd>` and call
/// [`ensure_jjf_fetch_refspec`] for each. Best-effort — individual
/// failures are logged-by-ignored; the caller (`jjf init`) doesn't
/// want a stale refspec write to break init.
fn backfill_fetch_refspec_for_all_remotes(cwd: &Path) -> std::io::Result<()> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["remote"])
        .output()?;
    if !out.status.success() {
        return Ok(());
    }
    for name in String::from_utf8_lossy(&out.stdout).lines() {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let _ = ensure_jjf_fetch_refspec(cwd, name);
    }
    Ok(())
}

/// Ensure `+refs/jjf/*:refs/remotes/<remote>/jjf/*` is configured as a
/// fetch refspec for `<remote>` in `<cwd>/.git/config`. Idempotent:
/// re-runs are no-ops once the refspec is present.
///
/// Without this refspec, a plain `git fetch <remote>` only pulls
/// `refs/heads/*`, so the jjforge `refs/jjf/*` namespace stays empty on
/// a fresh clone — `jjf ls` then errors with "run jjf init first" even
/// though the remote has every ref the local repo needs (ticket
/// `eaf0674`). `jjf pull` itself uses an explicit refspec and works
/// regardless, but downstream tooling (and curious users running raw
/// git) expects fetch to carry the namespace.
fn ensure_jjf_fetch_refspec(cwd: &Path, remote: &str) -> std::io::Result<()> {
    let key = format!("remote.{}.fetch", remote);
    let value = format!("+refs/jjf/*:refs/remotes/{}/jjf/*", remote);

    // Check if this exact value is already present. `git config
    // --get-all <key>` lists every value; bail if any equals our
    // target.
    let probe = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--get-all", &key])
        .output()?;
    if probe.status.success() {
        let existing = String::from_utf8_lossy(&probe.stdout);
        if existing.lines().any(|line| line.trim() == value) {
            return Ok(());
        }
    }

    // Append (don't replace). `git config --add` appends a new line to
    // a multi-valued config key; the standard heads-only fetch refspec
    // jj wrote at clone time stays in place.
    let add = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--add", &key, &value])
        .output()?;
    if !add.status.success() {
        return Err(std::io::Error::other(format!(
            "git config --add {} {}: {}",
            key,
            value,
            String::from_utf8_lossy(&add.stderr)
        )));
    }
    Ok(())
}

/// `jjf remote ls` — wrap `git remote -v` and re-render its output as
/// tab-separated `<name>\t<url>` lines.
///
/// `git remote -v` prints two lines per remote (fetch + push); we
/// dedupe to fetch-only and re-render because every other `ls`-style
/// verb in jjforge emits tab-separated columns, and a stable separator
/// means downstream `cut -f1` / `awk -F'\t'` pipelines don't have to
/// guess at column widths.
///
/// `--json` emits a JSON array of `{name, url}` objects. Empty result
/// is `[]` (per the same `ls` / `show` convention — scripts piping to
/// `jq length` get a useful value), and empty plain-text output is
/// silence (zero lines), not a header.
fn run_remote_ls(json: bool) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::jj_repo(&cwd)?;

    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&cwd)
        .args(["remote", "-v"])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        return Err(CliError::JjGitRemote(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    // `git remote -v` prints two lines per remote:
    //   <name>\t<url> (fetch)
    //   <name>\t<url> (push)
    // We dedupe by collecting only the fetch lines (those ending with
    // ` (fetch)`), which appear first for each remote. Using a
    // Vec to preserve insertion order.
    let mut seen = std::collections::HashSet::new();
    let remotes: Vec<(String, String)> = stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || !line.ends_with("(fetch)") {
                return None;
            }
            // Format: `<name>\t<url> (fetch)`
            let (name, rest) = line.split_once('\t')?;
            // Strip trailing " (fetch)"
            let url = rest.trim_end_matches("(fetch)").trim();
            if seen.insert(name.to_owned()) {
                Some((name.to_owned(), url.to_owned()))
            } else {
                None
            }
        })
        .collect();

    if json {
        let arr: Vec<serde_json::Value> = remotes
            .iter()
            .map(|(name, url)| {
                serde_json::json!({
                    "name": name,
                    "url": url,
                })
            })
            .collect();
        let s = serde_json::to_string_pretty(&arr)
            .map_err(|e| CliError::Storage(StorageError::Json(e)))?;
        println!("{s}");
    } else {
        for (name, url) in &remotes {
            println!("{name}\t{url}");
        }
    }
    Ok(())
}

/// `jjf remote rm <name>` — wrap `git remote remove <name>`.
///
/// Preflight + error mapping mirror `run_remote_add`. Stderr matching
/// on git's `No such remote` phrase (`error: No such remote: '<name>'`
/// / `fatal: No such remote ...`) is the typed `RemoteNotFound`
/// (exit 2); anything else falls through to `JjGitRemote` (exit 1).
fn run_remote_rm(json: bool, name: String) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::jj_repo(&cwd)?;

    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&cwd)
        .args(["remote", "remove", &name])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // git's phrase: `error: No such remote: '<name>'` or
        // `fatal: No such remote ...`
        if stderr.contains("No such remote") {
            return Err(CliError::RemoteNotFound(name));
        }
        return Err(CliError::JjGitRemote(stderr.trim().to_owned()));
    }

    if json {
        let out = serde_json::json!({
            "ok": true,
            "name": &name,
        });
        println!("{out}");
    } else {
        println!("remote {name} removed");
    }
    Ok(())
}

/// `jjf update <id> [--title T] [--status S] [--body-file PATH|-]
/// [--assignee NAME] [--unset-assignee] [--json]` — mutate one or more
/// scalar fields of an issue in a single commit.
///
/// All populated field flags bundle into ONE `Storage::update` call,
/// which lands ONE new commit on the `issues` bookmark carrying N
/// `Jjf-Op:` trailers (one per field that changed). This is the
/// multi-op-per-commit dividend the spec §5.5 gives us — running three
/// sibling verbs (e.g. `set-title` + `close` + a separate body update)
/// would fragment into three commits instead.
///
/// Preflight order matches the other write verbs (`run_set_status`,
/// `run_label`, `run_comment`): purely-local validation first
/// (id parse, at-least-one-flag rule, body-file read), then
/// canonicalize cwd, then probe for the jj repo + `issues` bookmark.
/// Issue-not-found surfaces from `Storage::update` as an
/// `IssueNotFound` (exit 1) because the user typed a well-formed id;
/// everything else the user can mistype is exit 2.
///
/// `--assignee` / `--unset-assignee` mutual exclusion is enforced by
/// clap via `conflicts_with`. The at-least-one-flag rule has no clap
/// equivalent (every flag is `Option<_>` or `bool`), so we check it
/// here and surface a typed `NoUpdateFields` (exit 2).
fn run_update(
    json: bool,
    id: String,
    title: Option<String>,
    status: Option<StatusArg>,
    type_arg: Option<TypeArg>,
    slug: Option<String>,
    unset_slug: bool,
    priority: Option<u8>,
    unset_priority: bool,
    body_file: Option<PathBuf>,
    assignee: Option<String>,
    unset_assignee: bool,
    claim: bool,
    unclaim: bool,
    actor: Option<String>,
) -> Result<(), CliError> {
    // 1. Build the `UpdateFields` bundle from the flag matrix. The
    // body-file read is done UP FRONT (before the at-least-one check,
    // and before the bookmark probe) so a bogus `--body-file` path
    // surfaces as a typed `BodyRead` error rather than getting masked
    // by a subsequent failure. `--assignee X` => `Some(Some(X))`;
    // `--unset-assignee` => `Some(None)`; neither => `None` (leave
    // alone) — the storage-side `UpdateFields::assignee` is double-
    // wrapped exactly to express this three-way distinction. The
    // same shape applies to `--slug` / `--unset-slug` and to
    // `--type` (which has no unset variant; setting it to a value
    // is the only path, since omitting it leaves the field alone
    // and a `--type unspecified` request collapses to a `Some(None)`
    // wrapper that storage maps back to the default).
    let body = match body_file.as_deref() {
        Some(path) => Some(read_body(Some(path))?),
        None => None,
    };
    let assignee_field: Option<Option<String>> = if unset_assignee {
        Some(None)
    } else {
        assignee.map(Some)
    };
    let slug_field: Option<Option<String>> = if unset_slug {
        Some(None)
    } else {
        slug.map(Some)
    };
    let priority_field: Option<Option<u8>> = if unset_priority {
        Some(None)
    } else {
        priority.map(Some)
    };
    // Pre-validate the title at the CLI boundary so the operator
    // sees the typed exit-2 error before any IO. Storage will
    // re-validate. `qa-title-validation` (issue `e4e483b`).
    if let Some(title) = &title {
        if let Err(reason) = jjf_storage::validate_title(title) {
            return Err(CliError::InvalidTitle {
                title: title.clone(),
                reason,
            });
        }
    }

    // Pre-validate the slug at the CLI boundary so the operator
    // sees the typed exit-2 error before any IO. Storage will
    // re-validate.
    if let Some(Some(slug)) = &slug_field {
        if let Err(reason) = jjf_storage::validate_slug(slug) {
            return Err(CliError::InvalidSlug {
                slug: slug.clone(),
                reason,
            });
        }
    }
    // Pre-validate the body cap at the CLI boundary. Issue
    // `679444a` (QA red-team 2026-06-25 sub-pass 4 C3).
    if let Some(body) = &body {
        if let Err(reason) = jjf_storage::validate_body(body) {
            return Err(CliError::InvalidBody { reason });
        }
    }
    // Pre-validate the priority value (v2.8). Clap's range check
    // catches obviously-wrong CLI input; this is the storage-side
    // contract echoed at the CLI boundary so the typed error
    // surfaces before any IO.
    if let Some(Some(p)) = priority_field {
        if let Err(reason) = validate_priority(Some(p)) {
            return Err(CliError::InvalidPriority { reason });
        }
    }
    let fields = UpdateFields {
        title,
        slug: slug_field,
        status: status.map(Status::from),
        type_: type_arg.map(|t| Some(IssueType::from(t))),
        priority: priority_field,
        body,
        assignee: assignee_field,
    };

    // 2. At-least-one-flag rule. Clap can't enforce this (every flag
    // is `Option<_>` / `bool`), so we surface a typed exit-2 hint
    // pointing at the available flags. `--claim` and `--unclaim`
    // count as "something to do" even though they don't populate
    // `UpdateFields`; they route through `Storage::claim` /
    // `Storage::unclaim` directly.
    if fields.is_empty() && !claim && !unclaim {
        return Err(CliError::NoUpdateFields);
    }

    // 3. Resolve + canonicalize cwd.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 4. Preflight: jj repo + `issues` bookmark present.
    preflight::issues_bookmark(&cwd)?;

    // 5. Open storage, resolve handle.
    let storage = Storage::open(&cwd)?;
    let issue_id = resolve_handle(&storage, &id)?;

    // 6a. `--claim` / `--unclaim` take the direct storage path. Clap
    // already enforces mutual exclusion with the field-level flags
    // (status/assignee/unset-assignee), so by the time we land here
    // `fields.is_empty()` is true and the only branch a user could
    // possibly want is the atomic claim verb.
    if claim {
        let who = resolve_current_user(actor.as_deref())?;
        storage.claim(&issue_id, &who)?;
        if json {
            let out = serde_json::json!({
                "ok": true,
                "id": issue_id.as_str(),
                "assignee": who,
                "status": Status::InProgress.as_str(),
                "claimed": true,
            });
            println!("{out}");
        } else {
            println!("claimed {issue_id} by {who}");
        }
        return Ok(());
    }
    if unclaim {
        storage.unclaim(&issue_id)?;
        if json {
            let out = serde_json::json!({
                "ok": true,
                "id": issue_id.as_str(),
                "status": Status::Open.as_str(),
                "claimed": false,
            });
            println!("{out}");
        } else {
            println!("unclaimed {issue_id}");
        }
        return Ok(());
    }

    // 6b. Field-update path. One call lands one commit with N
    // trailers.
    storage.update(&issue_id, fields.clone())?;

    // 7. Render. The list of field names mirrors the populated fields
    // in field-declaration order (matching the trailer order the
    // storage layer lands). We compute it once so plain-text and JSON
    // agree exactly.
    let changed = changed_field_names(&fields);
    if json {
        let out = serde_json::json!({
            "ok": true,
            "id": issue_id.as_str(),
            "fields": changed,
        });
        println!("{out}");
    } else {
        println!("updated {issue_id}: {}", changed.join(", "));
    }
    Ok(())
}

/// Enumerate the field-name strings for the populated fields of an
/// `UpdateFields`, in field-declaration order. Used to render both the
/// plain-text and `--json` outputs of `jjf update` so they list the
/// same set of names in the same order — and the same order the
/// storage layer's trailers appear in on the resulting commit.
fn changed_field_names(fields: &UpdateFields) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    if fields.title.is_some() {
        out.push("title");
    }
    if fields.slug.is_some() {
        out.push("slug");
    }
    if fields.status.is_some() {
        out.push("status");
    }
    if fields.type_.is_some() {
        out.push("type");
    }
    if fields.priority.is_some() {
        out.push("priority");
    }
    if fields.body.is_some() {
        out.push("body");
    }
    if fields.assignee.is_some() {
        out.push("assignee");
    }
    out
}

/// `jjf comment <id> -F <path|-> [--author <NAME>] [--json]` — append
/// one comment to an existing issue via the storage write path.
///
/// Preflight order mirrors `run_set_status`: parse the id, read the
/// body, resolve the author, canonicalize cwd, probe for the jj repo +
/// `issues` bookmark, then hand off to storage. We deliberately do the
/// purely-local checks (id parse, body read, author resolve) BEFORE
/// shelling out for the bookmark probe so a user typo doesn't kick off
/// a `jj` subprocess that we'd just throw away.
///
/// The storage layer returns the freshly-generated comment id; the
/// `--json` envelope surfaces it as `comment_id`. Plain-text output is
/// `comment added to <id>` — one line, no decoration — to slot cleanly
/// into a shell pipeline.
fn run_comment(
    json: bool,
    id: String,
    file: PathBuf,
    author: Option<String>,
) -> Result<(), CliError> {
    // 1. Read the body. `-F -` is stdin; `-F <path>` is the file.
    // Reuse the same helper `run_new` uses so the contract stays
    // consistent across verbs.
    let body = read_body(Some(file.as_path()))?;
    if body.is_empty() {
        return Err(CliError::EmptyCommentBody);
    }
    // Pre-validate the comment body cap at the CLI boundary. Same
    // 65,536-byte limit as issue bodies (same shape, same on-disk
    // risk). Issue `679444a`.
    if let Err(reason) = jjf_storage::validate_body(&body) {
        return Err(CliError::InvalidBody { reason });
    }

    // 2. Resolve + canonicalize cwd.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 3. Preflight: jj repo + `issues` bookmark present. We run this
    // BEFORE author resolution so a non-jj cwd surfaces the typed
    // "not a jj repo" error rather than the (correct but less useful)
    // "no comment author available" — the user almost always wants to
    // hear about the repo problem first.
    preflight::issues_bookmark(&cwd)?;

    // 5. Resolve the author. CLI override wins; otherwise we synthesize
    // `Name <email>` from jj's user config. If neither path yields a
    // non-empty string we bail with a typed hint rather than letting
    // the storage layer surface a generic `Invalid` error.
    let author = resolve_author(author)?;

    // 6. Open storage, resolve handle (`id`-or-`slug`), then hand off.
    // `add_comment` returns the freshly-minted comment id (a 7-hex
    // `IssueId`) for the JSON envelope.
    let storage = Storage::open(&cwd)?;
    let issue_id = resolve_handle(&storage, &id)?;
    let comment_id = storage.add_comment(&issue_id, &body, &author)?;

    // 7. Render.
    if json {
        let out = serde_json::json!({
            "ok": true,
            "id": issue_id.as_str(),
            "comment_id": comment_id.as_str(),
        });
        println!("{out}");
    } else {
        println!("comment added to {issue_id}");
    }
    Ok(())
}

/// Resolve the current user's name for `--claim`. Precedence chain
/// (each slot, when present and non-empty after trimming, wins;
/// otherwise we fall through to the next):
///
/// 1. `actor_override` — the `--actor <name>` CLI flag on
///    `jjf update`. Empty / whitespace falls through (per the
///    `actor-override-chain` ticket: `--actor ""` is "skip me," not
///    "set empty assignee").
/// 2. `JJF_ACTOR` env var. Same emptiness rule as the flag.
/// 3. `git config user.name` (J2: was `jj config get user.name`).
///
/// Returns [`CliError::NoCurrentUser`] when the chain runs dry.
/// Differs from [`resolve_author`] in that it doesn't synthesize
/// `Name <email>` — claims are short identity strings stored in
/// `assignee`, not authorship strings stored in `comments.jsonl`.
/// v2.3 (`agent-claim-atomic`), chain extended v2.12
/// (`actor-override-chain`, ticket `ae0866b`).
fn resolve_current_user(actor_override: Option<&str>) -> Result<String, CliError> {
    if let Some(name) = actor_override {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_owned());
        }
    }
    if let Some(name) = jjf_actor_env() {
        return Ok(name);
    }
    let name = git_config_get("user.name")?;
    match name {
        Some(n) => Ok(n),
        None => Err(CliError::NoCurrentUser),
    }
}

/// Read `JJF_ACTOR` from the environment, returning the trimmed
/// value when present and non-empty. Empty / whitespace-only values
/// behave as "unset" so a stray `JJF_ACTOR=` in a parent process
/// doesn't override the next-slot fallback. v2.12
/// (`actor-override-chain`).
fn jjf_actor_env() -> Option<String> {
    match std::env::var("JJF_ACTOR") {
        Ok(v) => {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        }
        Err(_) => None,
    }
}

/// Resolve the comment author. Precedence chain (each slot, when
/// present and non-empty after trimming, wins; otherwise we fall
/// through to the next):
///
/// 1. `override_name` — the `--author` CLI flag on `jjf comment`,
///    written verbatim (caller is responsible for formatting it
///    `Name <email>` if they want that shape).
/// 2. `JJF_ACTOR` env var, synthesized as `$JJF_ACTOR <user.email>`
///    (or just `$JJF_ACTOR` if `user.email` is unset). v2.12
///    (`actor-override-chain`).
/// 3. `git config user.name` + `user.email` synthesized as
///    `Name <email>` (or just `name` if `user.email` is unset). J2:
///    was `jj config user.name`.
///
/// Format matches jj's `author` commit-template field (`Name <email>`)
/// so a comment author and the surrounding commit's `author` line stay
/// canonically identical for history walks.
///
/// Edge cases:
/// - Override is empty / whitespace → falls through to the next
///   slot (matches the `--actor` chain semantics).
/// - `JJF_ACTOR` is set but empty / whitespace → falls through.
/// - `user.name` is unset (or empty) and the prior slots fell
///   through → `MissingAuthor`.
/// - `user.name` is set but `user.email` is unset → return just the
///   `name`. This matches the spirit of jj's own behavior (it'll let
///   you commit with just a name) but means the resulting author
///   string won't have the `<email>` suffix that `read_history`'s
///   per-commit `author` typically carries. Worth a follow-up to
///   canonicalize one way or the other.
fn resolve_author(override_name: Option<String>) -> Result<String, CliError> {
    if let Some(name) = override_name {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_owned());
        }
    }
    if let Some(actor) = jjf_actor_env() {
        let email = git_config_get("user.email")?;
        return Ok(match email {
            Some(email) => format!("{actor} <{email}>"),
            None => actor,
        });
    }
    let name = git_config_get("user.name")?;
    let Some(name) = name else {
        return Err(CliError::MissingAuthor);
    };
    let email = git_config_get("user.email")?;
    Ok(match email {
        Some(email) => format!("{name} <{email}>"),
        None => name,
    })
}

/// Shell out to `git config <key>` and return the trimmed value, or
/// `None` if the key isn't configured. Any spawn failure surfaces as
/// a `Probe` error.
///
/// `git config <key>` exits non-zero when the key is absent — we treat
/// that specific case as "not configured" rather than a hard probe
/// failure so the caller can decide what to do. J2 (jj-divorce):
/// replaces `jj_config_get` so actor-identity resolution has no jj
/// dependency.
fn git_config_get(key: &str) -> Result<Option<String>, CliError> {
    let out = std::process::Command::new("git")
        .args(["config", key])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        // `git config` exits non-zero when the key is absent. Treat any
        // non-success here as "not configured" — the verb falls back
        // accordingly.
        return Ok(None);
    }
    let val = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if val.is_empty() { Ok(None) } else { Ok(Some(val)) }
}

/// Cap on the number of unreadable-ref names listed inline in a
/// stderr warning. Beyond this we elide with `… and N more` so an
/// operator with a corrupted-ref pile-up still gets one screen of
/// warning instead of a paragraph.
const UNREADABLE_REFS_INLINE_CAP: usize = 5;

/// Emit a stderr warning naming the unreadable refs the snapshot
/// cache surfaced from this `Storage` instance. No-op when the list
/// is empty.
///
/// Plain-text shape (`json = false`):
/// ```text
/// jjf: warning: 2 ref(s) unreadable: refs/jjf/issues/eed62d7,
///   refs/jjf/memories/foo (skipped from listing)
/// ```
///
/// JSON shape (`json = true`): one single-line JSON envelope on
/// stderr, leaving stdout pristine for the machine-readable result.
/// Per the ticket: keep stdout's bare-array shape stable rather than
/// wrapping the envelope (which would break existing `--json`
/// callers). Shape:
/// ```json
/// {"warning":"unreadable_refs","count":2,
///  "refs":["refs/jjf/issues/eed62d7","refs/jjf/memories/foo"]}
/// ```
///
/// The `refs` array always carries the full list (no inline cap)
/// under `--json` — machines consume it; the cap only applies to
/// the human-formatted plain text. Ticket `4928ae6`.
fn emit_unreadable_warning(unreadable: &[UnreadableRef], json: bool) {
    if unreadable.is_empty() {
        return;
    }
    if json {
        let refs: Vec<&str> = unreadable.iter().map(|u| u.name.as_str()).collect();
        let envelope = serde_json::json!({
            "warning": "unreadable_refs",
            "count": unreadable.len(),
            "refs": refs,
        });
        eprintln!("{envelope}");
        return;
    }
    let count = unreadable.len();
    let shown = unreadable.len().min(UNREADABLE_REFS_INLINE_CAP);
    let names: Vec<&str> = unreadable
        .iter()
        .take(shown)
        .map(|u| u.name.as_str())
        .collect();
    let tail = if count > shown {
        format!(", ... and {} more", count - shown)
    } else {
        String::new()
    };
    eprintln!(
        "jjf: warning: {count} ref(s) unreadable: {names}{tail} (skipped from listing)",
        names = names.join(", ")
    );
}

/// `jjf ls [--status <S>] [--label <L>...] [--json]` — enumerate every
/// issue on the `issues` bookmark, filter by status and labels (AND
/// across labels), render newest-first.
///
/// Implementation strategy is the v1 "read all, filter in memory" path
/// the ticket calls out: `Storage::list_ids()` returns every id, then
/// we `Storage::read()` each one and apply the predicates. For repos
/// with a handful of issues this is fine; once N gets meaningfully
/// large the storage layer will grow either a filtered enumeration
/// primitive or a per-issue metadata cache (separate ticket). The
/// closing comment on this issue calls out the perf feel.
fn run_ls(
    json: bool,
    status: StatusFilter,
    labels: Vec<String>,
    meta: Vec<(String, String)>,
    types: Vec<TypeArg>,
    slug: Option<String>,
    priorities: Vec<u8>,
    parent: Option<String>,
) -> Result<(), CliError> {
    // Preflight: cwd is a jj repo AND `issues` bookmark exists. Same
    // order as `run_show` — typed `run jjf init first` message rather
    // than raw jj stderr if the bookmark is missing.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;

    let storage = Storage::open(&cwd)?;
    let ids = storage.list_ids()?;
    let wanted_types: Vec<IssueType> =
        types.into_iter().map(IssueType::from).collect();

    let parent_id: Option<IssueId> = match parent {
        Some(handle) => {
            let resolved = resolve_handle(&storage, &handle)?;
            // Force an existence check: `resolve_handle` short-circuits
            // on any well-formed 7-char hex id without confirming it
            // matches an issue, so a typoed hex would silently filter
            // to nothing. A read surfaces `issue_not_found` (exit 1),
            // matching the contract of `jjf show <bad-hex>`.
            storage.read(&resolved)?;
            Some(resolved)
        }
        None => None,
    };

    // Read every issue, filter. v1 is read-all; see the doc-comment.
    let mut issues: Vec<Issue> = Vec::with_capacity(ids.len());
    for id in &ids {
        let issue = storage.read(id)?;
        if !status_matches(&issue, status) {
            continue;
        }
        if !labels_match(&issue, &labels) {
            continue;
        }
        if !metadata_matches(&issue, &meta) {
            continue;
        }
        if !types_match(&issue, &wanted_types) {
            continue;
        }
        if !parent_matches(&issue, parent_id.as_ref()) {
            continue;
        }
        if !slug_matches(&issue, slug.as_deref()) {
            continue;
        }
        if !priorities_match(&issue, &priorities) {
            continue;
        }
        issues.push(issue);
    }

    // Newest-first by created_at. RFC 3339 second-resolution stamps
    // sort lexicographically — same trick the read path uses for
    // comments. `created_at` is set once at create and never bumped,
    // so the ordering is stable across mutation traffic.
    issues.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    if json {
        // Array of `Issue` records, pretty-printed. Same per-element
        // shape `show --json` emits — callers parsing one parse the
        // other. Empty result is a valid empty array `[]`, not silence,
        // because a script expecting JSON wants something it can
        // `jq length` against. (Plain text uses silence-on-empty
        // because grep / awk pipelines want zero lines, not a JSON
        // literal.)
        let s = serde_json::to_string_pretty(&issues)
            .map_err(|e| CliError::Storage(StorageError::Json(e)))?;
        println!("{s}");
    } else {
        // Plain text: tab-separated, no header, silent on empty. The
        // 7-char id prefix is the documented human-display convention
        // (CLAUDE.md).
        //
        // v2.8 row (326bbf7): <id>\t<status>\t<priority>\t<type>\t<title>.
        // Priority renders as `P0`..`P4` when set, single dash `-`
        // when null — keeps the column always populated so awk/cut
        // parsing stays trivial.
        for issue in &issues {
            let status_s = issue.status.as_str();
            let type_s = issue.type_.as_str();
            let priority_s = format_priority_column(issue.priority);
            println!(
                "{id}\t{status}\t{priority}\t{type_}\t{title}",
                id = issue.id,
                status = status_s,
                priority = priority_s,
                type_ = type_s,
                title = issue.title,
            );
        }
    }

    // Surface any refs the snapshot cache couldn't parse (e.g. a
    // `refs/jjf/issues/<id>` pointed at a non-commit object). Stdout
    // remains the survivor set; stderr names the casualties so an
    // operator can tell silent corruption apart from "no such issue".
    // Ticket `4928ae6`.
    let unreadable = storage.unreadable_refs()?;
    emit_unreadable_warning(&unreadable, json);

    Ok(())
}

/// `jjf ready [--label L...] [--type T...] [--limit N] [--json]`
/// — list the open issues whose dependencies are all closed (the
/// agent-ready set), sorted by type priority then created_at
/// ascending.
///
/// This is the headline agent-ergonomics verb. `jjf ready --limit 1
/// --json` is the canonical orchestrator-loop call: one unblocked
/// issue, machine-readable, ready to feed into the next action.
///
/// Preflight matches `run_ls` exactly — read verb, no
/// self-host-write guard. The filter/sort logic lives in
/// `Storage::list_ready`; this fn is just the clap → storage →
/// render plumbing.
fn run_ready(
    json: bool,
    labels: Vec<String>,
    types: Vec<TypeArg>,
    limit: Option<usize>,
    include_claimed: bool,
    include_blocked: bool,
    claim: bool,
    priorities: Vec<u8>,
    parent: Option<String>,
    meta: Vec<(String, String)>,
) -> Result<(), CliError> {
    // Preflight: --claim only composes with --limit 1. Reject any
    // other shape up front so callers don't quietly claim the first
    // of N candidates and forget the rest.
    if claim {
        match limit {
            Some(1) => {}
            _ => return Err(CliError::ClaimRequiresLimitOne),
        }
    }

    // Preflight: cwd is a jj repo AND `issues` bookmark exists.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;

    let storage = Storage::open(&cwd)?;

    let parent_id: Option<IssueId> = match parent {
        Some(handle) => {
            let resolved = resolve_handle(&storage, &handle)?;
            // Force an existence check: a well-formed-but-nonexistent
            // 7-char hex id would otherwise filter to nothing silently.
            // See `run_ls` for the same pattern.
            storage.read(&resolved)?;
            Some(resolved)
        }
        None => None,
    };

    let filter = ReadyFilter {
        labels,
        types: types.into_iter().map(IssueType::from).collect(),
        limit,
        include_claimed,
        include_blocked,
        parent: parent_id,
        meta,
    };
    let mut issues = storage.list_ready(&filter)?;
    // Priority filter is composed at the CLI layer (storage's
    // `list_ready` doesn't take it today — adding it to
    // `ReadyFilter` is symmetric with `labels` / `types` but the
    // ticket scopes the filter to the CLI surface for v2.8). Same
    // OR semantics as `labels_match` / `types_match`.
    if !priorities.is_empty() {
        issues.retain(|i| priorities_match(i, &priorities));
    }

    if claim {
        // Top result (if any) gets claimed atomically. Empty
        // ready set → exit 0 with `null` id under --json, silent
        // under plain text (mirrors --limit 1 on an empty set).
        // Race semantics: two parallel `ready --claim --limit 1`
        // calls both pick the same top id; both `Storage::claim`
        // calls race at `jj bookmark set`. jj rejects the loser
        // (non-fast-forward) and the loser surfaces a typed `Jj`
        // error — the orchestrator re-runs and picks the next id.
        let target = issues.first().cloned();
        match target {
            Some(issue) => {
                // `ready --claim` has no per-invocation actor flag;
                // the env-var slot in `resolve_current_user` is the
                // way orchestrators differentiate fan-out agents
                // here. v2.12 (`actor-override-chain`).
                let who = resolve_current_user(None)?;
                match storage.claim(&issue.id, &who)? {
                    ClaimResult::Claimed => {}
                    ClaimResult::AlreadyOurs => {
                        // The storage layer's mutate-retry contract
                        // surfaces this when our pre-write read showed
                        // the issue as already-claimed-by-us on the
                        // post-CAS-loss read. Since the ready filter
                        // excluded InProgress before we chose this id,
                        // someone (most likely a parallel `ready --claim`
                        // of ours) raced us to the CAS. Surface as a
                        // typed `claim_race_lost` so the orchestrator
                        // can re-run. `a6b8fb7`.
                        return Err(CliError::ClaimRaceLost {
                            id: issue.id.to_string(),
                        });
                    }
                }
                if json {
                    let out = serde_json::json!({
                        "ok": true,
                        "id": issue.id.as_str(),
                        "assignee": who,
                        "status": Status::InProgress.as_str(),
                        "claimed": true,
                    });
                    println!("{out}");
                } else {
                    println!("claimed {} by {who}", issue.id);
                }
            }
            None => {
                if json {
                    let out = serde_json::json!({
                        "ok": true,
                        "id": serde_json::Value::Null,
                        "claimed": false,
                    });
                    println!("{out}");
                }
                // plain text: silent on empty, mirroring `ls`.
            }
        }
        return Ok(());
    }

    if json {
        // Array of `Issue` records, pretty-printed. Same per-element
        // shape `ls --json` and `show --json` emit; callers parsing
        // one parse the others. Empty result is `[]`, not silence.
        let s = serde_json::to_string_pretty(&issues)
            .map_err(|e| CliError::Storage(StorageError::Json(e)))?;
        println!("{s}");
    } else {
        // Plain text: tab-separated rows mirroring `ls`'s shape so a
        // single awk/cut pipeline handles both. Silent on empty.
        //
        // v2.8 row (326bbf7): <id>\t<status>\t<priority>\t<type>\t<title>.
        for issue in &issues {
            let status_s = issue.status.as_str();
            let type_s = issue.type_.as_str();
            let priority_s = format_priority_column(issue.priority);
            println!(
                "{id}\t{status}\t{priority}\t{type_}\t{title}",
                id = issue.id,
                status = status_s,
                priority = priority_s,
                type_ = type_s,
                title = issue.title,
            );
        }
    }

    // Surface any unreadable refs the snapshot cache encountered —
    // same handling as `run_ls`. The ready set silently dropping an
    // id is a worse failure mode than `ls` doing it, because `ready`
    // is the agent's headline pick-next-work verb. Ticket `4928ae6`.
    let unreadable = storage.unreadable_refs()?;
    emit_unreadable_warning(&unreadable, json);

    Ok(())
}

/// `jjf search <query> [--status S] [--label L...] [--type T...]
/// [--include-comments] [--limit N] [--snippet-context N] [--json]`
/// — substring search across issue titles, bodies, and (optionally)
/// comment bodies.
///
/// Preflight mirrors `run_ls` / `run_ready` exactly — read verb, no
/// self-host-write guard. Storage layer handles the match logic;
/// this fn does filter composition, sort, limit, and render.
///
/// Sort: `score` descending (most hits first), then `created_at`
/// ascending (deterministic tiebreak — same shape as `list_ready`).
///
/// Output shapes diverge from `ls`'s bare-array convention because
/// the ticket spec calls for an envelope. The empty-result case
/// under `--json` is `{"ok":true,"results":[]}`, not silence,
/// matching the contract documented in `docs/cli-json.md`.
fn run_search(
    json: bool,
    query: String,
    status: StatusFilter,
    labels: Vec<String>,
    types: Vec<TypeArg>,
    include_comments: bool,
    limit: usize,
    snippet_context: usize,
    parent: Option<String>,
    include_metadata: bool,
    meta: Vec<(String, String)>,
) -> Result<(), CliError> {
    // Preflight: cwd is a jj repo AND `issues` bookmark exists.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;

    let storage = Storage::open(&cwd)?;
    let wanted_types: Vec<IssueType> =
        types.into_iter().map(IssueType::from).collect();

    let parent_id: Option<IssueId> = match parent {
        Some(handle) => {
            let resolved = resolve_handle(&storage, &handle)?;
            // Force an existence check: a well-formed-but-nonexistent
            // 7-char hex id would otherwise filter to nothing silently.
            // See `run_ls` for the same pattern.
            storage.read(&resolved)?;
            Some(resolved)
        }
        None => None,
    };

    let mut hits: Vec<SearchHit> =
        storage.search(&query, include_comments, include_metadata, snippet_context)?;
    // Compose status/label/type/meta filters on top of the substring
    // match. AND semantics across all filters, matching `ls`.
    hits.retain(|h| {
        status_matches(&h.issue, status)
            && labels_match(&h.issue, &labels)
            && types_match(&h.issue, &wanted_types)
            && parent_matches(&h.issue, parent_id.as_ref())
            && metadata_matches(&h.issue, &meta)
    });

    // Score descending, then created_at ascending. Stable sort means
    // equal-score entries fall back to the second key cleanly.
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.issue.created_at.cmp(&b.issue.created_at))
    });

    // `--limit 0` means unlimited (matches the `default_value_t = 20`
    // contract — a script that explicitly wants every match passes
    // `--limit 0`, not `--limit usize::MAX`).
    if limit > 0 && hits.len() > limit {
        hits.truncate(limit);
    }

    if json {
        // Envelope shape per the ticket spec:
        // `{"ok":true,"results":[{id,title,score,snippet,matched_field}]}`.
        // Distinct from `ls`'s bare-array convention; the ticket
        // body pins this contract explicitly.
        let results: Vec<serde_json::Value> = hits
            .iter()
            .map(|h| {
                serde_json::json!({
                    "id": h.issue.id.as_str(),
                    "title": h.issue.title,
                    "score": h.score,
                    "snippet": h.snippet,
                    "matched_field": h.matched_field.as_str(),
                })
            })
            .collect();
        let envelope = serde_json::json!({
            "ok": true,
            "results": results,
        });
        let s = serde_json::to_string_pretty(&envelope)
            .map_err(|e| CliError::Storage(StorageError::Json(e)))?;
        println!("{s}");
    } else {
        // Plain text: tab-separated rows. Silent on empty, mirroring
        // `ls`. Columns: id, title, matched_field, snippet.
        // The snippet itself has tabs/newlines normalized at the
        // storage layer (see `make_snippet`) so the column count
        // stays stable.
        for h in &hits {
            println!(
                "{id}\t{title}\t{field}\t{snippet}",
                id = h.issue.id,
                title = h.issue.title,
                field = h.matched_field.as_str(),
                snippet = h.snippet,
            );
        }
    }

    let unreadable = storage.unreadable_refs()?;
    emit_unreadable_warning(&unreadable, json);

    Ok(())
}

/// `jjf stale [--days N] [--status S] [--label L...] [--type T...]
/// [--limit N] [--json]` — surface issues whose `updated_at` is older
/// than `N` days. The orchestrator's "what work has gone quiet?"
/// hygiene query.
///
/// Preflight mirrors `run_ls` / `run_search` exactly — read verb, no
/// self-host-write guard. Storage layer computes the staleness set
/// (strict `>` semantics — see [`Storage::stale`]); this fn handles
/// the unit conversion (`days * 86_400`), filter composition, limit,
/// and render.
///
/// Output shapes mirror `ls`'s bare-array convention because the
/// ticket spec explicitly pins that shape (distinct from `search`,
/// which carries an `{ok:true,results:[...]}` envelope). Empty
/// result under `--json` is `[]`; plain text is silent.
fn run_stale(
    json: bool,
    days: u64,
    status: StatusFilter,
    labels: Vec<String>,
    types: Vec<TypeArg>,
    limit: usize,
    meta: Vec<(String, String)>,
) -> Result<(), CliError> {
    // Preflight: cwd is a jj repo AND `issues` bookmark exists. Same
    // order as `run_ls` / `run_search`.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;

    let storage = Storage::open(&cwd)?;
    let wanted_types: Vec<IssueType> =
        types.into_iter().map(IssueType::from).collect();

    // Unit conversion at the CLI boundary. Storage layer takes
    // seconds so the storage-level tests can pin to small intervals
    // (e.g. `--days 0` becomes `> 0 seconds`, hits anything older
    // than the pinned clock).
    let threshold_secs = days.saturating_mul(86_400);

    let mut hits: Vec<StaleHit> = storage.stale(threshold_secs, &meta)?;
    // Compose status/label/type filters on top of the staleness set.
    // AND semantics across all filters, matching `ls` / `search`.
    hits.retain(|h| {
        status_matches(&h.issue, status)
            && labels_match(&h.issue, &labels)
            && types_match(&h.issue, &wanted_types)
    });

    // Storage layer already sorts oldest-first; the retain pass
    // preserves order. `--limit 0` means unlimited — mirrors
    // `search`'s convention, declared in the clap doc-comment.
    if limit > 0 && hits.len() > limit {
        hits.truncate(limit);
    }

    if json {
        // Bare array of `{id, title, status, updated_at,
        // days_since_update}` — same structural shape `ls --json`
        // uses (also a bare array). The ticket's "JSON" example
        // pins this shape directly.
        let results: Vec<serde_json::Value> = hits
            .iter()
            .map(|h| {
                let days_since = h.seconds_since_update / 86_400;
                serde_json::json!({
                    "id": h.issue.id.as_str(),
                    "title": h.issue.title,
                    "status": h.issue.status.as_str(),
                    "updated_at": h.issue.updated_at,
                    "days_since_update": days_since,
                })
            })
            .collect();
        let s = serde_json::to_string_pretty(&results)
            .map_err(|e| CliError::Storage(StorageError::Json(e)))?;
        println!("{s}");
    } else {
        // Plain text: tab-separated rows. Columns: id, age (human
        // shape — `Nd`/`Nw`/`Nmo`), title, status. Silent on empty,
        // mirroring `ls` / `search`.
        for h in &hits {
            let age = render_age(h.seconds_since_update);
            println!(
                "{id}\t{age}\t{title}\t{status}",
                id = h.issue.id,
                age = age,
                title = h.issue.title,
                status = h.issue.status.as_str(),
            );
        }
    }

    let unreadable = storage.unreadable_refs()?;
    emit_unreadable_warning(&unreadable, json);

    Ok(())
}

/// Render a stale-age in seconds as a short human-friendly string.
///
/// Boundaries (round down at each):
///
/// - `< 30d` → `Nd` (whole days)
/// - `>= 30d && < 90d` → `Nw` (whole weeks; one week == 7 days)
/// - `>= 90d` → `Nmo` (whole months; one month == 30 days, the
///   approximate-by-convention figure — calendar months aren't a
///   stable unit at this resolution and the orchestrator just wants
///   a back-of-envelope number)
///
/// Boundaries chosen so the common stale-set lives in `Nd` shape
/// (most "this work has gone quiet" tickets are 2-4 weeks old and
/// `19d` reads faster than `2w` at a glance). Weeks and months kick
/// in only when the age has compressed enough that the precision is
/// noise.
///
/// Output is always a single token (no spaces); the renderer never
/// emits compound forms like `1w 5d` or `~3w`. Documented in
/// `docs/cli-json.md`'s per-verb `stale` section.
fn render_age(secs: u64) -> String {
    let days = secs / 86_400;
    if days < 30 {
        format!("{days}d")
    } else if days < 90 {
        format!("{}w", days / 7)
    } else {
        format!("{}mo", days / 30)
    }
}

/// `--status` predicate. `All` matches everything (including
/// `Abandoned`).
fn status_matches(issue: &Issue, filter: StatusFilter) -> bool {
    match filter {
        StatusFilter::All => true,
        StatusFilter::Open => issue.status == Status::Open,
        StatusFilter::Blocked => issue.status == Status::Blocked,
        StatusFilter::InProgress => issue.status == Status::InProgress,
        StatusFilter::Closed => issue.status == Status::Closed,
        StatusFilter::Abandoned => issue.status == Status::Abandoned,
    }
}

/// `--label` predicate. Empty filter matches every issue. A non-empty
/// filter requires the issue to carry EVERY listed label (intersection).
fn labels_match(issue: &Issue, wanted: &[String]) -> bool {
    wanted.iter().all(|w| issue.labels.iter().any(|l| l == w))
}

/// Clap `value_parser` for `--meta key=value`. Splits on the FIRST
/// `=`. Rejects bare keys (no `=`) at parse time so a typo like
/// `--meta gc.routed_to` exits 2 with a clear message instead of
/// silently filtering on `key=""`. Values may contain `=` (only the
/// first split matters).
fn parse_meta_kv(s: &str) -> std::result::Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) => Ok((k.to_owned(), v.to_owned())),
        None => Err(format!(
            "expected key=value, got `{}` — bare key (no `=`) is not a valid filter; \
             use `key=` to match an empty value explicitly",
            s
        )),
    }
}

/// `--meta` predicate. Empty filter matches every issue. A non-empty
/// filter requires the issue's metadata map to carry EVERY listed
/// `key=value` pair exactly (AND semantics, mirroring `--label`).
/// Parsing is done at argv-time by `parse_meta_kv`; bare keys are
/// rejected before this function is ever called. Values may contain `=`.
fn metadata_matches(issue: &Issue, wanted: &[(String, String)]) -> bool {
    wanted
        .iter()
        .all(|(k, v)| issue.metadata.get(k) == Some(v))
}

/// `--type` predicate. Empty filter matches every issue. A non-empty
/// filter requires the issue's type to equal AT LEAST ONE listed
/// type (union). Mirrors the OR-semantics behavior the ticket calls
/// out, distinct from `--label`'s AND.
fn types_match(issue: &Issue, wanted: &[IssueType]) -> bool {
    wanted.is_empty() || wanted.iter().any(|t| *t == issue.type_)
}

/// `--parent` predicate. None matches every issue. Some(pid)
/// requires the issue to carry a `DepKind::ParentChild` edge
/// whose target equals `pid`. Mirrors `ReadyFilter::parent`'s
/// semantics on the CLI side for verbs that don't go through
/// the storage-layer filter (`ls`, `search`).
fn parent_matches(issue: &Issue, wanted: Option<&IssueId>) -> bool {
    match wanted {
        None => true,
        Some(pid) => issue
            .dependencies
            .iter()
            .any(|d| d.target == *pid && d.kind == DepKind::ParentChild),
    }
}

/// `--priority` predicate. Empty filter matches every issue. A
/// non-empty filter requires the issue's `priority` to equal at
/// least one listed value (OR — mirrors `types_match`). Issues with
/// `None` priority never match any explicit value. v2.8
/// (`priority-field`).
fn priorities_match(issue: &Issue, wanted: &[u8]) -> bool {
    if wanted.is_empty() {
        return true;
    }
    match issue.priority {
        Some(n) => wanted.iter().any(|w| *w == n),
        None => false,
    }
}

/// Render the priority column for the tab-separated `ls` / `ready`
/// row. `Some(n)` becomes `P{n}` (e.g. `P0`); `None` becomes a
/// single dash so the column always carries a value (keeps
/// awk / cut parsing trivial). v2.8 (`priority-field`).
fn format_priority_column(p: Option<u8>) -> String {
    match p {
        Some(n) => format!("P{n}"),
        None => "-".into(),
    }
}

/// `--slug` predicate. `None` filter matches every issue. A non-`None`
/// filter requires the issue's `slug` to contain the pattern as a
/// substring (case-sensitive — slugs are already lowercase). Issues
/// without a slug never match.
fn slug_matches(issue: &Issue, pattern: Option<&str>) -> bool {
    match pattern {
        None => true,
        Some(p) => issue
            .slug
            .as_deref()
            .is_some_and(|s| s.contains(p)),
    }
}

/// `jjf push <remote>` — push every `refs/jjf/*` ref to `<remote>` via
/// standard git transport.
///
/// The v3 refspec is `refs/jjf/*:refs/jjf/*`, covering issues, memories,
/// and the format-version sentinel. Server-side config is vanilla git —
/// Forgejo / Gitea / GitLab / GitHub all accept this; no special
/// permissions or hooks needed beyond push access to the repo.
///
/// stderr classification mirrors `run_pull`: unknown remote / network
/// unreachable / auth / non-fast-forward / catch-all. The patterns
/// match libgit2's stable phrases so a stderr-format tweak on either
/// side stays detectable.
///
/// **V2 fallback.** If the repo is still v2-shape (env-var opt-out of
/// the migrator set in tests), this verb falls back to
/// `jj git push --bookmark issues` — the v2 transport. Operators don't
/// Preflight: full `issues_bookmark` probe — the v3 sentinel ref must
/// exist locally for there to be anything to push.
fn run_push(json: bool, remote: String) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;
    let storage = Storage::open(&cwd).map_err(CliError::Storage)?;
    run_push_v3(json, &remote, &storage)
}

/// v3 push — delegate to the storage layer, classify failures.
fn run_push_v3(
    json: bool,
    remote: &str,
    storage: &Storage,
) -> Result<(), CliError> {
    match storage.push_v3(remote) {
        Ok(report) => {
            if json {
                let out = serde_json::json!({
                    "ok": true,
                    "remote": remote,
                    "refs_pushed": report.refs_pushed,
                });
                println!("{out}");
            } else {
                println!(
                    "pushed {} refs/jjf/* ref(s) -> {remote}",
                    report.refs_pushed
                );
            }
            Ok(())
        }
        Err(e) => Err(classify_storage_push_error(remote, e)),
    }
}

/// Translate a storage-layer push failure into the typed CLI error
/// kinds, by sniffing the embedded stderr. Mirrors
/// [`classify_push_error`] (the v2 path's classifier); the substring
/// matchers are the same since both code paths shell out to git
/// transport ultimately.
fn classify_storage_push_error(remote: &str, e: StorageError) -> CliError {
    // Only `Error::Git` carries stderr we can classify; anything else
    // falls through to the generic storage envelope.
    let stderr = match &e {
        StorageError::Git(g) => format!("{}", g),
        _ => return CliError::Storage(e),
    };
    let parsed = classify_push_error(remote, stderr);
    // If the classifier didn't match anything specific, keep the
    // typed storage error so the envelope carries the storage::Error
    // shape rather than a flattened string.
    if matches!(parsed, CliError::JjGitPush(_)) {
        return CliError::Storage(e);
    }
    parsed
}

/// Map jj-git-push stderr to a typed `CliError`. Keeps the
/// substring-matching out of `run_push` proper so the dispatch logic
/// stays scannable and so the matcher can be unit-tested directly.
fn classify_push_error(remote: &str, stderr: String) -> CliError {
    // Unknown remote — jj's canonical phrase ("No git remote named ..."),
    // git's canonical phrase ("does not appear to be a git repository",
    // "Could not read from remote repository", "remote ... not found"),
    // any of which means the same thing operationally. The `remote rm`
    // verb's mapper uses the jj phrase; we reuse the kind across the
    // v2 / v3 transport split.
    if stderr.contains("No git remote named")
        || stderr.contains("does not appear to be a git repository")
        || stderr.contains("Could not read from remote repository")
        || stderr.contains("Repository not found")
    {
        return CliError::RemoteNotFound(remote.to_owned());
    }
    // Authentication. jj surfaces git2's libcurl/libgit2 errors here;
    // we pattern-match on the lowercase form of the key tokens so
    // case-variant stderr from different platforms still classifies.
    let lower = stderr.to_lowercase();
    if lower.contains("authentication")
        || lower.contains("permission denied")
        || lower.contains("access denied")
        || lower.contains("could not read username")
        || lower.contains("401 unauthorized")
    {
        return CliError::PushAuthFailure {
            remote: remote.to_owned(),
            stderr,
        };
    }
    // Non-fast-forward / rejected. The operator path here is "pull
    // first then retry"; the structured `hint` lives in
    // `details.hint` on the `--json` envelope (see `CliError::
    // details`). The Display impl is a short, deterministic, single
    // line — raw stderr stays in `details.stderr_raw` rather than
    // leaking into `message`.
    if lower.contains("refusing to push")
        || lower.contains("rejected")
        || lower.contains("non-fast-forward")
        || lower.contains("non fast-forward")
    {
        let refs_rejected = parse_rejected_refs(&stderr);
        return CliError::PushRejected {
            remote: remote.to_owned(),
            stderr,
            refs_rejected,
        };
    }
    // Network. Broad: any signal that we couldn't reach the remote.
    if lower.contains("could not resolve")
        || lower.contains("connection refused")
        || lower.contains("failed to connect")
        || lower.contains("no such device")
        || lower.contains("network is unreachable")
        || lower.contains("could not connect")
    {
        return CliError::PushNetworkFailure {
            remote: remote.to_owned(),
            stderr,
        };
    }
    CliError::JjGitPush(stderr.trim().to_owned())
}

/// Parse the destination refs git reported as rejected from `git push`
/// stderr. Looks for lines of the form (after whitespace):
///
/// ```text
/// ! [rejected]        <src> -> <dst> (fetch first)
/// ! [rejected]        <src> -> <dst> (non-fast-forward)
/// ```
///
/// The destination ref (the right-hand side of the arrow) is what
/// goes into `details.refs_rejected` — that's what a caller asking
/// "which refs do I need to pull?" wants. The trailing parenthetical
/// reason (`fetch first` / `non-fast-forward`) is dropped from the
/// structured surface; it's available in `details.stderr_raw` if a
/// caller really needs the disambiguation.
///
/// Best-effort: if no `! [rejected]` lines are present (or git's
/// future stderr drifts from this format), returns an empty vec —
/// the `details` formatter renders that as `null` so callers can
/// distinguish "parser saw nothing" from "ship an empty array".
fn parse_rejected_refs(stderr: &str) -> Vec<String> {
    let mut refs = Vec::new();
    for line in stderr.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("! [rejected]") {
            continue;
        }
        // Strip the leading `! [rejected]` marker, then look for
        // ` -> ` and take the next whitespace-delimited token as the
        // destination ref.
        let after_marker = trimmed.trim_start_matches("! [rejected]").trim_start();
        let Some(arrow_idx) = after_marker.find("->") else {
            continue;
        };
        let after_arrow = after_marker[arrow_idx + 2..].trim_start();
        let dst = match after_arrow.split_whitespace().next() {
            Some(s) => s,
            None => continue,
        };
        refs.push(dst.to_owned());
    }
    refs
}

/// `jjf pull <remote>` — fetch every `refs/jjf/*` from `<remote>` into
/// the standard remote-tracking namespace
/// (`refs/remotes/<remote>/jjf/*`), then reconcile each remote-tracking
/// ref against its local `refs/jjf/<rest>` counterpart using the
/// five-scenario merge algorithm.
///
/// Five-scenario merge (per ref):
/// 1. New (remote-only) → copy remote tip into local ref.
/// 2. Identical → no-op.
/// 3. Local ahead (remote is ancestor) → no-op.
/// 4. Fast-forward → advance local ref to remote tip.
/// 5. Diverged → land a 2-parent merge commit on the local ref whose
///    tree carries the LWW-resolved record + comments and whose message
///    has a `Jjf-Op: merge` trailer.
///
/// **No content-level "unmergeable" failure mode.** The DAG IS the
/// merge; the op-space LWW reducer always produces a valid resolved
/// state. The legacy `Unmergeable` / `CommentFileConflict` error kinds
/// stay wired for external callers but cannot arise from this verb in
/// v3 mode.
///
/// stderr classification (auth / network / unknown remote / catch-all)
/// mirrors `classify_fetch_error`.
fn run_pull(json: bool, remote: String) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    // `pull` uses the jj-repo-only preflight: a fresh clone has no
    // v3 sentinel ref; `pull` is precisely the verb that materializes
    // it. Requiring the sentinel up front would force an awkward
    // `jjf init` on a clone whose remote already has all the v3 refs
    // we want to fetch.
    preflight::jj_repo(&cwd)?;
    run_pull_v3(json, &remote, &cwd)
}

/// v3 pull — bare git fetch into the standard remote-tracking namespace,
/// then per-ref five-scenario reconcile. Implementation in
/// `jjf_storage::sync_v3`.
fn run_pull_v3(json: bool, remote: &str, cwd: &Path) -> Result<(), CliError> {
    // Lazy auto-config: a fresh clone may not have the jjforge fetch
    // refspec wired up yet (the user skipped `jjf init` or added the
    // remote outside `jjf remote add`). Pull is the first verb that
    // actually wants the refspec, so plant it now if missing. Failures
    // are tolerated — `sync_v3::pull_v3` uses an explicit refspec on
    // the fetch CLI and will still work.
    let _ = ensure_jjf_fetch_refspec(cwd, remote);

    match jjf_storage::pull_v3_bare(cwd, remote) {
        Ok(report) => {
            emit_pull_v3_success(json, remote, &report);
            Ok(())
        }
        Err(e) => Err(classify_storage_pull_error(remote, e)),
    }
}

/// Translate a storage-layer pull failure into typed CLI errors by
/// sniffing the embedded stderr. Same matchers as `classify_fetch_error`
/// since both paths shell to git transport.
fn classify_storage_pull_error(remote: &str, e: StorageError) -> CliError {
    let stderr = match &e {
        StorageError::Git(g) => format!("{}", g),
        _ => return CliError::Storage(e),
    };
    let parsed = classify_fetch_error(remote, stderr);
    if matches!(parsed, CliError::JjGitFetch(_)) {
        return CliError::Storage(e);
    }
    parsed
}

/// Emit the success envelope for a v3 pull. JSON shape carries the full
/// per-scenario tally; the plain-text shape summarizes.
fn emit_pull_v3_success(
    json: bool,
    remote: &str,
    report: &jjf_storage::PullReportV3,
) {
    let total_refs = report.new_local
        + report.identical
        + report.local_ahead
        + report.fast_forwards
        + report.merged;
    let remote_present = total_refs > 0;
    if json {
        let mut obj = serde_json::Map::new();
        obj.insert("ok".into(), serde_json::Value::Bool(true));
        obj.insert("remote".into(), serde_json::Value::String(remote.to_owned()));
        // Keep the legacy `bookmark` key out of the v3 envelope — the
        // v3 shape is per-ref, not bookmark-shaped. Tests for the v3
        // path key off `refs_pushed` / `merged` etc.
        obj.insert(
            "remote_present".into(),
            serde_json::Value::Bool(remote_present),
        );
        obj.insert(
            "merge_strategy".into(),
            serde_json::Value::String("per_ref_lww".into()),
        );
        obj.insert(
            "new_local".into(),
            serde_json::Value::from(report.new_local),
        );
        obj.insert(
            "identical".into(),
            serde_json::Value::from(report.identical),
        );
        obj.insert(
            "local_ahead".into(),
            serde_json::Value::from(report.local_ahead),
        );
        obj.insert(
            "fast_forwards".into(),
            serde_json::Value::from(report.fast_forwards),
        );
        obj.insert("merged".into(), serde_json::Value::from(report.merged));
        // Compat alias: v2's `resolved_issues` was the number of issue
        // refs that needed a merge. The v3 equivalent is `merged`; we
        // surface both names so existing parsers don't break.
        obj.insert(
            "resolved_issues".into(),
            serde_json::Value::from(report.merged),
        );
        let envelope = serde_json::Value::Object(obj);
        println!("{envelope}");
    } else if !remote_present {
        println!("pulled {remote}: no jjf refs on remote yet");
    } else if report.merged == 0 {
        println!("pulled {} refs/jjf/* ref(s) <- {remote}", total_refs);
    } else {
        println!(
            "pulled {} refs/jjf/* ref(s) <- {remote}; merged {} ref(s)",
            total_refs, report.merged
        );
    }
}

/// Map jj-git-fetch stderr to a typed `CliError`. Mirrors
/// `classify_push_error`'s shape; the substring sets are the same set
/// of "what does libgit2 say when it can't auth / can't reach" lines.
fn classify_fetch_error(remote: &str, stderr: String) -> CliError {
    // jj's fetch surface uses a slightly different phrase than its
    // `git remote remove` surface — "No matching remotes for names:
    // <name>" (followed by "No git remotes to fetch from") — so we
    // accept either canonical wording.
    if stderr.contains("No git remote named")
        || stderr.contains("No matching remotes for names")
        || stderr.contains("does not appear to be a git repository")
        || stderr.contains("Could not read from remote repository")
        || stderr.contains("Repository not found")
    {
        return CliError::RemoteNotFound(remote.to_owned());
    }
    let lower = stderr.to_lowercase();
    if lower.contains("authentication")
        || lower.contains("permission denied")
        || lower.contains("access denied")
        || lower.contains("could not read username")
        || lower.contains("401 unauthorized")
    {
        return CliError::PullAuthFailure {
            remote: remote.to_owned(),
            stderr,
        };
    }
    if lower.contains("could not resolve")
        || lower.contains("connection refused")
        || lower.contains("failed to connect")
        || lower.contains("no such device")
        || lower.contains("network is unreachable")
        || lower.contains("could not connect")
    {
        return CliError::PullNetworkFailure {
            remote: remote.to_owned(),
            stderr,
        };
    }
    CliError::JjGitFetch(stderr.trim().to_owned())
}


#[cfg(test)]
mod tests {
    //! Unit tests for the in-binary helpers — kept here so they can
    //! reach private functions (`classify_push_error`,
    //! `parse_rejected_refs`, the error envelope renderers) without
    //! widening the visibility surface.
    //!
    //! Integration tests for full verb behaviour live under
    //! `crates/jjf/tests/`.
    use super::*;

    /// Real stderr captured by `experiments/qa-redteam-2026-06-25/
    /// .scratch/d3b/evidence/d3b-hint-snapshot.txt`: a single rejected
    /// ref (`refs/jjf/issues/bfcfe03`), `(fetch first)` parenthetical,
    /// followed by the multi-line `hint:` preamble git pastes in.
    /// Pinning this exact shape so anything that drifts the parser
    /// (rename / trim / regex tweak) trips a known-good case.
    const REAL_D3B_STDERR: &str = "\
To file:///Users/myers/p/jjforge/experiments/qa-redteam-2026-06-25/.scratch/d3b-bare.git\n\
 ! [rejected]        refs/jjf/issues/bfcfe03 -> refs/jjf/issues/bfcfe03 (fetch first)\n\
error: failed to push some refs to 'file:///Users/myers/p/jjforge/experiments/qa-redteam-2026-06-25/.scratch/d3b-bare.git'\n\
hint: Updates were rejected because the remote contains work that you do not\n\
hint: have locally. This is usually caused by another repository pushing to\n\
hint: the same ref. If you want to integrate the remote changes, use\n\
hint: 'git pull' before pushing again.\n\
hint: See the 'Note about fast-forwards' in 'git push --help' for details.\n";

    #[test]
    fn parse_rejected_refs_single_ref() {
        let refs = parse_rejected_refs(REAL_D3B_STDERR);
        assert_eq!(refs, vec!["refs/jjf/issues/bfcfe03".to_owned()]);
    }

    #[test]
    fn parse_rejected_refs_multiple() {
        let stderr = "\
To file:///some/bare.git\n\
 ! [rejected]        refs/jjf/issues/aaaaaa1 -> refs/jjf/issues/aaaaaa1 (fetch first)\n\
 ! [rejected]        refs/jjf/issues/bbbbbb2 -> refs/jjf/issues/bbbbbb2 (non-fast-forward)\n\
 ! [rejected]        refs/jjf/memories/ccccc -> refs/jjf/memories/ccccc (fetch first)\n\
error: failed to push some refs\n";
        let refs = parse_rejected_refs(stderr);
        assert_eq!(
            refs,
            vec![
                "refs/jjf/issues/aaaaaa1".to_owned(),
                "refs/jjf/issues/bbbbbb2".to_owned(),
                "refs/jjf/memories/ccccc".to_owned(),
            ]
        );
    }

    #[test]
    fn parse_rejected_refs_none_when_no_marker() {
        // Catch-all stderr that doesn't carry git's `! [rejected]`
        // line at all — parser must return empty (not panic, not
        // hallucinate). The `details` formatter renders this as
        // `null` in the JSON envelope.
        let stderr = "error: failed to push some refs (remote hung up)\n";
        let refs = parse_rejected_refs(stderr);
        assert!(refs.is_empty(), "expected no refs, got {refs:?}");
    }

    #[test]
    fn parse_rejected_refs_handles_indented_marker() {
        // Some git versions / locales / colorisations stretch the
        // leading whitespace; the parser trims_start before matching
        // so any indent is fine.
        let stderr = "    ! [rejected]        refs/jjf/issues/x1y2z3a -> refs/jjf/issues/x1y2z3a (fetch first)\n";
        let refs = parse_rejected_refs(stderr);
        assert_eq!(refs, vec!["refs/jjf/issues/x1y2z3a".to_owned()]);
    }

    #[test]
    fn classify_push_error_real_d3b_yields_push_rejected_with_refs() {
        let err = classify_push_error("origin", REAL_D3B_STDERR.to_owned());
        match err {
            CliError::PushRejected {
                remote,
                stderr,
                refs_rejected,
            } => {
                assert_eq!(remote, "origin");
                assert_eq!(refs_rejected, vec!["refs/jjf/issues/bfcfe03".to_owned()]);
                // Raw stderr stays available for debugging callers
                // via `details.stderr_raw`.
                assert!(stderr.contains("! [rejected]"));
            }
            other => panic!("expected PushRejected, got {other:?}"),
        }
    }

    #[test]
    fn push_rejected_display_is_short_and_deterministic() {
        // The Display impl backs the `message` field in the `--json`
        // envelope. Spec: short, single-line, no raw git stderr, no
        // version-dependent advisory tokens (`fetch first`, git's
        // own `hint:` preamble). Scripts must use `kind`, not
        // `message` — but if a human ever does read this line, it
        // shouldn't change shape between git releases.
        let err = CliError::PushRejected {
            remote: "origin".to_owned(),
            stderr: REAL_D3B_STDERR.to_owned(),
            refs_rejected: vec!["refs/jjf/issues/bfcfe03".to_owned()],
        };
        let msg = format!("{err}");
        assert!(
            !msg.contains('\n'),
            "message should be single-line, got: {msg:?}"
        );
        assert!(
            !msg.contains("fetch first"),
            "message must not embed git's version-dependent `fetch first` token: {msg:?}"
        );
        assert!(
            !msg.contains("hint:"),
            "message must not embed git's `hint:` preamble: {msg:?}"
        );
        assert!(
            !msg.contains("refs/jjf"),
            "message must not embed the internal refspec: {msg:?}"
        );
        // Must name the remote so a human reading it knows which
        // push got rejected.
        assert!(msg.contains("origin"), "message should name remote: {msg:?}");
    }

    #[test]
    fn push_rejected_details_carry_structured_hint_and_refs() {
        // `details` is the contract surface. Asserts the shape the
        // `cli-json.md` push_rejected row promises.
        let err = CliError::PushRejected {
            remote: "origin".to_owned(),
            stderr: REAL_D3B_STDERR.to_owned(),
            refs_rejected: vec!["refs/jjf/issues/bfcfe03".to_owned()],
        };
        let details = err.details();
        assert_eq!(details["remote"], "origin");
        assert_eq!(
            details["hint"],
            "run `jjf pull origin` first, then retry the push"
        );
        assert_eq!(
            details["refs_rejected"],
            serde_json::json!(["refs/jjf/issues/bfcfe03"])
        );
        assert!(
            details["stderr_raw"]
                .as_str()
                .expect("stderr_raw is a string")
                .contains("! [rejected]"),
            "stderr_raw should preserve original git output"
        );
    }

    #[test]
    fn push_rejected_details_emit_null_when_no_refs_parsed() {
        // Honest signalling: if the parser didn't recognise any
        // rejected lines (e.g. git output drifted), surface `null`
        // rather than an empty array — callers can distinguish
        // "parser failed" from "definitively no refs".
        let err = CliError::PushRejected {
            remote: "origin".to_owned(),
            stderr: "error: failed to push some refs (remote hung up)\n".to_owned(),
            refs_rejected: vec![],
        };
        let details = err.details();
        assert!(
            details["refs_rejected"].is_null(),
            "expected null, got: {}",
            details["refs_rejected"]
        );
    }

    // --- actor-override-chain (v2.12, ticket `ae0866b`) --------------
    //
    // Only the pure trim/whitespace behaviour of `jjf_actor_env` is
    // tested in-process: nextest runs tests in shared processes, so
    // `std::env::set_var` from a unit test would leak across the
    // workspace and silently corrupt unrelated tests' env. The
    // full precedence-chain integration coverage (env-only set,
    // flag-over-env, flag-empty-falls-through, env-empty-falls-
    // through, all-empty → `NoCurrentUser`, comment author chain)
    // lives in `crates/jjf/tests/actor.rs`, which scopes every
    // env tweak to a child `Command::env(...)`.

    /// `jjf_actor_env` reads `JJF_ACTOR` lazily; passing
    /// whitespace-only or empty values has to fall through (the
    /// chain semantics say "skip me," not "set empty"). This test
    /// removes the var first so the parent env can't pollute it.
    #[test]
    fn jjf_actor_env_unset_returns_none() {
        // SAFETY: nextest runs each test in a shared process; we
        // only `remove_var` here (no set), and we restore nothing
        // because the canonical state of `JJF_ACTOR` for the test
        // process is "unset." If a parent set it, the orchestrator
        // wanted it gone for this assertion to be meaningful.
        unsafe {
            std::env::remove_var("JJF_ACTOR");
        }
        assert_eq!(jjf_actor_env(), None);
    }
}
