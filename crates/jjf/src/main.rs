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
//! All the actual work — the 4-CLI dance, the trailers, the merge
//! policy — lives in `jjf-storage` (and, for conflict-resolution,
//! `jjf-merge`). This crate's only jobs are: parse args, hand the
//! parsed shape to storage, render the result, map errors to exit
//! codes. No business logic.

mod preflight;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use jjf_storage::{
    ISSUES_BOOKMARK, Error as StorageError, IdError, Issue, IssueDraft, IssueId, IssueType,
    Memory, ReadyFilter, SlugInvalidReason, Status, Storage, UpdateFields,
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
/// equivalent. v2.3 added `in-progress` mirroring the new
/// `Status::InProgress` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum StatusFilter {
    Open,
    #[value(name = "in-progress")]
    InProgress,
    Closed,
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
    #[value(name = "in-progress")]
    InProgress,
    Closed,
}

impl From<StatusArg> for Status {
    fn from(s: StatusArg) -> Self {
        match s {
            StatusArg::Open => Status::Open,
            StatusArg::InProgress => Status::InProgress,
            StatusArg::Closed => Status::Closed,
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

        /// Declare a dependency on another issue id. Repeatable. Each
        /// value must be a 7-char lowercase-hex issue id; a bad value
        /// is a preflight failure (exit 2).
        #[arg(short = 'd', long = "dep")]
        deps: Vec<String>,

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
    },

    /// Print a single issue from the `issues` bookmark — title,
    /// status, labels, assignee, body, and comment thread. Plain-text
    /// by default; `--json` emits the structured `Issue` record
    /// verbatim (no envelope — the issue IS the payload). Requires
    /// `jjf init` to have been run first.
    Show {
        /// Issue handle (7-char hex id OR a slug). Slugs resolve
        /// across both open and closed issues. A handle that's
        /// neither a parseable id nor a known slug is exit 2
        /// (`slug_not_found`); a parseable id with no matching
        /// record on the bookmark is exit 1 (`issue_not_found`).
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
        /// Filter by status. `open` is the default (matches git-bug
        /// and the "lists are about what's actionable" convention).
        /// `all` shows every issue regardless of status.
        #[arg(long, value_enum, default_value_t = StatusFilter::Open)]
        status: StatusFilter,

        /// Filter by label. Repeatable. Semantics: AND — an issue
        /// must carry every listed label to match.
        #[arg(short = 'l', long = "label")]
        labels: Vec<String>,

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
    /// (exit 1). The plain-text message includes a hint to `pull`
    /// first.
    #[error("push to {remote} rejected: {stderr}\nhint: run `jjf pull {remote}` first, then retry the push")]
    PushRejected { remote: String, stderr: String },

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

    /// Refused to run a mutating verb from inside the jjforge source
    /// repo. The colocated jj+git layout means the storage layer's
    /// 4-CLI write dance moves git HEAD off `main` and onto a phantom
    /// `refs/jj/root`, leaving the working tree apparently empty
    /// against the new HEAD — destructive to recover from. Preflight
    /// failure (exit 2) per the standard exit-code convention; the
    /// operator can opt in via `JJF_ALLOW_SELF_HOST=1` if they
    /// genuinely need to write from inside (e.g. orchestration
    /// loops). See `crates/jjf/src/preflight.rs` for the marker-set
    /// detection rationale.
    #[error(
        "refusing to write from inside the jjforge source repo at {path}; this would drift git HEAD onto refs/jj/root.\nhint: cd to a sibling working dir (e.g. ~/p/jjforge-data) and retry, or set JJF_ALLOW_SELF_HOST=1 to override"
    )]
    SelfHostedWriteRefused {
        path: PathBuf,
        markers: Vec<String>,
    },

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

    /// A slug write would collide with an existing open issue.
    /// Preflight failure (exit 2). `conflicts_with` is the id of
    /// the open issue already holding the slug.
    ///
    /// In practice the storage layer's `Error::SlugCollision`
    /// surfaces this case — the CLI-side variant stays defined so
    /// that future callers can construct it directly without going
    /// through `Storage` (e.g. if the CLI grows pre-flight
    /// uniqueness checks).
    #[allow(dead_code)]
    #[error(
        "slug {slug:?} already in use by open issue {conflicts_with}"
    )]
    SlugCollision {
        slug: String,
        conflicts_with: String,
    },

    /// `Storage::resolve` couldn't translate the handle the operator
    /// supplied: it wasn't a parseable 7-hex id and no open-or-closed
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
    /// a `user.name` in jj's config. Preflight failure (exit 2) —
    /// claims require an identity to assign to.
    /// v2.3 (`agent-claim-atomic`).
    #[error(
        "no current user available; set jj user.name (e.g. `jj config set --user user.name 'Your Name'`) to claim issues"
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
            CliError::Storage(StorageError::InvalidSlug { .. }) => 2,
            CliError::Storage(StorageError::SlugCollision { .. }) => 2,
            CliError::Storage(StorageError::SlugNotFound { .. }) => 2,
            CliError::Storage(StorageError::AlreadyClaimed { .. }) => 2,
            CliError::Cwd(_) => 2,
            CliError::BodyRead { .. } => 2,
            CliError::BadDepId { .. } => 2,
            CliError::BadIssueId { .. } => 2,
            CliError::MissingIssuesBookmark(_) => 2,
            CliError::EmptyCommentBody => 2,
            CliError::EmptyLabel => 2,
            CliError::MissingAuthor => 2,
            CliError::NoUpdateFields => 2,
            CliError::RemoteAlreadyExists(_) => 2,
            CliError::RemoteNotFound(_) => 2,
            CliError::SelfHostedWriteRefused { .. } => 2,
            CliError::InvalidSlug { .. } => 2,
            CliError::SlugCollision { .. } => 2,
            CliError::SlugNotFound { .. } => 2,
            CliError::MissingMemoryValue => 2,
            CliError::EmptyMemoryKey { .. } => 2,
            CliError::MemoryNotFound { .. } => 1,
            CliError::NoCurrentUser => 2,
            CliError::ClaimRequiresLimitOne => 2,
            CliError::AlreadyClaimed { .. } => 2,
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
            CliError::Storage(StorageError::IssueNotFound(_)) => "issue_not_found",
            CliError::Storage(StorageError::Invalid(_)) => "invalid_input",
            CliError::Storage(StorageError::Clock(_)) => "clock_error",
            CliError::Storage(StorageError::Io(_)) => "io_error",
            CliError::Storage(StorageError::Json(_)) => "json_error",
            CliError::Storage(StorageError::Jj(_)) => "jj_error",
            CliError::Storage(StorageError::InvalidSlug { .. }) => "invalid_slug",
            CliError::Storage(StorageError::SlugCollision { .. }) => "slug_collision",
            CliError::Storage(StorageError::SlugNotFound { .. }) => "slug_not_found",
            CliError::Storage(StorageError::AlreadyClaimed { .. }) => "already_claimed",
            CliError::InvalidSlug { .. } => "invalid_slug",
            CliError::SlugCollision { .. } => "slug_collision",
            CliError::SlugNotFound { .. } => "slug_not_found",
            CliError::MissingMemoryValue => "missing_memory_value",
            CliError::EmptyMemoryKey { .. } => "empty_memory_key",
            CliError::MemoryNotFound { .. } => "memory_not_found",
            CliError::Cwd(_) => "cwd_error",
            CliError::BodyRead { .. } => "body_read_error",
            CliError::BadDepId { .. } => "bad_id",
            CliError::BadIssueId { .. } => "bad_id",
            CliError::MissingIssuesBookmark(_) => "missing_issues_bookmark",
            CliError::EmptyCommentBody => "empty_body",
            CliError::EmptyLabel => "empty_label",
            CliError::MissingAuthor => "missing_author",
            CliError::NoUpdateFields => "no_update_fields",
            CliError::RemoteAlreadyExists(_) => "remote_already_exists",
            CliError::RemoteNotFound(_) => "remote_not_found",
            CliError::SelfHostedWriteRefused { .. } => "self_hosted_write_refused",
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
            CliError::Storage(StorageError::IssueNotFound(id)) => {
                json!({ "id": id.as_str() })
            }
            CliError::BodyRead { from, .. } => json!({ "from": from }),
            CliError::BadDepId { value, .. } => json!({ "value": value, "field": "dep" }),
            CliError::BadIssueId { value, .. } => json!({ "value": value, "field": "id" }),
            CliError::MissingIssuesBookmark(path) => {
                json!({ "path": path.display().to_string() })
            }
            CliError::RemoteAlreadyExists(name) => json!({ "name": name }),
            CliError::RemoteNotFound(name) => json!({ "name": name }),
            CliError::SelfHostedWriteRefused { path, markers } => json!({
                "path": path.display().to_string(),
                "markers": markers,
            }),
            CliError::PushNetworkFailure { remote, .. }
            | CliError::PushAuthFailure { remote, .. }
            | CliError::PushRejected { remote, .. }
            | CliError::PullNetworkFailure { remote, .. }
            | CliError::PullAuthFailure { remote, .. } => json!({ "remote": remote }),
            CliError::Unmergeable { issue_id, detail } => {
                json!({ "issue_id": issue_id, "detail": detail })
            }
            CliError::CommentFileConflict { issue_id } => json!({ "issue_id": issue_id }),
            CliError::Storage(StorageError::InvalidSlug { slug, reason })
            | CliError::InvalidSlug { slug, reason } => {
                json!({ "slug": slug, "reason": reason.as_str() })
            }
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
            assignee,
            r#type,
            slug,
        } => run_new(cli.json, title, file, labels, deps, assignee, r#type, slug),
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
            types,
            slug,
        } => run_ls(cli.json, status, labels, types, slug),
        Commands::Ready {
            labels,
            types,
            limit,
            include_claimed,
            claim,
        } => run_ready(cli.json, labels, types, limit, include_claimed, claim),
        Commands::Close { id } => run_set_status(cli.json, id, Status::Closed),
        Commands::Open { id } => run_set_status(cli.json, id, Status::Open),
        Commands::Comment { id, file, author } => run_comment(cli.json, id, file, author),
        Commands::Label { action } => match action {
            LabelAction::Add { id, label } => {
                run_label(cli.json, id, label, LabelOp::Add)
            }
            LabelAction::Rm { id, label } => run_label(cli.json, id, label, LabelOp::Rm),
        },
        Commands::Remote { action } => match action {
            RemoteAction::Add { name, url } => run_remote_add(cli.json, name, url),
            RemoteAction::Ls => run_remote_ls(cli.json),
            RemoteAction::Rm { name } => run_remote_rm(cli.json, name),
        },
        Commands::Push { remote } => run_push(cli.json, remote),
        Commands::Pull { remote } => run_pull(cli.json, remote),
        Commands::Update {
            id,
            title,
            status,
            r#type,
            slug,
            unset_slug,
            body_file,
            assignee,
            unset_assignee,
            claim,
            unclaim,
        } => run_update(
            cli.json,
            id,
            title,
            status,
            r#type,
            slug,
            unset_slug,
            body_file,
            assignee,
            unset_assignee,
            claim,
            unclaim,
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

/// `jjf init` — wrap `Storage::init` against the cwd. Idempotent;
/// emits either a one-line success message or, with `--json`, the
/// ticket-spec `{"ok": true, "bookmark": "issues"}`.
fn run_init(json: bool) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    // Refuse to run from inside the jjforge source repo (colocate
    // drift guard — see preflight::refuse_self_hosted_write). Init is
    // a mutating verb: it runs the 4-CLI seed dance, which flips git
    // HEAD onto refs/jj/root in a colocated repo. `JJF_ALLOW_SELF_HOST=1`
    // bypasses with a loud stderr line.
    let cwd_canon = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::refuse_self_hosted_write(&cwd_canon, json)?;
    Storage::init(&cwd)?;
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
        println!("jjf: initialized bookmark `{ISSUES_BOOKMARK}`");
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
    assignee: Option<String>,
    type_arg: Option<TypeArg>,
    slug: Option<String>,
) -> Result<(), CliError> {
    // 1. Parse dep ids first — purely-local validation, no IO.
    let deps: Vec<IssueId> = deps
        .into_iter()
        .map(|raw| {
            IssueId::parse(&raw).map_err(|error| CliError::BadDepId { value: raw, error })
        })
        .collect::<Result<Vec<_>, _>>()?;

    // 2. Read the body. `-F -` is stdin; `-F <path>` is the file's
    // bytes; omitted is empty. We deliberately preserve raw bytes — no
    // trim, no newline normalization — so round-trip stays exact.
    let body = read_body(file.as_deref())?;

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

    // 3. Resolve the cwd as an absolute path. `Storage::open` requires
    // absolute; we canonicalize so symlinks in the path don't bite us.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 4. Preflight: refuse to run from inside the jjforge source repo
    // (colocate drift guard — see `preflight::refuse_self_hosted_write`).
    // Runs FIRST among the preflights so an operator inside the source
    // tree gets the actionable "use a sibling working dir" message
    // rather than a generic `MissingIssuesBookmark` (when they haven't
    // run `jjf init` in that scratch dir yet, which is the common case
    // since `jjf init` is also guarded).
    preflight::refuse_self_hosted_write(&cwd, json)?;

    // 5. Preflight: we're inside a jj repo AND the `issues` bookmark
    // exists. The storage layer doesn't distinguish missing-bookmark
    // today (see follow-ups in the cli-new/cli-show closing comments);
    // doing the probe here keeps the user-facing error precise without
    // expanding the storage API. Implementation lives in `preflight`
    // so the read verbs share the same code.
    preflight::issues_bookmark(&cwd)?;

    // 6. Hand the draft to storage.
    let storage = Storage::open(&cwd)?;
    let draft = IssueDraft {
        title,
        body,
        labels,
        dependencies: deps,
        assignee,
        type_: type_arg.map(IssueType::from),
        slug,
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

    // 3. Preflight cwd + bookmark + self-host guard.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::refuse_self_hosted_write(&cwd, json)?;
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
    preflight::refuse_self_hosted_write(&cwd, json)?;
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
    let assignee = issue.assignee.as_deref().unwrap_or("(none)");
    println!("assignee: {assignee}");
    let deps = if issue.dependencies.is_empty() {
        "(none)".to_owned()
    } else {
        issue
            .dependencies
            .iter()
            .map(|d| d.as_str().to_owned())
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!("dependencies: {deps}");
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

    // 2. Preflight: refuse to run from the jjforge source repo
    // (colocate drift guard). See `preflight::refuse_self_hosted_write`.
    preflight::refuse_self_hosted_write(&cwd, json)?;

    // 3. Preflight: jj repo + `issues` bookmark present.
    preflight::issues_bookmark(&cwd)?;

    // 4. Open storage, resolve the handle (`id`-or-`slug`), then
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
        };
        println!("{verb} {issue_id}");
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

    // 3. Preflight: refuse to run from the jjforge source repo
    // (colocate drift guard). See `preflight::refuse_self_hosted_write`.
    preflight::refuse_self_hosted_write(&cwd, json)?;

    // 4. Preflight: jj repo + `issues` bookmark present.
    preflight::issues_bookmark(&cwd)?;

    // 5. Open storage, resolve handle (`id`-or-`slug`), then hand off.
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

/// `jjf remote add <name> <url>` — wrap `jj git remote add <name>
/// <url>` against the cwd's jj repo.
///
/// jj does the actual remote-add work; we translate the two specific
/// error stderrs we recognize (`already exists`, anything else) into
/// typed `CliError` variants so `kind()` stays stable. URL syntax
/// validation is jj's responsibility — we accept what it accepts and
/// surface its rejection unchanged.
///
/// Preflight is jj-repo-only (no `issues` bookmark required), because
/// adding a remote is meaningful before `jjf init` runs.
fn run_remote_add(json: bool, name: String, url: String) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::jj_repo(&cwd)?;

    let out = std::process::Command::new("jj")
        .arg("--repository")
        .arg(&cwd)
        .args(["git", "remote", "add", &name, &url])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // jj's canonical phrase: `Error: Git remote named '<name>' already exists`.
        if stderr.contains("already exists") {
            return Err(CliError::RemoteAlreadyExists(name));
        }
        return Err(CliError::JjGitRemote(stderr.trim().to_owned()));
    }

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

/// `jjf remote ls` — wrap `jj git remote list` and re-render its
/// output as tab-separated `<name>\t<url>` lines.
///
/// jj's own output uses SPACE as the column separator; we re-render
/// because every other `ls`-style verb in jjforge emits tab-separated
/// columns, and a stable separator means downstream `cut -f1` /
/// `awk -F'\t'` pipelines don't have to guess at column widths.
///
/// `--json` emits a JSON array of `{name, url}` objects. Empty result
/// is `[]` (per the same `ls` / `show` convention — scripts piping to
/// `jq length` get a useful value), and empty plain-text output is
/// silence (zero lines), not a header.
fn run_remote_ls(json: bool) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::jj_repo(&cwd)?;

    let out = std::process::Command::new("jj")
        .arg("--repository")
        .arg(&cwd)
        .args(["git", "remote", "list"])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        return Err(CliError::JjGitRemote(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let remotes: Vec<(String, String)> = stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            // jj emits `<name> <url>` — split on the FIRST whitespace
            // run so URLs containing spaces (rare but possible for
            // local paths on weirdly-named directories) stay intact.
            let (name, url) = line.split_once(char::is_whitespace)?;
            Some((name.to_owned(), url.trim().to_owned()))
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

/// `jjf remote rm <name>` — wrap `jj git remote remove <name>`.
///
/// Note: jj also forgets bookmarks tracked from the removed remote
/// (that's jj's behavior, not ours — it's why the underlying command
/// is `remove`, not `rm`). Documented in the help text so a user
/// stripping a remote after a pull doesn't get a surprise.
///
/// Preflight + error mapping mirror `run_remote_add`. Stderr matching
/// on `No git remote named` is the typed `RemoteNotFound` (exit 2);
/// anything else falls through to `JjGitRemote` (exit 1).
fn run_remote_rm(json: bool, name: String) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::jj_repo(&cwd)?;

    let out = std::process::Command::new("jj")
        .arg("--repository")
        .arg(&cwd)
        .args(["git", "remote", "remove", &name])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // jj's canonical phrase: `Error: No git remote named '<name>'`.
        if stderr.contains("No git remote named") {
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
    body_file: Option<PathBuf>,
    assignee: Option<String>,
    unset_assignee: bool,
    claim: bool,
    unclaim: bool,
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
    let fields = UpdateFields {
        title,
        slug: slug_field,
        status: status.map(Status::from),
        type_: type_arg.map(|t| Some(IssueType::from(t))),
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

    // 4. Preflight: refuse to run from the jjforge source repo
    // (colocate drift guard). See `preflight::refuse_self_hosted_write`.
    preflight::refuse_self_hosted_write(&cwd, json)?;

    // 5. Preflight: jj repo + `issues` bookmark present.
    preflight::issues_bookmark(&cwd)?;

    // 6. Open storage, resolve handle.
    let storage = Storage::open(&cwd)?;
    let issue_id = resolve_handle(&storage, &id)?;

    // 6a. `--claim` / `--unclaim` take the direct storage path. Clap
    // already enforces mutual exclusion with the field-level flags
    // (status/assignee/unset-assignee), so by the time we land here
    // `fields.is_empty()` is true and the only branch a user could
    // possibly want is the atomic claim verb.
    if claim {
        let who = resolve_current_user()?;
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

    // 2. Resolve + canonicalize cwd.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 3. Preflight: refuse to run from the jjforge source repo
    // (colocate drift guard). See `preflight::refuse_self_hosted_write`.
    preflight::refuse_self_hosted_write(&cwd, json)?;

    // 4. Preflight: jj repo + `issues` bookmark present. We run this
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

/// Resolve the current jj user's `user.name` for `--claim`.
/// Returns the trimmed value or [`CliError::NoCurrentUser`] when
/// `jj config get user.name` is unset / empty. Differs from
/// [`resolve_author`] in that it doesn't synthesize `Name <email>`
/// — claims are short identity strings stored in `assignee`, not
/// authorship strings stored in `comments.jsonl`. v2.3
/// (`agent-claim-atomic`).
fn resolve_current_user() -> Result<String, CliError> {
    let name = jj_config_get("user.name")?;
    match name {
        Some(n) => Ok(n),
        None => Err(CliError::NoCurrentUser),
    }
}

/// Resolve the comment author. Returns the caller's `--author` override
/// when present and non-empty; otherwise synthesizes `Name <email>`
/// from `jj config get user.name` + `jj config get user.email`.
///
/// Format matches jj's `author` commit-template field (`Name <email>`)
/// so a comment author and the surrounding commit's `author` line stay
/// canonically identical for history walks.
///
/// Edge cases:
/// - Override is empty / whitespace → `MissingAuthor`.
/// - `user.name` is unset (or empty) → `MissingAuthor`.
/// - `user.name` is set but `user.email` is unset → return just the
///   `name`. This matches the spirit of jj's own behavior (it'll let
///   you commit with just a name) but means the resulting author
///   string won't have the `<email>` suffix that `read_history`'s
///   per-commit `author` typically carries. Worth a follow-up to
///   canonicalize one way or the other.
fn resolve_author(override_name: Option<String>) -> Result<String, CliError> {
    if let Some(name) = override_name {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(CliError::MissingAuthor);
        }
        return Ok(trimmed.to_owned());
    }
    let name = jj_config_get("user.name")?;
    let Some(name) = name else {
        return Err(CliError::MissingAuthor);
    };
    let email = jj_config_get("user.email")?;
    Ok(match email {
        Some(email) => format!("{name} <{email}>"),
        None => name,
    })
}

/// Shell out to `jj config get <key>` and return the trimmed value, or
/// `None` if the key isn't configured. Any other failure (binary not
/// on PATH, unexpected stderr) surfaces as a `Probe` error.
///
/// `jj config get` exits non-zero when the key is absent — we treat
/// that specific case as "not configured" rather than a hard probe
/// failure so the caller can decide what to do.
fn jj_config_get(key: &str) -> Result<Option<String>, CliError> {
    let out = std::process::Command::new("jj")
        .args(["config", "get", key])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        // jj prints `config error: ... is not defined` (or similar) and
        // exits non-zero when the key is missing. Treat any non-success
        // here as "not configured" — the verb falls back accordingly,
        // and if the real failure was something else (e.g. malformed
        // config file) the user will hit it on the next jj invocation
        // with a clearer message than we could synthesize.
        return Ok(None);
    }
    let val = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if val.is_empty() { Ok(None) } else { Ok(Some(val)) }
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
    types: Vec<TypeArg>,
    slug: Option<String>,
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
        if !types_match(&issue, &wanted_types) {
            continue;
        }
        if !slug_matches(&issue, slug.as_deref()) {
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
        // (CLAUDE.md). label-count is rendered with a trailing `L` so
        // an eyeball can tell `3L` (three labels) apart from a numeric
        // column that might mean comments or something else later.
        for issue in &issues {
            let status_s = issue.status.as_str();
            println!(
                "{id}\t{status}\t{n}L\t{title}",
                id = issue.id,
                status = status_s,
                n = issue.labels.len(),
                title = issue.title,
            );
        }
    }
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
    claim: bool,
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
    // --claim is a mutating shape, so it also gets the self-host
    // write guard (otherwise we'd silently drift git HEAD in the
    // source repo).
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    if claim {
        preflight::refuse_self_hosted_write(&cwd, json)?;
    }
    preflight::issues_bookmark(&cwd)?;

    let storage = Storage::open(&cwd)?;
    let filter = ReadyFilter {
        labels,
        types: types.into_iter().map(IssueType::from).collect(),
        limit,
        include_claimed,
    };
    let issues = storage.list_ready(&filter)?;

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
                let who = resolve_current_user()?;
                storage.claim(&issue.id, &who)?;
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
        for issue in &issues {
            let status_s = issue.status.as_str();
            println!(
                "{id}\t{status}\t{n}L\t{title}",
                id = issue.id,
                status = status_s,
                n = issue.labels.len(),
                title = issue.title,
            );
        }
    }
    Ok(())
}

/// `--status` predicate. `All` matches everything.
fn status_matches(issue: &Issue, filter: StatusFilter) -> bool {
    match filter {
        StatusFilter::All => true,
        StatusFilter::Open => issue.status == Status::Open,
        StatusFilter::InProgress => issue.status == Status::InProgress,
        StatusFilter::Closed => issue.status == Status::Closed,
    }
}

/// `--label` predicate. Empty filter matches every issue. A non-empty
/// filter requires the issue to carry EVERY listed label (intersection).
fn labels_match(issue: &Issue, wanted: &[String]) -> bool {
    wanted.iter().all(|w| issue.labels.iter().any(|l| l == w))
}

/// `--type` predicate. Empty filter matches every issue. A non-empty
/// filter requires the issue's type to equal AT LEAST ONE listed
/// type (union). Mirrors the OR-semantics behavior the ticket calls
/// out, distinct from `--label`'s AND.
fn types_match(issue: &Issue, wanted: &[IssueType]) -> bool {
    wanted.is_empty() || wanted.iter().any(|t| *t == issue.type_)
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

/// `jjf push <remote>` — shell out to `jj git push --bookmark issues
/// --remote <remote>` and translate known failure modes to typed
/// errors.
///
/// jj's stderr for the relevant cases (observed against jj 0.40):
/// - unknown remote: `Error: No git remote named '<name>'`.
/// - network unreachable: contains "could not resolve" /
///   "Connection refused" / "Failed to connect" / "No such device or
///   address".
/// - auth: contains "authentication" / "access denied" / "permission
///   denied" / "could not read Username" / "401".
/// - non-fast-forward / hook rejection: contains "Refusing to push" /
///   "rejected" / "non-fast-forward".
///
/// Anything else falls through to `jj_git_push_error` with jj's
/// stderr verbatim in the message so the operator can diagnose.
///
/// Preflight: full `issues_bookmark` probe — the bookmark must exist
/// locally for there to be anything to push.
fn run_push(json: bool, remote: String) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    // Refuse to run from the jjforge source repo (colocate drift guard).
    // Push doesn't directly drive the 4-CLI dance, but it's grouped
    // with the other mutating verbs for consistency and because a
    // future jj release could move `@` during push (jj has changed
    // working-copy-touching semantics across versions before).
    preflight::refuse_self_hosted_write(&cwd, json)?;
    preflight::issues_bookmark(&cwd)?;

    let out = std::process::Command::new("jj")
        .arg("--repository")
        .arg(&cwd)
        .args(["git", "push", "--bookmark", "issues", "--remote", &remote])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(classify_push_error(&remote, stderr));
    }

    if json {
        let out = serde_json::json!({
            "ok": true,
            "remote": &remote,
            "bookmark": jjf_storage::ISSUES_BOOKMARK,
        });
        println!("{out}");
    } else {
        println!("pushed issues -> {remote}");
    }
    Ok(())
}

/// Map jj-git-push stderr to a typed `CliError`. Keeps the
/// substring-matching out of `run_push` proper so the dispatch logic
/// stays scannable and so the matcher can be unit-tested directly.
fn classify_push_error(remote: &str, stderr: String) -> CliError {
    // Unknown remote — jj's canonical phrase. The `remote rm` verb's
    // mapper uses the same phrase; we reuse the kind.
    if stderr.contains("No git remote named") {
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
    // first then retry"; the message embeds that hint.
    if lower.contains("refusing to push")
        || lower.contains("rejected")
        || lower.contains("non-fast-forward")
        || lower.contains("non fast-forward")
    {
        return CliError::PushRejected {
            remote: remote.to_owned(),
            stderr,
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

/// `jjf pull <remote>` — fetch the remote, track the `issues@<remote>`
/// bookmark if needed, then resolve any divergence in op-space.
///
/// See the verb's doc-comment on `Commands::Pull` for the high-level
/// flow. This function is the orchestrator; it shells out to `jj`
/// directly (mirroring `run_push` / `run_remote_*`) and calls the
/// storage layer's `resolve_divergence` + `record_merge_op_space`
/// primitives for the merge pass.
///
/// As of `bfc732b` (sync-conflict-fallback), this verb no longer uses
/// the v1 file-bytes merge driver (`jjf_merge::resolve`). The op-space
/// resolver replays each head's op chain field-by-field per spec §6's
/// LWW ordering tuple, then re-renders the merged file as a
/// deterministic projection of the op stream. There is no human-surface
/// "unmergeable" failure mode in this path — every divergence resolves.
/// The `Unmergeable` / `CommentFileConflict` error kinds stay wired
/// (they're reachable from external callers of `jjf_merge::resolve` and
/// from the storage layer) but cannot arise from this v2 operator pull.
fn run_pull(json: bool, remote: String) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    // `pull` uses the jj-repo-only preflight: a fresh clone has
    // `issues@<remote>` but no local `issues` yet, and `pull` is
    // precisely the verb that materializes the local bookmark via
    // `jj bookmark track`. Requiring the bookmark up front would
    // force an awkward `jjf init` on a clone that already has the
    // bookmark server-side. `push`, by contrast, requires the local
    // bookmark (there's nothing to push without it).
    //
    // Refuse to run from the jjforge source repo (colocate drift guard).
    // Pull can land a merge commit via the 4-CLI dance on divergence;
    // that path absolutely drifts `@` in a colocated repo.
    preflight::refuse_self_hosted_write(&cwd, json)?;
    preflight::jj_repo(&cwd)?;

    // 1. Fetch. Map known failures the same way push does.
    let out = std::process::Command::new("jj")
        .arg("--repository")
        .arg(&cwd)
        .args(["git", "fetch", "--remote", &remote])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(classify_fetch_error(&remote, stderr));
    }

    // 2. Probe for remote-bookmark presence. `jj bookmark list
    // --all-remotes -T 'name ++ "@" ++ remote ++ "\n"' issues` lists
    // one line per (local + each remote) view of the bookmark. If
    // `issues@<remote>` is absent, the other side hasn't pushed yet —
    // not an error.
    let bm_out = std::process::Command::new("jj")
        .arg("--repository")
        .arg(&cwd)
        .args([
            "bookmark",
            "list",
            "--all-remotes",
            "-T",
            "name ++ \"@\" ++ remote ++ \"\\n\"",
            "issues",
        ])
        .output()
        .map_err(CliError::Probe)?;
    if !bm_out.status.success() {
        return Err(CliError::Probe(std::io::Error::other(format!(
            "jj bookmark list failed: {}",
            String::from_utf8_lossy(&bm_out.stderr)
        ))));
    }
    let bm_text = String::from_utf8_lossy(&bm_out.stdout);
    let remote_marker = format!("issues@{remote}");
    let remote_present = bm_text.lines().any(|l| l.trim() == remote_marker);

    if !remote_present {
        // No remote bookmark — fetch landed nothing for us. Exit 0;
        // tests pin this case so callers can distinguish "first push
        // hasn't happened" from network failure (also exit 0 would
        // mask) by inspecting `remote_present` in the JSON envelope.
        emit_pull_success(json, &remote, false, 0);
        return Ok(());
    }

    // 3. Track-if-absent. The first fetch on a fresh clone leaves
    // `issues@<remote>` untracked; subsequent fetches see the bookmark
    // as new remote bookmarks every time. We want it tracked so a
    // divergent edit shows up as the conflicted-bookmark state we're
    // here to resolve. `jj bookmark track` is idempotent in spirit —
    // it returns success-ish when already tracked, but its stderr
    // when "already tracked" is harmless; we treat any non-success
    // that mentions "already tracked" as success and surface
    // everything else as a generic probe error.
    let track_out = std::process::Command::new("jj")
        .arg("--repository")
        .arg(&cwd)
        .args(["bookmark", "track", &remote_marker])
        .output()
        .map_err(CliError::Probe)?;
    if !track_out.status.success() {
        let stderr = String::from_utf8_lossy(&track_out.stderr);
        if !stderr.contains("already tracked")
            && !stderr.contains("is already tracking")
        {
            return Err(CliError::Probe(std::io::Error::other(format!(
                "jj bookmark track failed: {stderr}"
            ))));
        }
    }

    // 4. Probe for divergence. `heads(bookmarks(issues))` returns one
    // change per head; >1 means the bookmark is in the "conflicted"
    // state our investigation in `experiments/sync-remote/` documented.
    let storage = Storage::open(&cwd)?;
    let heads = storage.issues_heads()?;
    if heads.len() < 2 {
        // Clean fetch — either nothing changed remotely (fast-forward
        // already done) or there was no local divergence to resolve.
        emit_pull_success(json, &remote, true, 0);
        return Ok(());
    }

    // 5. Op-space resolution. Walk each head's op chain per issue,
    // reduce field-by-field per spec §6's LWW ordering tuple, and
    // render the merged record + comments. No probe-merge commit is
    // needed: the op-space driver reads pristine bytes from each head
    // via `jj file show -r <head>` (see `crates/jjf-storage/src/
    // merge_ops.rs`) and never touches the working copy with conflict
    // markers. Body bytes come from whichever head's rendered file
    // matches the winning `SetBody` op's `body_hash` (the §5.2
    // body-hash join).
    let report = storage.resolve_divergence()?;

    if report.issues.is_empty() {
        // issues_heads said >=2 heads but no head touched any issue
        // file we recognized. Defensive: in v1 storage every head
        // exists because of an issue-mutating commit, so this branch
        // is mostly unreachable. We still need to pin the bookmark so
        // it stops being conflicted. Mirror the old clean-merge dance:
        // jj-new across the heads, set the bookmark, step off.
        let merge_args = {
            let mut v: Vec<&str> = vec!["new"];
            for h in &heads {
                v.push(h.as_str());
            }
            v.push("-m");
            v.push("jjf: empty merge (no issue files touched)");
            v
        };
        let merge_out = std::process::Command::new("jj")
            .arg("--repository")
            .arg(&cwd)
            .args(&merge_args)
            .output()
            .map_err(CliError::Probe)?;
        if !merge_out.status.success() {
            return Err(CliError::Probe(std::io::Error::other(format!(
                "jj new (empty merge) failed: {}",
                String::from_utf8_lossy(&merge_out.stderr)
            ))));
        }
        let bm_set = std::process::Command::new("jj")
            .arg("--repository")
            .arg(&cwd)
            .args([
                "bookmark",
                "set",
                "issues",
                "-r",
                "@",
                "--allow-backwards",
            ])
            .output()
            .map_err(CliError::Probe)?;
        if !bm_set.status.success() {
            return Err(CliError::Probe(std::io::Error::other(format!(
                "jj bookmark set (clean-merge) failed: {}",
                String::from_utf8_lossy(&bm_set.stderr)
            ))));
        }
        let step = std::process::Command::new("jj")
            .arg("--repository")
            .arg(&cwd)
            .args(["new", "root()"])
            .output()
            .map_err(CliError::Probe)?;
        if !step.status.success() {
            return Err(CliError::Probe(std::io::Error::other(format!(
                "jj new root() (clean-merge step-off) failed: {}",
                String::from_utf8_lossy(&step.stderr)
            ))));
        }
        emit_pull_success(json, &remote, true, 0);
        return Ok(());
    }

    // Non-empty report: land the multi-parent merge commit with one
    // `Jjf-Op: merge` trailer per resolved issue (spec §5.7) and write
    // the merged record + comments files. The storage primitive owns
    // the 4-CLI dance.
    let count = report.issues.len();
    storage.record_merge_op_space(&heads, &report)?;
    emit_pull_success(json, &remote, true, count);
    Ok(())
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

/// Emit the success path for `jjf pull`. Kept as a helper so all four
/// success branches (no-remote-bookmark, clean-fetch-no-divergence,
/// clean-merge-no-resolution, real-merge-with-resolution) render the
/// same envelope shape with one shared call site.
///
/// The `resolved_issues` field replaces the older `merged_files` (the
/// shape difference reflects the v1→v2 switch from a file-bytes driver
/// to an op-space resolver, where the unit of resolution is an issue,
/// not a file). The `merge_strategy` field pins which driver ran so
/// downstream consumers can branch on the contract.
fn emit_pull_success(json: bool, remote: &str, remote_present: bool, resolved_issues: usize) {
    if json {
        let mut obj = serde_json::Map::new();
        obj.insert("ok".into(), serde_json::Value::Bool(true));
        obj.insert("remote".into(), serde_json::Value::String(remote.to_owned()));
        obj.insert(
            "bookmark".into(),
            serde_json::Value::String(jjf_storage::ISSUES_BOOKMARK.to_owned()),
        );
        obj.insert(
            "remote_present".into(),
            serde_json::Value::Bool(remote_present),
        );
        obj.insert(
            "merge_strategy".into(),
            serde_json::Value::String("op_space".into()),
        );
        obj.insert(
            "resolved_issues".into(),
            serde_json::Value::from(resolved_issues),
        );
        let envelope = serde_json::Value::Object(obj);
        println!("{envelope}");
    } else if !remote_present {
        println!("pulled {remote}: no issues bookmark on remote yet");
    } else if resolved_issues == 0 {
        println!("pulled issues <- {remote}");
    } else {
        println!(
            "pulled issues <- {remote}; resolved {resolved_issues} issue(s) op-space"
        );
    }
}
