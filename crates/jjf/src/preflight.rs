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

use jjf_storage::{Error as StorageError, ISSUES_BOOKMARK, Storage};

use crate::CliError;

/// The v1 bookmark name. If we see this AND no `issues` bookmark,
/// the repo is pre-migration; calling `Storage::open` runs the
/// inline v1→v2 rename. Kept duplicated here because the storage
/// crate doesn't expose its `V1_BUGS_BOOKMARK` constant publicly.
const V1_BUGS_BOOKMARK: &str = "bugs";

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

/// Probe that (a) `cwd` is inside a jj repo and (b) the repo is
/// `jjf init`-ed — meaning either the v2 `issues` bookmark exists or
/// the v3 `refs/jjf/meta/format-version` sentinel ref resolves.
///
/// Both checks shell out directly so we can surface distinct
/// preflight-error variants rather than the storage layer's generic
/// `Jj` runtime error.
///
/// Most read/write verbs (`jjf new`, `jjf show`, `jjf ls`, etc.) call
/// this. The `jjf remote *` verbs use the simpler [`jj_repo`] probe
/// because remote setup is meaningful before `jjf init`.
pub(crate) fn issues_bookmark(cwd: &Path) -> Result<(), CliError> {
    // Check 1: is this a jj repo at all? Same logic as `jj_repo` —
    // reuse it so the two probes stay in sync.
    jj_repo(cwd)?;

    // Check 2a: v3 sentinel? Cheapest probe — one `git rev-parse`.
    // If present, the repo is v3-init'd and we're done.
    if git_ref_exists(cwd, "refs/jjf/meta/format-version")? {
        return Ok(());
    }

    // Check 2b: does the v2 `issues` bookmark exist? `jj bookmark
    // list` exits 0 either way; we key off stdout content.
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
        // Neither v3 sentinel nor v2 bookmark. Before refusing, check
        // if the v1 `bugs` bookmark is present — if so, this is a v1
        // repo that hasn't been migrated yet; calling `Storage::open`
        // triggers the inline v1→v2 rename in the storage layer.
        // After the migration commits, `issues` exists and the verb
        // proceeds normally on the next call.
        let v1_out = Command::new("jj")
            .arg("--repository")
            .arg(cwd)
            .args(["bookmark", "list", "-T", "name ++ \"\\n\"", V1_BUGS_BOOKMARK])
            .output()
            .map_err(CliError::Probe)?;
        let v1_stdout = String::from_utf8_lossy(&v1_out.stdout);
        if v1_out.status.success()
            && v1_stdout.lines().any(|l| l.trim() == V1_BUGS_BOOKMARK)
        {
            let _ =
                Storage::open(PathBuf::from(cwd)).map_err(CliError::Storage)?;
            return Ok(());
        }
        return Err(CliError::MissingIssuesBookmark(cwd.to_owned()));
    }
    Ok(())
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

