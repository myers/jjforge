//! Thin wrapper over the `jj` CLI. Lifted from
//! `experiments/jj-shellout-hello/src/main.rs`. Decisions copied as-is:
//!
//! - Every command is built via `jj --repository <abs path> ...` so we
//!   never depend on cwd discovery.
//! - Non-zero exit becomes a typed error carrying stderr.
//! - We don't shell-quote; args are passed as separate `Command` args.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug, Clone)]
pub(crate) struct JjRepo {
    root: PathBuf,
}

impl JjRepo {
    pub(crate) fn open(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        debug_assert!(
            root.is_absolute(),
            "JjRepo::open requires an absolute path; caller checked"
        );
        Self { root }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new("jj");
        c.arg("--repository").arg(&self.root);
        c.args(args);
        c
    }

    pub(crate) fn run(&self, args: &[&str]) -> Result<String, JjError> {
        let out: Output = self.cmd(args).output().map_err(JjError::Io)?;
        if !out.status.success() {
            return Err(JjError::Cli {
                cmd: format!("jj {}", args.join(" ")),
                status: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JjError {
    #[error("spawning jj: {0}")]
    Io(#[source] std::io::Error),
    #[error("`{cmd}` exited {status:?}:\n{stderr}")]
    Cli {
        cmd: String,
        status: Option<i32>,
        stderr: String,
    },
}

impl JjError {
    /// Heuristic: does this jj failure look like a concurrent-write
    /// conflict on the working copy / bookmark? jj surfaces these as
    /// an "Internal error: Failed to check out commit … Caused by:
    /// Concurrent checkout" cascade when a sibling writer's
    /// `jj bookmark set` landed first and our `jj new
    /// bookmarks(issues)` snapshot is stale.
    ///
    /// Used by `Storage::commit_record_change` to translate the raw
    /// jj-internal vomit into a typed [`crate::Error::ConcurrentWrite`]
    /// (and the create-with-slug path upgrades that to
    /// [`crate::Error::SlugCollision`] on a post-failure probe).
    ///
    /// Pattern-matches on substrings rather than exact stderr lines:
    /// the message wraps differently across jj versions, but both
    /// phrases ("Concurrent checkout" and "Failed to check out
    /// commit") are stable identifiers of the same conflict.
    pub(crate) fn is_concurrent_write(&self) -> bool {
        match self {
            JjError::Cli { stderr, .. } => {
                stderr.contains("Concurrent checkout")
                    || stderr.contains("Failed to check out commit")
            }
            JjError::Io(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The literal stderr text the QA red-team 2026-06-23 test 16
    /// captured. Locks the detector to the actual production failure
    /// shape so a future stderr-format tweak in this file can't
    /// silently regress.
    #[test]
    fn qa_observed_concurrent_checkout_stderr_is_detected() {
        let stderr = "Internal error: Failed to check out commit \
                      c50c7d819aa507052461627b908757c5dd362b63\n\
                      Caused by: Concurrent checkout\n";
        let err = JjError::Cli {
            cmd: "jj new bookmarks(issues) -m ...".into(),
            status: Some(255),
            stderr: stderr.into(),
        };
        assert!(
            err.is_concurrent_write(),
            "detector missed the QA-observed concurrent-checkout stderr",
        );
    }

    /// Each phrase fires the detector independently. Different jj
    /// versions emit the messages differently; we want either
    /// substring to be sufficient.
    #[test]
    fn either_concurrent_checkout_phrase_is_detected() {
        let just_checkout = JjError::Cli {
            cmd: "jj new ...".into(),
            status: Some(255),
            stderr: "Concurrent checkout".into(),
        };
        assert!(just_checkout.is_concurrent_write());

        let just_failed_to_check_out = JjError::Cli {
            cmd: "jj new ...".into(),
            status: Some(255),
            stderr: "Failed to check out commit abc123".into(),
        };
        assert!(just_failed_to_check_out.is_concurrent_write());
    }

    /// Unrelated jj failures (network, parse error, etc.) must NOT
    /// be misdetected as concurrent-write — that would mistakenly
    /// retry mutations whose underlying error retry can't fix.
    #[test]
    fn unrelated_jj_errors_are_not_concurrent_write() {
        let parse_err = JjError::Cli {
            cmd: "jj new bogus_revset".into(),
            status: Some(2),
            stderr: "Error: Failed to parse revset".into(),
        };
        assert!(!parse_err.is_concurrent_write());

        let io_err = JjError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "jj binary not on PATH",
        ));
        assert!(!io_err.is_concurrent_write());
    }
}
