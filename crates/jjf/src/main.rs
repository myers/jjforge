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
//! output is `{"ok": true, "bookmark": "bugs"}` per the
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
    BUGS_BOOKMARK, Bug, BugDraft, BugId, Error as StorageError, IdError, Status, Storage,
    UpdateFields,
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
/// equivalent — the `Status` enum only has `Open` / `Closed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum StatusFilter {
    Open,
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
    Closed,
}

impl From<StatusArg> for Status {
    fn from(s: StatusArg) -> Self {
        match s {
            StatusArg::Open => Status::Open,
            StatusArg::Closed => Status::Closed,
        }
    }
}

/// Every verb the epic body (`c4f7fcb`) calls out, plus `init`. Stubs
/// exist so `--help` lists the full surface from day one; later
/// per-verb tickets replace the stubs with real implementations.
#[derive(Debug, Subcommand)]
enum Commands {
    /// Initialize the `bugs` bookmark on the current jj repo.
    /// Idempotent — running twice in the same repo is a no-op.
    Init,

    /// Create a new bug on the `bugs` bookmark. Requires `jjf init` to
    /// have been run first. Prints the new bug's id on stdout (or the
    /// `{"ok": true, "id": "..."}` object under `--json`); exits 0.
    New {
        /// Title of the new bug. Required, non-empty.
        #[arg(short = 't', long)]
        title: String,

        /// Source for the bug body. Path to read, or `-` to read stdin.
        /// Omit to leave the body empty (the epic's "no prompts ever"
        /// rule — no editor pop-up).
        #[arg(short = 'F', long)]
        file: Option<PathBuf>,

        /// Attach a label. Repeatable (`-l bug -l p1`).
        #[arg(short = 'l', long = "label")]
        labels: Vec<String>,

        /// Declare a dependency on another bug id. Repeatable. Each
        /// value must be a 7-char lowercase-hex bug id; a bad value is
        /// a preflight failure (exit 2).
        #[arg(short = 'd', long = "dep")]
        deps: Vec<String>,

        /// Set the assignee. Optional; omit to leave the field unset
        /// (creates a record with `assignee: null`).
        #[arg(short = 'a', long)]
        assignee: Option<String>,
    },

    /// Print a single bug from the `bugs` bookmark — title, status,
    /// labels, assignee, body, and comment thread. Plain-text by
    /// default; `--json` emits the structured `Bug` record verbatim
    /// (no envelope — the bug IS the payload). Requires `jjf init`
    /// to have been run first.
    Show {
        /// Full 7-char hex bug id. Prefix lookup isn't supported yet
        /// (the storage layer is full-id-only); a bad id is a
        /// preflight failure (exit 2), a valid id that doesn't exist
        /// at the bookmark tip is a runtime failure (exit 1).
        id: String,
    },

    /// List bugs from the `bugs` bookmark, with optional filters.
    /// Default: every open bug. Plain-text output is one row per bug,
    /// tab-separated columns (`<id-7>\t<status>\t<labels>L\t<title>`),
    /// no header, sorted newest-first by `created_at`. `--json` emits
    /// a JSON array of `Bug` records (the same shape `show --json`
    /// emits per element). Empty result is exit 0 with no output.
    Ls {
        /// Filter by status. `open` is the default (matches git-bug and
        /// the "lists are about what's actionable" convention). `all`
        /// shows every bug regardless of status.
        #[arg(long, value_enum, default_value_t = StatusFilter::Open)]
        status: StatusFilter,

        /// Filter by label. Repeatable. Semantics: AND — a bug must
        /// carry every listed label to match.
        #[arg(short = 'l', long = "label")]
        labels: Vec<String>,
    },

    /// Mutate one or more scalar fields of a bug in a single commit.
    ///
    /// Every populated field flag lands as a `Jjf-Op:` trailer on ONE
    /// new commit on the `bugs` bookmark (spec §5.5 multi-op-per-commit).
    /// So `update <id> --title T --status closed --body-file -` ships
    /// three trailers (`set-title`, `set-status`, `set-body`) on one
    /// commit — distinct from running three sibling verbs back-to-back,
    /// which would fragment into three commits.
    ///
    /// At least one of `--title` / `--status` / `--body-file` /
    /// `--assignee` / `--unset-assignee` is required; running with none
    /// is an exit-2 preflight failure (clap can't enforce the
    /// at-least-one rule for us). `--assignee` and `--unset-assignee`
    /// are mutually exclusive (clap `conflicts_with`).
    ///
    /// `--status` overlaps with `jjf close` / `jjf open` by design — use
    /// the standalone verbs for the single-shot ergonomic path, this
    /// verb for the multi-field case.
    Update {
        /// Full 7-char hex bug id. Bad parse → exit 2; valid id that
        /// doesn't exist on the bookmark → exit 1.
        id: String,

        /// Replace the title. Must be non-empty (after trim) at the
        /// storage layer.
        #[arg(long)]
        title: Option<String>,

        /// Replace the status. Use `open` or `closed`.
        #[arg(long, value_enum)]
        status: Option<StatusArg>,

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
    },

    /// Append a comment to an existing bug on the `bugs` bookmark.
    /// Body source is REQUIRED — pass `-F <path>` or `-F -` for stdin.
    /// Author defaults to the jj user identity (`Name <email>` per jj's
    /// `author` template); `--author <NAME>` overrides. Empty bodies
    /// are rejected at the CLI layer (exit 2) because an empty comment
    /// is almost certainly a user mistake.
    Comment {
        /// Full 7-char hex bug id. Bad parse → exit 2; valid id that
        /// doesn't exist on the bookmark → exit 1.
        id: String,

        /// Source for the comment body. Path to read, or `-` to read
        /// stdin. REQUIRED — the epic's "no prompts ever" rule means we
        /// do NOT launch an editor when this is omitted. Empty body
        /// (after read) is a preflight failure (exit 2).
        #[arg(short = 'F', long, required = true)]
        file: PathBuf,

        /// Override the comment author. Free-form string written
        /// verbatim into the comment record. When omitted, the author
        /// is sourced from `jj config get user.name` + `user.email` in
        /// the `Name <email>` format that matches jj's commit-author
        /// template. If no jj `user.name` is configured and no override
        /// is given, the verb exits 2 with a hint to set one.
        #[arg(long)]
        author: Option<String>,
    },

    /// Close a bug. Lands a `set-status` op on a new commit on the
    /// `bugs` bookmark. Not idempotent per the spec — closing an
    /// already-closed bug still writes a fresh trailer so the audit
    /// log records the intent. Requires `jjf init` to have been run
    /// first.
    Close {
        /// Full 7-char hex bug id. A bad parse is a preflight failure
        /// (exit 2); a well-formed id that doesn't exist on the
        /// bookmark is a runtime failure (exit 1).
        id: String,
    },

    /// Reopen a bug. Same shape and same non-idempotency rules as
    /// `close`, just lands `set-status=open`.
    Open {
        /// Full 7-char hex bug id. A bad parse is a preflight failure
        /// (exit 2); a well-formed id that doesn't exist on the
        /// bookmark is a runtime failure (exit 1).
        id: String,
    },

    /// Add or remove a single label on a bug. Lands a fresh
    /// `label-add` or `label-rm` op on a new commit on the `bugs`
    /// bookmark.
    ///
    /// Per the spec (§5.2) and matching `close`/`open`'s twin-mutator
    /// shape: the call is NOT idempotent — re-adding an already-present
    /// label, or removing one that isn't there, still writes a fresh
    /// trailer so the audit log records the intent. The in-memory
    /// label set is dedup'd, so `show` reports a clean list either way.
    ///
    /// v1 is single-label-per-call. Bulk (`label add <id> a b c`) is
    /// out of scope; repeat the command in a loop for now.
    Label {
        #[command(subcommand)]
        action: LabelAction,
    },
}

/// Inner enum for `jjf label <action>`. Separating the action from the
/// outer verb keeps the clap-derive `--help` clean (one help page per
/// add/rm rather than two flag combinations on one verb) and gives
/// `cli-update`'s scalar fan-out a pattern to copy if it wants nested
/// subcommands instead of flags.
#[derive(Debug, Subcommand)]
enum LabelAction {
    /// Add a label to a bug. Idempotent at the record level (the label
    /// set dedupes) but NOT at the commit level — a fresh `label-add`
    /// op lands either way per spec §5.2.
    Add {
        /// Full 7-char hex bug id. Bad parse → exit 2; valid id that
        /// doesn't exist on the bookmark → exit 1.
        id: String,

        /// Label to add. Must be non-empty; an empty string is a
        /// preflight failure (exit 2) at the CLI layer because the
        /// storage layer doesn't validate it.
        label: String,
    },

    /// Remove a label from a bug. No-op at the record level if the
    /// label isn't present, but a fresh `label-rm` op lands either way
    /// per spec §5.2.
    Rm {
        /// Full 7-char hex bug id. Bad parse → exit 2; valid id that
        /// doesn't exist on the bookmark → exit 1.
        id: String,

        /// Label to remove. Must be non-empty (same rule as `add`).
        label: String,
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

    /// Reading the bug body from `-F <path>` (or `-F -`) failed.
    /// Preflight failure: the user gave us a path we couldn't open
    /// (or stdin closed in a way we couldn't drain).
    #[error("could not read body from {from}: {error}")]
    BodyRead {
        from: String,
        error: std::io::Error,
    },

    /// A `-d / --dep` value didn't parse as a valid `BugId`.
    /// Preflight failure (exit 2) — the user typed something wrong;
    /// no point in starting the dance only to fail mid-write.
    #[error("invalid bug id for --dep {value:?}: {error}")]
    BadDepId { value: String, error: IdError },

    /// A positional bug id (e.g. `jjf show <id>`) didn't parse as
    /// a valid `BugId`. Preflight failure (exit 2) — the user typed
    /// something the storage layer can never resolve.
    #[error("invalid bug id {value:?}: {error}")]
    BadBugId { value: String, error: IdError },

    /// We're inside a jj repo, but the `bugs` bookmark doesn't
    /// exist yet. Surfaced as a preflight (exit 2) so the user gets
    /// a typed signal that they need to run `jjf init` rather than
    /// the raw jj-stderr we'd get from trying to write against an
    /// empty `bookmarks(bugs)` revset.
    #[error("the `bugs` bookmark does not exist in {0}; run `jjf init` first")]
    MissingBugsBookmark(PathBuf),

    /// Probing for the `bugs` bookmark (or for jj-repo-presence)
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
        "nothing to update; pass at least one of --title / --status / --body-file / --assignee / --unset-assignee"
    )]
    NoUpdateFields,
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
            CliError::Cwd(_) => 2,
            CliError::BodyRead { .. } => 2,
            CliError::BadDepId { .. } => 2,
            CliError::BadBugId { .. } => 2,
            CliError::MissingBugsBookmark(_) => 2,
            CliError::EmptyCommentBody => 2,
            CliError::EmptyLabel => 2,
            CliError::MissingAuthor => 2,
            CliError::NoUpdateFields => 2,
            CliError::Probe(_) => 1,
            // `BugNotFound` is the user typing a valid id that just
            // doesn't exist — runtime failure, not preflight (the input
            // was well-formed; we tried to honor it and it wasn't there).
            CliError::Storage(_) => 1,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("jjf: {e}");
            ExitCode::from(e.exit_code())
        }
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
        } => run_new(cli.json, title, file, labels, deps, assignee),
        Commands::Show { id } => run_show(cli.json, id),
        Commands::Ls { status, labels } => run_ls(cli.json, status, labels),
        Commands::Close { id } => run_set_status(cli.json, id, Status::Closed),
        Commands::Open { id } => run_set_status(cli.json, id, Status::Open),
        Commands::Comment { id, file, author } => run_comment(cli.json, id, file, author),
        Commands::Label { action } => match action {
            LabelAction::Add { id, label } => {
                run_label(cli.json, id, label, LabelOp::Add)
            }
            LabelAction::Rm { id, label } => run_label(cli.json, id, label, LabelOp::Rm),
        },
        Commands::Update {
            id,
            title,
            status,
            body_file,
            assignee,
            unset_assignee,
        } => run_update(
            cli.json,
            id,
            title,
            status,
            body_file,
            assignee,
            unset_assignee,
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
/// ticket-spec `{"ok": true, "bookmark": "bugs"}`.
fn run_init(json: bool) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    Storage::init(&cwd)?;
    if json {
        // We hand-build this object rather than using `serde_json::json!`
        // so the dep surface stays as narrow as possible — one tiny
        // object, no macro pulled in, no derive overhead. Field order
        // is fixed by the ticket: `ok` first, `bookmark` second.
        let out = serde_json::json!({
            "ok": true,
            "bookmark": BUGS_BOOKMARK,
        });
        println!("{out}");
    } else {
        println!("jjf: initialized bookmark `{BUGS_BOOKMARK}`");
    }
    Ok(())
}

/// `jjf new -t <title> [-F <path|->] [-l <label>...] [-d <id>...] [-a <name>]`
/// — create one bug on the `bugs` bookmark via the storage write path
/// and emit its id.
///
/// The preflight order matters: we parse the dep ids and read the body
/// BEFORE shelling out to jj, so user-typo / stdin-empty failures don't
/// land any half-state on the bookmark. The bookmark-presence probe
/// then runs against the cwd; if the bookmark is missing we surface a
/// `run jjf init first` message rather than letting the storage layer
/// fail mid-write on an empty `bookmarks(bugs)` revset.
fn run_new(
    json: bool,
    title: String,
    file: Option<PathBuf>,
    labels: Vec<String>,
    deps: Vec<String>,
    assignee: Option<String>,
) -> Result<(), CliError> {
    // 1. Parse dep ids first — purely-local validation, no IO.
    let deps: Vec<BugId> = deps
        .into_iter()
        .map(|raw| {
            BugId::parse(&raw).map_err(|error| CliError::BadDepId { value: raw, error })
        })
        .collect::<Result<Vec<_>, _>>()?;

    // 2. Read the body. `-F -` is stdin; `-F <path>` is the file's
    // bytes; omitted is empty. We deliberately preserve raw bytes — no
    // trim, no newline normalization — so round-trip stays exact.
    let body = read_body(file.as_deref())?;

    // 3. Resolve the cwd as an absolute path. `Storage::open` requires
    // absolute; we canonicalize so symlinks in the path don't bite us.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 4. Preflight: we're inside a jj repo AND the `bugs` bookmark
    // exists. The storage layer doesn't distinguish missing-bookmark
    // today (see follow-ups in the cli-new/cli-show closing comments);
    // doing the probe here keeps the user-facing error precise without
    // expanding the storage API. Implementation lives in `preflight`
    // so the read verbs share the same code.
    preflight::bugs_bookmark(&cwd)?;

    // 5. Hand the draft to storage.
    let storage = Storage::open(&cwd)?;
    let draft = BugDraft {
        title,
        body,
        labels,
        dependencies: deps,
        assignee,
    };
    let id = storage.create_bug(&draft)?;

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

/// Read the bug body per the `-F` flag's contract.
///
/// - `None` — empty body. The epic's "no prompts ever" rule means we do
///   NOT launch an editor when `-F` is omitted; users who want one can
///   pipe it in.
/// - `Some("-")` — read all of stdin, raw bytes. UTF-8 enforced because
///   bug bodies are serialized into a JSON string field.
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

/// `jjf show <id> [--json]` — fetch one bug's structured record from
/// the `bugs` bookmark via `Storage::read` and render it.
///
/// The preflight order matches `run_new`: parse the id, resolve the
/// cwd, probe for the jj repo + `bugs` bookmark, then hand off to the
/// storage layer. Bug-not-found is a runtime failure (exit 1) — the
/// user typed something well-formed, we tried to honor it, and the
/// answer is "no such bug at the bookmark tip."
fn run_show(json: bool, id: String) -> Result<(), CliError> {
    // 1. Parse the id — purely-local validation, no IO. A typo here
    // is a preflight failure (exit 2), distinct from "valid id that
    // doesn't exist" (exit 1).
    let bug_id =
        BugId::parse(&id).map_err(|error| CliError::BadBugId { value: id, error })?;

    // 2. Resolve the cwd. `Storage::open` wants an absolute path;
    // canonicalize so symlinks don't bite.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 3. Preflight the same checks the write path runs. `run jjf
    // init first` is the right error when the bookmark is missing,
    // not a raw jj-stderr.
    preflight::bugs_bookmark(&cwd)?;

    // 4. Hand off to storage. `BugNotFound` flows out as a `Storage`
    // variant of `CliError`, which `exit_code` maps to 1.
    let storage = Storage::open(&cwd)?;
    let bug = storage.read(&bug_id)?;

    // 5. Render.
    if json {
        // The `Bug` struct IS the structured payload — emit it
        // verbatim, no `{"ok": true, ...}` envelope. (`init` and `new`
        // use the envelope because they have no payload beyond a
        // success signal; `show`'s whole job is to expose the record.)
        let s = serde_json::to_string_pretty(&bug)
            .map_err(|e| CliError::Storage(StorageError::Json(e)))?;
        println!("{s}");
    } else {
        print_bug_plain(&bug);
    }
    Ok(())
}

/// Render a bug as human-readable plain text. v1 shape per the
/// `cli-show` ticket — readable and stable, not a contract. If a
/// caller wants machine parsing they should pass `--json`.
fn print_bug_plain(bug: &Bug) {
    let status = match bug.status {
        jjf_storage::Status::Open => "open",
        jjf_storage::Status::Closed => "closed",
    };
    println!("{}  [{}]", bug.id, status);
    println!("{}", bug.title);
    let labels = if bug.labels.is_empty() {
        "(none)".to_owned()
    } else {
        bug.labels.join(", ")
    };
    println!("labels: {labels}");
    let assignee = bug.assignee.as_deref().unwrap_or("(none)");
    println!("assignee: {assignee}");
    let deps = if bug.dependencies.is_empty() {
        "(none)".to_owned()
    } else {
        bug.dependencies
            .iter()
            .map(|d| d.as_str().to_owned())
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!("dependencies: {deps}");
    println!(
        "created: {}   updated: {}",
        bug.created_at, bug.updated_at
    );
    println!();
    // Body verbatim, no rewrap — the writer preserves bytes exactly,
    // and the reader's job is to show them. Add a trailing newline
    // only if the body doesn't already end with one, so two bodies
    // that differ only in trailing newline still render distinctly.
    if !bug.body.is_empty() {
        print!("{}", bug.body);
        if !bug.body.ends_with('\n') {
            println!();
        }
        println!();
    }
    let n = bug.comments.len();
    println!("--- comments ({n}) ---");
    for c in &bug.comments {
        println!("[{}] {}:", c.created_at, c.author);
        print!("{}", c.body);
        if !c.body.ends_with('\n') {
            println!();
        }
        println!();
    }
}

/// `jjf close <id>` / `jjf open <id>` — flip a bug's status via the
/// storage write path. Both verbs differ only in the `Status` value
/// they pass to `Storage::set_status`, so they share one helper.
///
/// Per the spec (and the `cli-status` ticket): closing an
/// already-closed bug (or opening an already-open one) is NOT a no-op
/// — it lands a fresh `set-status` trailer on a new commit. The
/// storage layer enforces this by always calling `mutate` regardless
/// of whether the record actually changed; we just pass the request
/// through.
///
/// Preflight order matches `run_show`: parse the id (exit 2 on bad
/// shape), resolve the cwd, probe for the jj repo + `bugs` bookmark
/// (exit 2 with `run jjf init first` if absent), then hand off to
/// storage. A well-formed id that doesn't exist on the bookmark
/// surfaces as `BugNotFound` and exits 1.
fn run_set_status(json: bool, id: String, status: Status) -> Result<(), CliError> {
    // 1. Parse the id. Same exit-2 rule as `show`.
    let bug_id =
        BugId::parse(&id).map_err(|error| CliError::BadBugId { value: id, error })?;

    // 2. Resolve + canonicalize cwd.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 3. Preflight: jj repo + `bugs` bookmark present.
    preflight::bugs_bookmark(&cwd)?;

    // 4. Hand off to storage.
    let storage = Storage::open(&cwd)?;
    storage.set_status(&bug_id, status)?;

    // 5. Render. The plain-text shape (`closed <id>` / `opened <id>`)
    // is intentionally minimal — one line, no decoration — so it slots
    // cleanly into a shell pipeline. The `--json` envelope mirrors
    // `init` / `new`: `{"ok": true, ...}` plus the verb-specific
    // payload (id + the resulting status).
    let status_word = match status {
        Status::Open => "open",
        Status::Closed => "closed",
    };
    if json {
        let out = serde_json::json!({
            "ok": true,
            "id": bug_id.as_str(),
            "status": status_word,
        });
        println!("{out}");
    } else {
        // Past tense for the human form: `closed <id>` / `opened <id>`.
        // The verb describes the action just performed, not the
        // resulting state — that's `status` in the JSON envelope.
        let verb = match status {
            Status::Open => "opened",
            Status::Closed => "closed",
        };
        println!("{verb} {bug_id}");
    }
    Ok(())
}

/// `jjf label add|rm <id> <label>` — flip one label on a bug via the
/// storage write path. Both arms differ only in which `Storage`
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
/// repo + `bugs` bookmark (exit 2 with `run jjf init first` if
/// absent), then hand off to storage. A well-formed id that doesn't
/// exist on the bookmark surfaces as `BugNotFound` and exits 1.
fn run_label(json: bool, id: String, label: String, op: LabelOp) -> Result<(), CliError> {
    // 1. Parse the id. Same exit-2 rule as `show` / `close`.
    let bug_id =
        BugId::parse(&id).map_err(|error| CliError::BadBugId { value: id, error })?;

    // 2. Reject empty labels at the CLI layer — storage doesn't
    // validate. We trim before the check because a whitespace-only
    // label is almost certainly the same shell-quoting mistake an
    // empty one would be.
    if label.trim().is_empty() {
        return Err(CliError::EmptyLabel);
    }

    // 3. Resolve + canonicalize cwd.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 4. Preflight: jj repo + `bugs` bookmark present.
    preflight::bugs_bookmark(&cwd)?;

    // 5. Hand off to storage. The two mutators have the same signature
    // (`&BugId, &str -> Result<()>`); branch on the action enum.
    let storage = Storage::open(&cwd)?;
    match op {
        LabelOp::Add => storage.add_label(&bug_id, &label)?,
        LabelOp::Rm => storage.remove_label(&bug_id, &label)?,
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
            "id": bug_id.as_str(),
            "label": &label,
            "action": action_word,
        });
        println!("{out}");
    } else {
        println!("label {action_word}: {label} -> {bug_id}");
    }
    Ok(())
}

/// `jjf update <id> [--title T] [--status S] [--body-file PATH|-]
/// [--assignee NAME] [--unset-assignee] [--json]` — mutate one or more
/// scalar fields of a bug in a single commit.
///
/// All populated field flags bundle into ONE `Storage::update` call,
/// which lands ONE new commit on the `bugs` bookmark carrying N
/// `Jjf-Op:` trailers (one per field that changed). This is the
/// multi-op-per-commit dividend the spec §5.5 gives us — running three
/// sibling verbs (e.g. `set-title` + `close` + a separate body update)
/// would fragment into three commits instead.
///
/// Preflight order matches the other write verbs (`run_set_status`,
/// `run_label`, `run_comment`): purely-local validation first
/// (id parse, at-least-one-flag rule, body-file read), then
/// canonicalize cwd, then probe for the jj repo + `bugs` bookmark.
/// Bug-not-found surfaces from `Storage::update` as a `BugNotFound`
/// (exit 1) because the user typed a well-formed id; everything else
/// the user can mistype is exit 2.
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
    body_file: Option<PathBuf>,
    assignee: Option<String>,
    unset_assignee: bool,
) -> Result<(), CliError> {
    // 1. Parse the id. Same exit-2 rule as `show` / `close` / `label`.
    let bug_id =
        BugId::parse(&id).map_err(|error| CliError::BadBugId { value: id, error })?;

    // 2. Build the `UpdateFields` bundle from the flag matrix. The
    // body-file read is done UP FRONT (before the at-least-one check,
    // and before the bookmark probe) so a bogus `--body-file` path
    // surfaces as a typed `BodyRead` error rather than getting masked
    // by a subsequent failure. `--assignee X` => `Some(Some(X))`;
    // `--unset-assignee` => `Some(None)`; neither => `None` (leave
    // alone) — the storage-side `UpdateFields::assignee` is double-
    // wrapped exactly to express this three-way distinction.
    let body = match body_file.as_deref() {
        Some(path) => Some(read_body(Some(path))?),
        None => None,
    };
    let assignee_field: Option<Option<String>> = if unset_assignee {
        Some(None)
    } else {
        assignee.map(Some)
    };
    let fields = UpdateFields {
        title,
        status: status.map(Status::from),
        body,
        assignee: assignee_field,
    };

    // 3. At-least-one-flag rule. Clap can't enforce this (every flag
    // is `Option<_>` / `bool`), so we surface a typed exit-2 hint
    // pointing at the available flags. The storage layer would also
    // reject this with `Error::Invalid`, but the CLI message names
    // the flags so the operator sees what to do next without parsing
    // a generic storage error.
    if fields.is_empty() {
        return Err(CliError::NoUpdateFields);
    }

    // 4. Resolve + canonicalize cwd.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 5. Preflight: jj repo + `bugs` bookmark present.
    preflight::bugs_bookmark(&cwd)?;

    // 6. Hand off to storage. One call lands one commit with N
    // trailers.
    let storage = Storage::open(&cwd)?;
    storage.update(&bug_id, fields.clone())?;

    // 7. Render. The list of field names mirrors the populated fields
    // in field-declaration order (matching the trailer order the
    // storage layer lands). We compute it once so plain-text and JSON
    // agree exactly.
    let changed = changed_field_names(&fields);
    if json {
        let out = serde_json::json!({
            "ok": true,
            "id": bug_id.as_str(),
            "fields": changed,
        });
        println!("{out}");
    } else {
        println!("updated {bug_id}: {}", changed.join(", "));
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
    if fields.status.is_some() {
        out.push("status");
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
/// one comment to an existing bug via the storage write path.
///
/// Preflight order mirrors `run_set_status`: parse the id, read the
/// body, resolve the author, canonicalize cwd, probe for the jj repo +
/// `bugs` bookmark, then hand off to storage. We deliberately do the
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
    // 1. Parse the id. Bad shape → exit 2.
    let bug_id =
        BugId::parse(&id).map_err(|error| CliError::BadBugId { value: id, error })?;

    // 2. Read the body. `-F -` is stdin; `-F <path>` is the file.
    // Reuse the same helper `run_new` uses so the contract stays
    // consistent across verbs.
    let body = read_body(Some(file.as_path()))?;
    if body.is_empty() {
        return Err(CliError::EmptyCommentBody);
    }

    // 3. Resolve + canonicalize cwd.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;

    // 4. Preflight: jj repo + `bugs` bookmark present. We run this
    // BEFORE author resolution so a non-jj cwd surfaces the typed
    // "not a jj repo" error rather than the (correct but less useful)
    // "no comment author available" — the user almost always wants to
    // hear about the repo problem first.
    preflight::bugs_bookmark(&cwd)?;

    // 5. Resolve the author. CLI override wins; otherwise we synthesize
    // `Name <email>` from jj's user config. If neither path yields a
    // non-empty string we bail with a typed hint rather than letting
    // the storage layer surface a generic `Invalid` error.
    let author = resolve_author(author)?;

    // 6. Hand off to storage. `add_comment` returns the freshly-minted
    // comment id (a 7-hex `BugId`) for the JSON envelope.
    let storage = Storage::open(&cwd)?;
    let comment_id = storage.add_comment(&bug_id, &body, &author)?;

    // 7. Render.
    if json {
        let out = serde_json::json!({
            "ok": true,
            "id": bug_id.as_str(),
            "comment_id": comment_id.as_str(),
        });
        println!("{out}");
    } else {
        println!("comment added to {bug_id}");
    }
    Ok(())
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
/// bug on the `bugs` bookmark, filter by status and labels (AND across
/// labels), render newest-first.
///
/// Implementation strategy is the v1 "read all, filter in memory" path
/// the ticket calls out: `Storage::list_ids()` returns every id, then
/// we `Storage::read()` each one and apply the predicates. For repos
/// with a handful of bugs this is fine; once N gets meaningfully large
/// the storage layer will grow either a filtered enumeration primitive
/// or a per-bug metadata cache (separate ticket). The closing comment
/// on this issue calls out the perf feel.
fn run_ls(
    json: bool,
    status: StatusFilter,
    labels: Vec<String>,
) -> Result<(), CliError> {
    // Preflight: cwd is a jj repo AND `bugs` bookmark exists. Same
    // order as `run_show` — typed `run jjf init first` message rather
    // than raw jj stderr if the bookmark is missing.
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::bugs_bookmark(&cwd)?;

    let storage = Storage::open(&cwd)?;
    let ids = storage.list_ids()?;

    // Read every bug, filter. v1 is read-all; see the doc-comment.
    let mut bugs: Vec<Bug> = Vec::with_capacity(ids.len());
    for id in &ids {
        let bug = storage.read(id)?;
        if !status_matches(&bug, status) {
            continue;
        }
        if !labels_match(&bug, &labels) {
            continue;
        }
        bugs.push(bug);
    }

    // Newest-first by created_at. RFC 3339 second-resolution stamps
    // sort lexicographically — same trick the read path uses for
    // comments. `created_at` is set once at create and never bumped,
    // so the ordering is stable across mutation traffic.
    bugs.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    if json {
        // Array of `Bug` records, pretty-printed. Same per-element
        // shape `show --json` emits — callers parsing one parse the
        // other. Empty result is a valid empty array `[]`, not silence,
        // because a script expecting JSON wants something it can
        // `jq length` against. (Plain text uses silence-on-empty because
        // grep / awk pipelines want zero lines, not a JSON literal.)
        let s = serde_json::to_string_pretty(&bugs)
            .map_err(|e| CliError::Storage(StorageError::Json(e)))?;
        println!("{s}");
    } else {
        // Plain text: tab-separated, no header, silent on empty. The
        // 7-char id prefix is the documented human-display convention
        // (CLAUDE.md). label-count is rendered with a trailing `L` so
        // an eyeball can tell `3L` (three labels) apart from a numeric
        // column that might mean comments or something else later.
        for bug in &bugs {
            let status_s = match bug.status {
                Status::Open => "open",
                Status::Closed => "closed",
            };
            println!(
                "{id}\t{status}\t{n}L\t{title}",
                id = bug.id,
                status = status_s,
                n = bug.labels.len(),
                title = bug.title,
            );
        }
    }
    Ok(())
}

/// `--status` predicate. `All` matches everything.
fn status_matches(bug: &Bug, filter: StatusFilter) -> bool {
    match filter {
        StatusFilter::All => true,
        StatusFilter::Open => bug.status == Status::Open,
        StatusFilter::Closed => bug.status == Status::Closed,
    }
}

/// `--label` predicate. Empty filter matches every bug. A non-empty
/// filter requires the bug to carry EVERY listed label (intersection).
fn labels_match(bug: &Bug, wanted: &[String]) -> bool {
    wanted.iter().all(|w| bug.labels.iter().any(|l| l == w))
}
