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
