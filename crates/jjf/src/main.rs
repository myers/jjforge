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

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use jjf_storage::{BUGS_BOOKMARK, Error as StorageError, Storage};

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

/// Every verb the epic body (`c4f7fcb`) calls out, plus `init`. Stubs
/// exist so `--help` lists the full surface from day one; later
/// per-verb tickets replace the stubs with real implementations.
#[derive(Debug, Subcommand)]
enum Commands {
    /// Initialize the `bugs` bookmark on the current jj repo.
    /// Idempotent — running twice in the same repo is a no-op.
    Init,

    /// Create a new bug. Not yet implemented (ticket: `cli-new`).
    New,

    /// Print a single bug. Not yet implemented (ticket: `cli-show`).
    Show,

    /// List bugs, with optional filters. Not yet implemented
    /// (ticket: `cli-ls`).
    Ls,

    /// Mutate a bug's scalar fields. Not yet implemented (ticket:
    /// `cli-update`).
    Update,

    /// Append a comment to a bug. Not yet implemented (ticket:
    /// `cli-comment`).
    Comment,

    /// Close a bug. Not yet implemented (ticket: `cli-status`).
    Close,

    /// Reopen a bug. Not yet implemented (ticket: `cli-status`).
    Open,

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
        // Stubs. We deliberately return a generic runtime error
        // (exit 1) rather than a clap-level error (exit 2): the
        // command parsed fine, we just haven't implemented its
        // body. When the per-verb ticket lands, this arm goes away.
        Commands::New
        | Commands::Show
        | Commands::Ls
        | Commands::Update
        | Commands::Comment
        | Commands::Close
        | Commands::Open
        | Commands::Label => Err(CliError::Storage(StorageError::Invalid(
            "not yet implemented".into(),
        ))),
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
