//! Pre-storage probes the binary runs before handing off to
//! `jjf-storage`. Today there's exactly one — [`bugs_bookmark`] — but
//! it lives in its own module so every read/write verb calls the same
//! implementation rather than each open-coding the same two `jj`
//! shell-outs.
//!
//! # Why this is in the binary, not the storage crate
//!
//! `jjf-storage` deliberately does NOT check for the `bugs` bookmark
//! in `Storage::open` — the storage layer treats "bookmark exists" as
//! a precondition the caller is responsible for. The CLI wants a
//! distinct, typed "run `jjf init` first" signal (exit 2, message
//! pointing at the fix) rather than the raw jj-stderr that would
//! bubble up from a first storage write against an empty
//! `bookmarks(bugs)` revset, so it runs the probe itself.
//!
//! If a future ticket lifts this check into `Storage::open_strict`
//! (or extends the `StorageError` enum), this module can shrink to
//! a single-line wrapper or go away entirely. Until then: one probe,
//! one home.

use std::path::{Path, PathBuf};
use std::process::Command;

use jjf_storage::{BUGS_BOOKMARK, Error as StorageError};

use crate::CliError;

/// Probe that (a) `cwd` is inside a jj repo and (b) the `bugs`
/// bookmark exists on it. Both checks shell out to `jj` directly
/// (mirroring what `Storage::init` does internally) so we can surface
/// distinct preflight-error variants rather than the storage layer's
/// generic `Jj` runtime error.
///
/// Both `jjf new` and `jjf show` call this. Adding a new verb? Call
/// it here too — the moment a third caller appears, this module is
/// still the right home; don't re-open-code the probe.
pub(crate) fn bugs_bookmark(cwd: &Path) -> Result<(), CliError> {
    // Check 1: is this a jj repo at all? Mirrors the
    // `Storage::init` probe — translate the one specific stderr
    // we recognize into `NotAJjRepo`, everything else into Probe.
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
        // Some other jj failure — surface its stderr verbatim so the
        // operator can see what jj said.
        return Err(CliError::Probe(std::io::Error::other(format!(
            "jj workspace root failed: {stderr}"
        ))));
    }

    // Check 2: does `bugs` bookmark exist? `jj bookmark list`
    // exits 0 either way; we key off stdout content.
    let out = Command::new("jj")
        .arg("--repository")
        .arg(cwd)
        .args(["bookmark", "list", "-T", "name ++ \"\\n\"", BUGS_BOOKMARK])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        return Err(CliError::Probe(std::io::Error::other(format!(
            "jj bookmark list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ))));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !stdout.lines().any(|l| l.trim() == BUGS_BOOKMARK) {
        return Err(CliError::MissingBugsBookmark(cwd.to_owned()));
    }
    Ok(())
}
