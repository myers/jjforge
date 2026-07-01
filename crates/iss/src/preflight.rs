//! Pre-storage probes the binary runs before handing off to
//! `jjf-storage`. Today there's exactly one — [`issues_bookmark`] —
//! but it lives in its own module so every read/write verb calls the
//! same implementation rather than each open-coding the same probes.
//!
//! # Why this is in the binary, not the storage crate
//!
//! `jjf-storage` deliberately does NOT check for the `issues`
//! bookmark in `Storage::open` — the storage layer treats
//! "bookmark exists" as a precondition the caller is responsible for.
//! The CLI wants a distinct, typed "run `iss init` first" signal
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

use iss_storage::Error as StorageError;

use crate::CliError;

/// Probe that `cwd` is inside a git repo. Shells out to `git rev-parse
/// --git-dir` and translates a non-zero exit into `NotAJjRepo`;
/// everything else becomes a generic `Probe` failure.
///
/// Callers that ALSO need the `issues` bookmark (every read/write
/// verb that touches an existing issue) should use
/// [`issues_bookmark`] instead — it composes this probe with the
/// bookmark check. Callers that meaningfully run before `iss init`
/// (today: `iss remote add|ls|rm`) call this one directly.
pub(crate) fn jj_repo(cwd: &Path) -> Result<(), CliError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--git-dir"])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        return Err(CliError::Storage(StorageError::NotAJjRepo(
            PathBuf::from(cwd),
        )));
    }
    Ok(())
}

/// Probe that (a) `cwd` is inside a git repo and (b) the repo is
/// `iss init`-ed — meaning the v3 `refs/jjf/meta/format-version`
/// sentinel ref resolves.
///
/// The v3 sentinel is the only supported init marker. v2 (`issues`
/// bookmark) and v1 (`bugs` bookmark) shapes are no longer accepted;
/// repos using those shapes must migrate via `iss init`.
///
/// Most read/write verbs (`iss new`, `iss show`, `iss ls`, etc.) call
/// this. The `iss remote *` verbs use the simpler [`jj_repo`] probe
/// because remote setup is meaningful before `iss init`.
pub(crate) fn issues_bookmark(cwd: &Path) -> Result<(), CliError> {
    // Check 1: is this a git repo at all?
    jj_repo(cwd)?;

    // Check 2: v3 sentinel ref. Its presence means the repo is
    // `iss init`-ed. v2/v1 bookmark shapes are no longer supported.
    if git_ref_exists(cwd, "refs/jjf/meta/format-version")? {
        return Ok(());
    }
    Err(CliError::MissingIssuesBookmark(cwd.to_owned()))
}

/// Cheap check: does a git ref resolve in `cwd`? Used by
/// [`issues_bookmark`] to detect v3-shape repos via the sentinel ref.
fn git_ref_exists(cwd: &Path, ref_name: &str) -> Result<bool, CliError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--verify", "--quiet", ref_name])
        .output()
        .map_err(CliError::Probe)?;
    // `--quiet` makes a missing ref exit 1 with empty stdout.
    Ok(out.status.success())
}

