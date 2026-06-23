//! Pre-storage probes the binary runs before handing off to
//! `jjf-storage`. Today there's exactly one — [`issues_bookmark`] —
//! but it lives in its own module so every read/write verb calls the
//! same implementation rather than each open-coding the same two `jj`
//! shell-outs.
//!
//! # Why this is in the binary, not the storage crate
//!
//! `jjf-storage` deliberately does NOT check for the `issues`
//! bookmark in `Storage::open` — the storage layer treats
//! "bookmark exists" as a precondition the caller is responsible for.
//! The CLI wants a distinct, typed "run `jjf init` first" signal
//! (exit 2, message pointing at the fix) rather than the raw
//! jj-stderr that would bubble up from a first storage write against
//! an empty `bookmarks(issues)` revset, so it runs the probe itself.
//!
//! If a future ticket lifts this check into `Storage::open_strict`
//! (or extends the `StorageError` enum), this module can shrink to
//! a single-line wrapper or go away entirely. Until then: one probe,
//! one home.

use std::path::{Path, PathBuf};
use std::process::Command;

use jjf_storage::{Error as StorageError, ISSUES_BOOKMARK};

use crate::CliError;

/// Probe that `cwd` is inside a jj repo. Shells out to `jj workspace
/// root` and translates the one specific "not a jj repo" stderr into
/// `NotAJjRepo`; everything else becomes a generic `Probe` failure.
///
/// Callers that ALSO need the `issues` bookmark (every read/write
/// verb that touches an existing issue) should use
/// [`issues_bookmark`] instead — it composes this probe with the
/// bookmark check. Callers that meaningfully run before `jjf init`
/// (today: `jjf remote add|ls|rm`) call this one directly.
pub(crate) fn jj_repo(cwd: &Path) -> Result<(), CliError> {
    let out = Command::new("jj")
        .arg("--repository")
        .arg(cwd)
        .args(["workspace", "root"])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("no jj repo") {
            return Err(CliError::Storage(StorageError::NotAJjRepo(
                PathBuf::from(cwd),
            )));
        }
        return Err(CliError::Probe(std::io::Error::other(format!(
            "jj workspace root failed: {stderr}"
        ))));
    }
    Ok(())
}

/// Probe that (a) `cwd` is inside a jj repo and (b) the `issues`
/// bookmark exists on it. Both checks shell out to `jj` directly
/// (mirroring what `Storage::init` does internally) so we can surface
/// distinct preflight-error variants rather than the storage layer's
/// generic `Jj` runtime error.
///
/// Most read/write verbs (`jjf new`, `jjf show`, `jjf ls`, etc.) call
/// this. The `jjf remote *` verbs use the simpler [`jj_repo`] probe
/// because remote setup is meaningful before `jjf init`.
pub(crate) fn issues_bookmark(cwd: &Path) -> Result<(), CliError> {
    // Check 1: is this a jj repo at all? Same logic as `jj_repo` —
    // reuse it so the two probes stay in sync.
    jj_repo(cwd)?;

    // Check 2: does `issues` bookmark exist? `jj bookmark list`
    // exits 0 either way; we key off stdout content.
    let out = Command::new("jj")
        .arg("--repository")
        .arg(cwd)
        .args(["bookmark", "list", "-T", "name ++ \"\\n\"", ISSUES_BOOKMARK])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        return Err(CliError::Probe(std::io::Error::other(format!(
            "jj bookmark list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ))));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !stdout.lines().any(|l| l.trim() == ISSUES_BOOKMARK) {
        return Err(CliError::MissingIssuesBookmark(cwd.to_owned()));
    }
    Ok(())
}

/// Environment variable that bypasses the [`refuse_self_hosted_write`]
/// guard. Set to `1` (or any non-empty value) to explicitly authorize
/// a mutating verb to run from inside the jjforge source repo.
const SELF_HOST_BYPASS_ENV: &str = "JJF_ALLOW_SELF_HOST";

/// Marker files that, when both present at `cwd` or any ancestor, mark
/// the directory as "inside the jjforge source repo." Both must match
/// — using two markers guards against a false positive on an unrelated
/// repo that happens to contain a `crates/jjf` subtree (e.g. a fork in
/// progress) or against a project that happens to ship a
/// `docs/storage-format.md`. The combination is jjforge-specific.
const SELF_HOST_MARKERS: &[&str] = &["crates/jjf/Cargo.toml", "docs/storage-format.md"];

/// Refuse to run a mutating verb when the current working directory
/// looks like the jjforge source repo, unless the operator explicitly
/// opts in via `JJF_ALLOW_SELF_HOST=1`.
///
/// `json_output` selects the stderr shape on bypass: in text mode we
/// emit a loud `jjf: JJF_ALLOW_SELF_HOST=1 set; proceeding...` line so
/// an interactive operator notices the override; in `--json` mode the
/// bypass is silent so the verb's own JSON envelope (success or
/// error) is the only thing on stderr/stdout that a downstream
/// parser has to handle. Scripted callers know they set the env;
/// they don't need an extra preamble line to confirm it.
///
/// # Why this exists
///
/// jjforge is colocated jj+git: the storage layer's 4-CLI write dance
/// (`jj new bookmarks(issues)` → edit working copy → `jj describe` →
/// `jj bookmark set` → `jj new root()`) moves the jj working copy
/// onto the `issues` bookmark and back. In a colocated repo, this
/// drag also moves git HEAD — jj writes `refs/jj/root` into
/// `.git/HEAD` during the final `jj new root()` step. The result:
/// after running any mutating `jjf` verb from inside the source
/// repo, `git status` reports the whole tree as new-against-empty,
/// and `git commit` lands on a phantom root commit instead of on top
/// of `main`. The recovery is destructive
/// (`git symbolic-ref HEAD refs/heads/main && git reset --hard main`)
/// and requires operator intervention.
///
/// `--ignore-working-copy` on the underlying jj calls was investigated
/// (see issue `08cf14b`) and does NOT prevent the drift — jj still
/// rewrites `.git/HEAD` to `refs/jj/root` because the working copy
/// commit is unbranched. Full workspace isolation (option B in the
/// ticket) would fix this but is a multi-ticket engineering project.
///
/// Until then: refuse to write from inside the source repo. Operators
/// who genuinely need to write from inside (e.g. orchestration loops
/// authorizing themselves explicitly) opt in via `JJF_ALLOW_SELF_HOST=1`
/// and accept the drift, with a loud stderr line announcing the
/// bypass.
///
/// # Detection
///
/// We climb from `cwd` toward the filesystem root and look for ANY
/// ancestor whose tree contains both marker files in
/// [`SELF_HOST_MARKERS`]. Using both as a combo guards against a
/// false positive on unrelated repos.
///
/// # Wiring
///
/// Mutating verbs (`init`, `new`, `update`, `comment`, `close`,
/// `open`, `label`, `push`, `pull`) call this before the other
/// preflight probes. Read verbs (`show`, `ls`, `remote ls`) skip it
/// — they don't trigger the 4-CLI dance and don't drift.
pub(crate) fn refuse_self_hosted_write(
    cwd: &Path,
    json_output: bool,
) -> Result<(), CliError> {
    let bypass = std::env::var(SELF_HOST_BYPASS_ENV)
        .ok()
        .filter(|v| !v.is_empty())
        .is_some();

    let Some(root) = find_self_host_root(cwd) else {
        // Not inside the jjforge source repo — nothing to refuse.
        return Ok(());
    };

    if bypass {
        // Loud bypass for the text path: stderr announces the
        // override so a log scrape catches it. JSON-output callers
        // already set the env consciously and want a clean envelope
        // on stderr/stdout — the announcement would break their
        // single-line JSON-envelope parsers (see e.g. close.rs,
        // remote.rs).
        //
        // We deliberately do NOT emit this when the env is set but
        // the cwd isn't the source repo — that would be noise on
        // every invocation for an operator who's set the env
        // globally.
        if !json_output {
            eprintln!(
                "jjf: {SELF_HOST_BYPASS_ENV}=1 set; proceeding with write from inside jjforge source repo at {}",
                root.display()
            );
        }
        return Ok(());
    }

    Err(CliError::SelfHostedWriteRefused {
        path: root,
        markers: SELF_HOST_MARKERS.iter().map(|s| (*s).to_owned()).collect(),
    })
}

/// Walk upward from `cwd` looking for an ancestor that contains every
/// marker in [`SELF_HOST_MARKERS`]. Returns the matched root, or
/// `None` if no ancestor matches.
///
/// Walking upward (rather than checking just `cwd`) means a subagent
/// invoked from `crates/jjf/` or `experiments/<topic>/` still gets
/// caught — the markers live at the repo root, not wherever the
/// agent's cwd happens to be.
fn find_self_host_root(cwd: &Path) -> Option<PathBuf> {
    let mut current: Option<&Path> = Some(cwd);
    while let Some(dir) = current {
        if SELF_HOST_MARKERS.iter().all(|m| dir.join(m).is_file()) {
            return Some(dir.to_owned());
        }
        current = dir.parent();
    }
    None
}
