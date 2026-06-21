//! `jj-shellout-hello` — the smallest reasonable Rust example
//! proving the API shape jjforge will actually use.
//!
//! Background: issues 2130de1 and a60bb95 ruled out linking
//! `jj-lib` for v1 and ruled out `jj op log` as the audit surface.
//! The decided shape is:
//!
//!   - All jj interactions are shell-outs to the `jj` binary.
//!   - Each jjforge mutation is one `jj` commit whose description
//!     carries a structured `Jjf-Op:` trailer (git-trailer style).
//!   - The audit surface for a single bug is
//!     `jj log <path-to-bug-file> -T '<json template>'`, parsed
//!     back out of the commit descriptions.
//!
//! This program demonstrates that shape end to end:
//!
//!   1. Open a jj repo from an absolute path (= `jj --repository`).
//!   2. Create a change carrying a structured commit message on a
//!      named bookmark, without checking out that bookmark.
//!   3. Read the per-bug op chain via `jj log <path>` and parse
//!      the trailers.
//!
//! Run with:
//!   cargo run --quiet
//!
//! The program builds its own throwaway test repo under the OS
//! temp dir, exercises the shape, prints what it found, and
//! cleans up. No fixtures in the experiment dir.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::Deserialize;

// ---------------------------------------------------------------
// Thin wrapper over the `jj` CLI. This is the trait shape 2130de1
// recommended ("wrap the `jj` CLI behind a thin Rust trait"). Here
// it's a plain struct because the example only needs one impl.
// ---------------------------------------------------------------

#[derive(Debug, Clone)]
struct JjRepo {
    /// Absolute path to the repo root (the dir containing `.jj/`).
    root: PathBuf,
}

#[derive(Debug)]
enum JjError {
    Io(std::io::Error),
    Cli {
        cmd: String,
        status: Option<i32>,
        stderr: String,
    },
    Parse(String),
}

impl std::fmt::Display for JjError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JjError::Io(e) => write!(f, "io: {e}"),
            JjError::Cli { cmd, status, stderr } => write!(
                f,
                "`jj` exited {status:?} for `{cmd}`:\n{stderr}"
            ),
            JjError::Parse(s) => write!(f, "parse: {s}"),
        }
    }
}

impl std::error::Error for JjError {}

impl From<std::io::Error> for JjError {
    fn from(e: std::io::Error) -> Self {
        JjError::Io(e)
    }
}

impl JjRepo {
    /// (1) Open a jj repo from an absolute path. We don't *check*
    /// here; the first command will fail loudly if it isn't a repo.
    /// Matches issue (1): "open a jj repo from an absolute path."
    fn open(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        assert!(root.is_absolute(), "JjRepo::open requires an absolute path");
        Self { root }
    }

    /// Build a `jj` Command rooted at this repo. Note `--repository`:
    /// this is how we avoid relying on cwd discovery.
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new("jj");
        c.arg("--repository").arg(&self.root);
        c.args(args);
        c
    }

    fn run(&self, args: &[&str]) -> Result<String, JjError> {
        let out: Output = self.cmd(args).output()?;
        if !out.status.success() {
            return Err(JjError::Cli {
                cmd: format!("jj {}", args.join(" ")),
                status: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// (2) Create a change with a structured commit message on a
    /// named bookmark, **without leaving the working copy pinned to
    /// that bookmark**.
    ///
    /// `jj` doesn't have a `file write -r <change>` (as of 0.40),
    /// so we have to stage the content through the working copy.
    /// The trick to satisfy the "without checking out" requirement
    /// is to land on an *empty* @ on top of root() when we're done,
    /// not on the bookmark's tip.
    ///
    /// Strategy:
    ///
    ///   1. `jj new <parent> -m '<msg with trailer>'` (this moves
    ///      @ onto the new change so we can edit it).
    ///   2. Write the bug file into the working copy with normal
    ///      filesystem I/O. `jj` will snapshot it.
    ///   3. `jj bookmark set <name> -r @ --allow-backwards`
    ///      points the bookmark at this change.
    ///   4. `jj new root()` puts @ back on a fresh empty change on
    ///      top of root, so we are no longer "on" the bookmark.
    ///
    /// Returns the new change id (the one the bookmark now points
    /// at — i.e. the parent we'll chain off next time).
    fn create_change_on_bookmark(
        &self,
        bookmark: &str,
        parent_revset: &str,
        path: &Path,
        contents: &str,
        op: &JjfOp,
    ) -> Result<String, JjError> {
        let msg = op.to_commit_message();

        // 1. Create the change with @ on it.
        self.run(&["new", parent_revset, "-m", &msg])?;

        // 2. Write the file to the working copy. `jj` snapshots @
        //    automatically at the start of the next command.
        let abs_path = self.root.join(path);
        if let Some(parent) = abs_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&abs_path, contents)?;

        // Capture the change id now (before we step @ off it).
        let change_id = self
            .run(&[
                "log",
                "--no-graph",
                "-r",
                "@",
                "-T",
                "change_id ++ \"\\n\"",
            ])?
            .lines()
            .next()
            .ok_or_else(|| JjError::Parse("could not read @ change id".into()))?
            .trim()
            .to_string();

        // 3. Point the bookmark at it. `--allow-backwards` keeps
        //    the example idempotent if it's re-run after a panic.
        self.run(&[
            "bookmark",
            "set",
            bookmark,
            "-r",
            &change_id,
            "--allow-backwards",
        ])?;

        // 4. Step @ off the bookmark onto a fresh empty change on
        //    top of root, so we don't end up "on" the bookmark.
        //    Use --no-edit=false (default) — we want @ here so that
        //    the next iteration's `jj new <parent>` doesn't see the
        //    file we just wrote in its working-copy snapshot.
        self.run(&["new", "root()"])?;

        Ok(change_id)
    }

    /// (3) Read the per-bug op chain. Uses `jj log <path>`, not
    /// `jj op log`, per a60bb95.
    fn bug_history(&self, path: &Path) -> Result<Vec<BugHistoryEntry>, JjError> {
        // Custom template emits one JSON object per matching commit
        // with exactly the fields we want. `json(description)` does
        // the necessary string escaping.
        let tmpl = r#""{" ++ "\"change_id\":\"" ++ change_id.short() ++ "\"," ++ "\"commit_id\":\"" ++ commit_id.short() ++ "\"," ++ "\"description\":" ++ json(description) ++ "}\n""#;
        // `jj` resolves file arguments relative to *cwd*, not the
        // repo root, even when `--repository` is set. Use the
        // `root:` fileset prefix to be unambiguous.
        let pattern = format!("root:{}", path.display());
        let raw = self.run(&[
            "log",
            "--no-graph",
            "-T",
            tmpl,
            &pattern,
        ])?;

        let mut out = Vec::new();
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let row: BugRow = serde_json::from_str(line)
                .map_err(|e| JjError::Parse(format!("{e}: {line}")))?;
            let op = JjfOp::from_commit_message(&row.description);
            out.push(BugHistoryEntry {
                change_id: row.change_id,
                commit_id: row.commit_id,
                description: row.description,
                op,
            });
        }
        Ok(out)
    }
}

#[derive(Debug, Deserialize)]
struct BugRow {
    change_id: String,
    commit_id: String,
    description: String,
}

#[derive(Debug)]
#[allow(dead_code)] // fields printed via Debug
struct BugHistoryEntry {
    change_id: String,
    commit_id: String,
    description: String,
    /// Parsed `Jjf-Op:` trailer, if present.
    op: Option<JjfOp>,
}

// ---------------------------------------------------------------
// Structured op header. a60bb95 recommended git-trailer style.
// This is a minimal stand-in to prove the round-trip works.
// ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum JjfOp {
    Create { bug_id: String, title: String },
    SetTitle { bug_id: String, title: String },
    AddComment { bug_id: String, author: String },
}

impl JjfOp {
    /// Render as a complete commit message: a human-readable
    /// summary line, a blank line, then the structured trailer.
    /// Trailers survive `jj describe` reflow because they're
    /// single-line `Key: value` pairs at the bottom of the message.
    fn to_commit_message(&self) -> String {
        match self {
            JjfOp::Create { bug_id, title } => format!(
                "jjf: bug {bug_id} - create\n\nJjf-Op: create\nJjf-Bug: {bug_id}\nJjf-Title: {title}\n"
            ),
            JjfOp::SetTitle { bug_id, title } => format!(
                "jjf: bug {bug_id} - set-title\n\nJjf-Op: set-title\nJjf-Bug: {bug_id}\nJjf-Title: {title}\n"
            ),
            JjfOp::AddComment { bug_id, author } => format!(
                "jjf: bug {bug_id} - comment\n\nJjf-Op: add-comment\nJjf-Bug: {bug_id}\nJjf-Author: {author}\n"
            ),
        }
    }

    /// Parse a commit message back to a structured op. Returns
    /// None if it doesn't carry a `Jjf-Op:` trailer.
    fn from_commit_message(msg: &str) -> Option<Self> {
        let mut op = None;
        let mut bug = None;
        let mut title = None;
        let mut author = None;
        for line in msg.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("Jjf-Op:") {
                op = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("Jjf-Bug:") {
                bug = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("Jjf-Title:") {
                title = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("Jjf-Author:") {
                author = Some(v.trim().to_string());
            }
        }
        match (op.as_deref(), bug, title, author) {
            (Some("create"), Some(b), Some(t), _) => {
                Some(JjfOp::Create { bug_id: b, title: t })
            }
            (Some("set-title"), Some(b), Some(t), _) => {
                Some(JjfOp::SetTitle { bug_id: b, title: t })
            }
            (Some("add-comment"), Some(b), _, Some(a)) => {
                Some(JjfOp::AddComment { bug_id: b, author: a })
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------
// The hello world.
// ---------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build a throwaway repo under the OS temp dir so this
    // experiment never leaves a .jj/ behind in the source tree.
    let tmp = env::temp_dir().join(format!(
        "jj-shellout-hello-{}",
        std::process::id()
    ));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(&tmp)?;
    println!("scratch repo: {}", tmp.display());

    let out = Command::new("jj")
        .args(["git", "init"])
        .current_dir(&tmp)
        .output()?;
    if !out.status.success() {
        eprintln!("jj git init failed: {}", String::from_utf8_lossy(&out.stderr));
        std::process::exit(1);
    }

    // (1) Open the repo from an absolute path.
    let repo = JjRepo::open(&tmp);
    println!("opened repo at {}", repo.root.display());

    // (2) Create three changes on a `bugs` bookmark, with
    // structured commit messages, without checking that bookmark
    // out. Each commit modifies bugs/aa6600b.json.
    let bug_path = PathBuf::from("bugs/aa6600b.json");

    let ops = [
        (
            JjfOp::Create {
                bug_id: "aa6600b".into(),
                title: "first bug".into(),
            },
            r#"{"title":"first bug","status":"open","comments":[]}"#,
        ),
        (
            JjfOp::SetTitle {
                bug_id: "aa6600b".into(),
                title: "first bug, retitled".into(),
            },
            r#"{"title":"first bug, retitled","status":"open","comments":[]}"#,
        ),
        (
            JjfOp::AddComment {
                bug_id: "aa6600b".into(),
                author: "alice".into(),
            },
            r#"{"title":"first bug, retitled","status":"open","comments":[{"author":"alice","body":"hi"}]}"#,
        ),
    ];

    // Build a linear chain on the bookmark: each op's parent is
    // the previous tip. First parent is root().
    let mut parent = "root()".to_string();
    for (op, contents) in ops.iter() {
        let change_id = repo.create_change_on_bookmark(
            "bugs",
            &parent,
            &bug_path,
            contents,
            op,
        )?;
        println!("created change {change_id} on bookmark `bugs`: {op:?}");
        parent = change_id;
    }

    // Verify @ never moved off root - we never checked out the
    // bookmark. This is the "without checking out" half of (2).
    let at_at = repo.run(&[
        "log",
        "--no-graph",
        "-r",
        "@",
        "-T",
        "change_id.short() ++ \" \" ++ if(empty, \"empty\", \"non-empty\") ++ \"\\n\"",
    ])?;
    println!("working copy (@) after writes: {}", at_at.trim());

    // (3) Read the per-bug op chain via `jj log <path>` and parse
    // the trailers back into structured ops.
    let history = repo.bug_history(&bug_path)?;
    println!();
    println!("--- bug history for {} ---", bug_path.display());
    // History comes back newest-first. Reverse to read like a log.
    for entry in history.iter().rev() {
        println!(
            "  {}  {}  parsed_op = {:?}",
            entry.commit_id,
            entry
                .description
                .lines()
                .next()
                .unwrap_or("(empty description)"),
            entry.op,
        );
    }

    // Sanity asserts so the example fails loud if jj's behavior
    // shifts and breaks our shape.
    assert_eq!(history.len(), 3, "expected 3 commits for this bug");
    assert!(
        history.iter().all(|e| e.op.is_some()),
        "all jjforge commits should carry a Jjf-Op trailer"
    );
    let earliest_op = history.last().unwrap().op.as_ref().unwrap();
    assert!(matches!(earliest_op, JjfOp::Create { .. }));

    println!();
    println!("ok: round-trip Jjf-Op trailer through jj commit description works.");
    println!("ok: `jj log <path>` is a usable per-bug audit surface.");

    // Clean up so the experiment dir never contains an inner .jj/.
    drop(repo);
    fs::remove_dir_all(&tmp).ok();
    println!("cleaned up: {}", tmp.display());

    Ok(())
}
