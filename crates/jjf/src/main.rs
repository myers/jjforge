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

    /// Mutate a bug's scalar fields. Not yet implemented (ticket:
    /// `cli-update`).
    Update,

    /// Append a comment to a bug. Not yet implemented (ticket:
    /// `cli-comment`).
    Comment,

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

    /// Add or remove labels. Not yet implemented (ticket:
    /// `cli-label`).
    Label,
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
        // Stubs. We deliberately return a generic runtime error
        // (exit 1) rather than a clap-level error (exit 2): the
        // command parsed fine, we just haven't implemented its
        // body. When the per-verb ticket lands, this arm goes away.
        Commands::Update | Commands::Comment | Commands::Label => Err(CliError::Storage(
            StorageError::Invalid("not yet implemented".into()),
        )),
    }
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
