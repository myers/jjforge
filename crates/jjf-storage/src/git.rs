//! Thin wrapper over the `git` CLI for the v3 storage write path.
//!
//! The v3 design (`docs/storage-out-of-tree.md` "Write path") replaces
//! the 4-CLI `jj` working-copy dance with five direct `git` calls
//! (`cat-file`, `hash-object`, `mktree`, `commit-tree`, `update-ref`).
//! This module is the lowest layer: spawn `git`, capture stdout/stderr,
//! return typed results. No knowledge of issues, refs, or jjforge
//! semantics — those live in [`crate::v3_write`].
//!
//! ## Why a separate wrapper from [`crate::jj::JjRepo`]?
//!
//! - We want a different subprocess (`git` vs `jj`). The two binaries
//!   have different exit-status conventions and different stderr
//!   shapes; one wrapper-per-CLI keeps the error-translation logic
//!   sharp.
//! - The v3 write path NEVER calls `jj`. Keeping the v3 helpers in a
//!   dedicated module makes that contract grep-checkable (per ticket
//!   `eb42f50`: "No `jj` subprocess invoked on a v3 write path.
//!   Verify by grepping the write path code; no `Command::new(\"jj\")`.").
//! - Future v3 read-path work (ticket `6e2c843`) ports over the same
//!   set of helpers; centralizing them now means that ticket is a
//!   pure caller-side refactor.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// All-zeros git OID. `git update-ref <ref> <new> <old>` accepts this
/// as the "ref does not yet exist" CAS sentinel — i.e. create-iff-absent
/// semantics on the v3 write path's `create_issue` case.
pub(crate) const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// Handle to a colocated git repo. Built once per [`crate::Storage`]
/// alongside [`crate::jj::JjRepo`]; the two coexist and share the same
/// repo root on disk.
#[derive(Debug, Clone)]
pub(crate) struct GitRepo {
    /// Absolute path to the git work-tree root. We pass `--git-dir`
    /// rather than relying on cwd discovery so the helpers work
    /// regardless of where the parent process was invoked.
    root: PathBuf,
}

impl GitRepo {
    pub(crate) fn open(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        debug_assert!(
            root.is_absolute(),
            "GitRepo::open requires an absolute path; caller checked"
        );
        Self { root }
    }

    /// Kept around as a future-facing accessor — the v3 read path
    /// (ticket `6e2c843`) will want to project the repo root for
    /// snapshot-cache placement, mirroring [`crate::jj::JjRepo::root`].
    /// Suppressed via `dead_code` until that ticket lands.
    #[allow(dead_code)]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Build a `git` Command rooted at `self.root`. We use `-C <root>`
    /// (the same shape `jj --repository` takes) so the working
    /// directory of the parent process is irrelevant; `git` resolves
    /// the repo from `-C`.
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(&self.root);
        c.args(args);
        c
    }

    /// Run `git <args>`; capture stdout. Non-zero exit becomes a typed
    /// [`GitError::Cli`] carrying stderr verbatim. The caller decides
    /// what to do with it; see [`GitError::is_concurrent_write`] for
    /// the v3 CAS-failure detector.
    pub(crate) fn run(&self, args: &[&str]) -> Result<String, GitError> {
        let out: Output = self.cmd(args).output().map_err(GitError::Io)?;
        if !out.status.success() {
            return Err(GitError::Cli {
                cmd: format!("git {}", args.join(" ")),
                status: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Hash `bytes` into a blob and write the object into the
    /// repo's database. Returns the blob's hex object id (40 chars).
    ///
    /// Shells out to `git hash-object -w --stdin` and feeds the bytes
    /// via stdin so we never have to materialize them as a tempfile.
    pub(crate) fn hash_object(&self, bytes: &[u8]) -> Result<String, GitError> {
        let mut child = self
            .cmd(&["hash-object", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(GitError::Io)?;
        // The pipe is the only way the child sees the bytes; on a
        // short write or broken pipe we surface the IO error.
        {
            let stdin = child
                .stdin
                .as_mut()
                .expect("hash-object child stdin piped");
            stdin.write_all(bytes).map_err(GitError::Io)?;
        }
        let out = child.wait_with_output().map_err(GitError::Io)?;
        if !out.status.success() {
            return Err(GitError::Cli {
                cmd: "git hash-object -w --stdin".into(),
                status: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        let oid = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        Ok(oid)
    }

    /// Hash `bytes` without writing the object into the database.
    /// Returns the same hex oid `hash_object` would, minus the
    /// write-side effect. Used by the v3 snapshot cache's
    /// `probe_ref_set_key_v3` to fingerprint the ref-set without
    /// polluting the object DB with cache-key blobs.
    pub(crate) fn hash_object_no_write(&self, bytes: &[u8]) -> Result<String, GitError> {
        let mut child = self
            .cmd(&["hash-object", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(GitError::Io)?;
        {
            let stdin = child
                .stdin
                .as_mut()
                .expect("hash-object child stdin piped");
            stdin.write_all(bytes).map_err(GitError::Io)?;
        }
        let out = child.wait_with_output().map_err(GitError::Io)?;
        if !out.status.success() {
            return Err(GitError::Cli {
                cmd: "git hash-object --stdin".into(),
                status: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// Assemble a tree from `entries` and return its hex object id.
    ///
    /// Each entry is `(mode, name, oid)`. `mktree` reads stdin in the
    /// format `<mode> <type> <oid>\t<name>\n`; we render that and pipe
    /// it in. For blobs (the only entry kind the v3 write path uses)
    /// mode is `100644` and type is `blob`.
    ///
    /// Empty `entries` is allowed — it produces the empty-tree oid.
    /// We don't have a use for empty trees on the v3 write path, but
    /// the wrapper stays general.
    pub(crate) fn mktree(
        &self,
        entries: &[(&str, &str, &str)],
    ) -> Result<String, GitError> {
        let mut stdin_input = String::new();
        for (mode, name, oid) in entries {
            stdin_input.push_str(mode);
            stdin_input.push(' ');
            stdin_input.push_str("blob");
            stdin_input.push(' ');
            stdin_input.push_str(oid);
            stdin_input.push('\t');
            stdin_input.push_str(name);
            stdin_input.push('\n');
        }
        let mut child = self
            .cmd(&["mktree"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(GitError::Io)?;
        {
            let stdin = child.stdin.as_mut().expect("mktree child stdin piped");
            stdin
                .write_all(stdin_input.as_bytes())
                .map_err(GitError::Io)?;
        }
        let out = child.wait_with_output().map_err(GitError::Io)?;
        if !out.status.success() {
            return Err(GitError::Cli {
                cmd: "git mktree".into(),
                status: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        let oid = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        Ok(oid)
    }

    /// Create a commit object with the given tree, parents, and full
    /// message (summary + trailer block, exactly the bytes returned by
    /// [`crate::build_commit_message`]). Returns the commit's hex
    /// object id.
    ///
    /// We pipe the message via stdin so multi-line trailer blocks
    /// round-trip verbatim — passing them as `-m` argv would fight
    /// shell quoting in tests / callers.
    ///
    /// Empty `parents` is the create-from-nothing case (the v3 issue's
    /// first commit). `git commit-tree` produces a parentless root
    /// commit when no `-p` is passed.
    pub(crate) fn commit_tree(
        &self,
        tree_oid: &str,
        parents: &[&str],
        message: &str,
    ) -> Result<String, GitError> {
        let mut argv: Vec<String> = vec!["commit-tree".to_owned(), tree_oid.to_owned()];
        for p in parents {
            argv.push("-p".to_owned());
            argv.push((*p).to_owned());
        }
        // `-F -` reads the commit message from stdin.
        argv.push("-F".to_owned());
        argv.push("-".to_owned());
        let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        let mut child = self
            .cmd(&argv_refs)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(GitError::Io)?;
        {
            let stdin = child
                .stdin
                .as_mut()
                .expect("commit-tree child stdin piped");
            stdin
                .write_all(message.as_bytes())
                .map_err(GitError::Io)?;
        }
        let out = child.wait_with_output().map_err(GitError::Io)?;
        if !out.status.success() {
            return Err(GitError::Cli {
                cmd: format!("git {}", argv.join(" ")),
                status: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        let oid = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        Ok(oid)
    }

    /// Compare-and-swap update a ref. `expected_old_oid` is the CAS
    /// sentinel: a literal hex oid the ref must currently point at, or
    /// [`ZERO_OID`] to require the ref to NOT yet exist (i.e. the
    /// "create" case for a brand-new issue's first commit).
    ///
    /// On a CAS mismatch git exits non-zero with a short
    /// "cannot lock" / "expected ..." stderr; we map that to
    /// [`GitError::Cli`] and the caller's
    /// [`GitError::is_concurrent_write`] check translates it to the
    /// typed [`crate::Error::ConcurrentWrite`].
    pub(crate) fn update_ref(
        &self,
        ref_name: &str,
        new_oid: &str,
        expected_old_oid: &str,
    ) -> Result<(), GitError> {
        // `git update-ref <ref> <new> <old>` — atomic by git's own
        // ref-lock contract. We don't pass `-d` (delete) here; the v3
        // write path only ever creates or fast-forwards.
        self.run(&["update-ref", ref_name, new_oid, expected_old_oid])?;
        Ok(())
    }

    /// Resolve `ref_name` to its current commit oid. Returns `None` if
    /// the ref does not exist (the v3 create-issue case). We
    /// distinguish "ref absent" from "git broke" by checking stderr
    /// for git's stable "unknown revision" / "fatal: ambiguous
    /// argument" / "Needed a single revision" messages, but in
    /// practice `git rev-parse --verify --quiet <ref>` is the cleanest
    /// detector: exit 1 with empty stdout on missing, exit 0 with the
    /// oid on present.
    pub(crate) fn resolve_ref(
        &self,
        ref_name: &str,
    ) -> Result<Option<String>, GitError> {
        // We use rev-parse rather than show-ref because rev-parse with
        // `--verify --quiet` has the cleanest "absent" signal:
        // non-zero exit, empty stdout, empty stderr.
        let out = self
            .cmd(&["rev-parse", "--verify", "--quiet", ref_name])
            .output()
            .map_err(GitError::Io)?;
        if !out.status.success() {
            // Quiet failure on a missing ref is the design — return
            // None. Any non-empty stderr means git failed for a
            // different reason; surface it.
            if !out.stderr.is_empty() {
                return Err(GitError::Cli {
                    cmd: format!("git rev-parse --verify --quiet {}", ref_name),
                    status: out.status.code(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                });
            }
            return Ok(None);
        }
        let oid = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if oid.is_empty() {
            return Ok(None);
        }
        Ok(Some(oid))
    }

    /// Read the bytes of `path` from `ref_name`'s tree. Returns
    /// `Ok(None)` if either the ref doesn't exist or the path is
    /// absent within the ref's tree (a brand-new issue case, or a
    /// missing-comments-file case).
    ///
    /// We invoke `git cat-file blob <ref>:<path>`. git returns
    /// non-zero in both absent cases (`unknown revision` for missing
    /// ref, `path is not in HEAD` for missing path); we map both to
    /// `Ok(None)`. Real failures (corrupt object, IO error) bubble up.
    pub(crate) fn cat_blob(
        &self,
        ref_name: &str,
        path: &str,
    ) -> Result<Option<Vec<u8>>, GitError> {
        // We deliberately don't use `--filters` — we want raw bytes
        // exactly as the writer hashed them, not the smudged
        // working-tree shape.
        let spec = format!("{}:{}", ref_name, path);
        let out = self
            .cmd(&["cat-file", "blob", &spec])
            .output()
            .map_err(GitError::Io)?;
        if !out.status.success() {
            // Distinguish absent-path / absent-ref from real failures.
            // git's stable phrases for the absent-cases (across
            // recent versions, lowercase or capitalized variants):
            // - "fatal: Not a valid object name <spec>"
            // - "fatal: invalid object name '<ref>'"   (ref absent)
            // - "fatal: path '<path>' does not exist in '<ref>'"
            // - "fatal: bad revision '<spec>'"
            // - "fatal: Path '<path>' exists on disk, but not in '<ref>'"
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stderr_lc = stderr.to_ascii_lowercase();
            let absent = stderr_lc.contains("not a valid object name")
                || stderr_lc.contains("invalid object name")
                || stderr_lc.contains("does not exist")
                || stderr_lc.contains("bad revision")
                || stderr_lc.contains("exists on disk, but not in");
            if absent {
                return Ok(None);
            }
            return Err(GitError::Cli {
                cmd: format!("git cat-file blob {}", spec),
                status: out.status.code(),
                stderr: stderr.into_owned(),
            });
        }
        Ok(Some(out.stdout))
    }

    /// List every ref under `prefix` (e.g. `refs/jjf/issues/`).
    /// Returns the full refnames, sorted lexicographically. Empty
    /// result is normal — a v3 repo with no issues yet has no refs
    /// under `refs/jjf/issues/`.
    pub(crate) fn for_each_ref(
        &self,
        prefix: &str,
    ) -> Result<Vec<String>, GitError> {
        let out = self.run(&[
            "for-each-ref",
            "--format=%(refname)",
            prefix,
        ])?;
        let mut refs: Vec<String> = out
            .lines()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        refs.sort();
        Ok(refs)
    }

    /// List every ref under any of `prefixes`, returning `(refname,
    /// objectname)` pairs sorted ascending by refname. Empty result
    /// is normal — a fresh v3 repo with no issues and no memories has
    /// no refs under either prefix.
    ///
    /// Used by the v3 snapshot cache (`cache.rs::probe_ref_set_key`)
    /// to fingerprint the full ref-set for invalidation. The fields
    /// are space-separated by `--format='%(refname) %(objectname)'`;
    /// the parser splits on the first ASCII space and tolerates
    /// trailing whitespace.
    pub(crate) fn for_each_ref_with_oid(
        &self,
        prefixes: &[&str],
    ) -> Result<Vec<(String, String)>, GitError> {
        let mut args: Vec<&str> = vec![
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname) %(objectname)",
        ];
        args.extend_from_slice(prefixes);
        let out = self.run(&args)?;
        let mut pairs: Vec<(String, String)> = Vec::new();
        for line in out.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (name, oid) = match line.split_once(' ') {
                Some(parts) => parts,
                None => continue,
            };
            pairs.push((name.to_owned(), oid.to_owned()));
        }
        // `--sort=refname` already orders by refname ascending; keep
        // the result stable even if a future git emits unsorted output
        // by re-sorting here.
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(pairs)
    }

    /// Walk the commit chain ending at `ref_name`, oldest-first. Each
    /// returned record carries `(commit_oid, author "Name <email>",
    /// author_timestamp as `YYYY-MM-DDTHH:MM:SSZ`, full commit message)`.
    ///
    /// Used by the v3 read path (ticket `6e2c843`) to reconstruct the
    /// per-issue op log from the per-issue ref's commit chain — the v3
    /// counterpart to history's `jj log -r ancestors(...)` template
    /// dance, but cheaper (one process spawn instead of jj's heavier
    /// invocation).
    ///
    /// Returns an empty vec if `ref_name` resolves to no commit (the ref
    /// doesn't exist). git's `log` exits non-zero on an unknown ref; we
    /// translate that absence to `Ok(vec![])` so the caller can fall
    /// through to "no history" without string-matching stderr.
    ///
    /// The wire format uses two sentinels: one between fields within a
    /// record, one between records. Both are deliberately unlikely to
    /// collide with anything a commit message could contain (a Jjf-Op:
    /// trailer is plain ASCII; the sentinels are punctuated with hex
    /// nonces).
    ///
    /// The order is **chronological commit order (oldest first)** — we
    /// pass `--reverse` to `git log` so the first record is the issue's
    /// `create` commit and the last is the tip. The v3 op-replay folds
    /// in this order, matching the v2 history walker's contract.
    pub(crate) fn walk_commits(
        &self,
        ref_name: &str,
    ) -> Result<Vec<WalkedCommit>, GitError> {
        // Field separator between commit_id / author / timestamp /
        // message inside one record. Record separator between records.
        // Both are escape-friendly for git's `--format=` template
        // (no `%` collisions, no shell metacharacters).
        let field_sep = "----JJF-V3-WALK-FIELD-c0ffee----";
        let record_sep = "----JJF-V3-WALK-REC-c0ffee----";
        // %H: full commit hash. %an <%ae>: author "Name <email>". %aI:
        // ISO-8601 strict author timestamp (RFC 3339). %B: raw subject
        // + body (the commit message, including blank lines and any
        // trailer block). We render the timestamp in the same shape
        // the v2 history walker produces (`YYYY-MM-DDTHH:MM:SSZ`) by
        // post-processing — `%aI` emits e.g. `2026-06-23T23:07:15-04:00`
        // and we normalize via `chrono` if needed, but for the
        // op-replay cross-check we only care about the trailer's
        // `Jjf-At` (preferred) and the commit's relative ordering;
        // the timestamp string is informational. We keep the raw
        // `%aI` here and let the consumer normalize if it needs to.
        let format = format!(
            "%H%n{f}%n%an <%ae>%n{f}%n%aI%n{f}%n%B%n{r}",
            f = field_sep,
            r = record_sep,
        );
        // Resolve absence cleanly: if the ref doesn't exist, `git log`
        // exits non-zero with "unknown revision". `resolve_ref`
        // already has a clean detector; reuse it as a pre-check.
        if self.resolve_ref(ref_name)?.is_none() {
            return Ok(Vec::new());
        }
        let format_arg = format!("--format={}", format);
        let raw = self.run(&[
            "log",
            "--reverse",
            &format_arg,
            ref_name,
        ])?;
        let mut out = Vec::new();
        for record in raw.split(record_sep) {
            let record = record.trim_matches('\n');
            if record.is_empty() {
                continue;
            }
            // Each record is "commit\n<sep>\nauthor\n<sep>\nts\n<sep>\nmessage".
            let parts: Vec<&str> = record.split(field_sep).collect();
            if parts.len() != 4 {
                return Err(GitError::Cli {
                    cmd: format!("git log --reverse --format=... {}", ref_name),
                    status: None,
                    stderr: format!(
                        "internal: walk_commits record split into {} parts (expected 4):\n{:?}",
                        parts.len(),
                        record
                    ),
                });
            }
            // Trim each part: %H is followed by a literal `\n`, the
            // sentinel is wrapped in `\n`s, so each `parts[i]` has
            // newlines surrounding the payload.
            let commit = parts[0].trim_matches('\n').to_owned();
            let author = parts[1].trim_matches('\n').to_owned();
            let timestamp = parts[2].trim_matches('\n').to_owned();
            let message = parts[3].trim_matches('\n').to_owned();
            out.push(WalkedCommit {
                commit,
                author,
                timestamp,
                message,
            });
        }
        Ok(out)
    }
}

/// One commit on a per-issue ref's chain. Returned by
/// [`GitRepo::walk_commits`]. Field shapes mirror the v2
/// `history::HistoryEntry` per-commit fields so the read-path consumer
/// can produce the same `HistoryEntry`s in either mode.
#[derive(Debug, Clone)]
pub(crate) struct WalkedCommit {
    /// Full hex commit oid.
    pub(crate) commit: String,
    /// Rendered as `Name <email>` (git's standard).
    pub(crate) author: String,
    /// Author timestamp as git's `%aI` (RFC 3339 with offset).
    /// Consumers that want UTC seconds normalize themselves; the
    /// op-replay cross-check uses the trailer's `Jjf-At:` for the
    /// LWW key, falling back only when no stamp is present.
    pub(crate) timestamp: String,
    /// Raw `%B` — commit summary + blank line + body + trailer block.
    /// Passed verbatim to the trailer parser.
    pub(crate) message: String,
}

/// Typed error from the git CLI wrapper. Mirrors the shape of
/// [`crate::jj::JjError`] so the storage-side error translation can
/// reuse the same "is this a CAS / concurrent-write failure?" pattern.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("spawning git: {0}")]
    Io(#[source] std::io::Error),
    #[error("`{cmd}` exited {status:?}:\n{stderr}")]
    Cli {
        cmd: String,
        status: Option<i32>,
        stderr: String,
    },
}

impl GitError {
    /// Heuristic: does this git failure look like an `update-ref` CAS
    /// mismatch — i.e. another writer (different process / different
    /// in-process call) landed first and our `expected_old_oid` no
    /// longer matches?
    ///
    /// git's stable phrases for the CAS-failure case across recent
    /// versions:
    ///
    /// - "cannot lock ref '...'"
    /// - "is at <oid> but expected <oid>"
    /// - "reference already exists" (when expected_old was ZERO_OID)
    ///
    /// We pattern-match on substrings; the message wraps slightly
    /// differently across git versions but the substrings stay stable.
    /// Used by `Storage` to translate raw `GitError::Cli` into the
    /// typed [`crate::Error::ConcurrentWrite`] for clean operator UX.
    pub(crate) fn is_concurrent_write(&self) -> bool {
        match self {
            GitError::Cli { stderr, .. } => {
                stderr.contains("cannot lock ref")
                    || stderr.contains("but expected")
                    || stderr.contains("reference already exists")
            }
            GitError::Io(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The literal CAS-failure stderr modern git emits when
    /// `expected_old` doesn't match the on-disk ref. Locks the
    /// detector to the production failure shape so a stderr-format
    /// tweak here can't silently regress.
    #[test]
    fn cannot_lock_ref_is_detected() {
        let err = GitError::Cli {
            cmd: "git update-ref refs/jjf/issues/abc1234 NEW OLD".into(),
            status: Some(128),
            stderr: "fatal: cannot lock ref 'refs/jjf/issues/abc1234': \
                     is at NEWOID but expected OLDOID\n"
                .into(),
        };
        assert!(err.is_concurrent_write());
    }

    #[test]
    fn reference_already_exists_is_detected() {
        // Emitted when expected_old was ZERO_OID and the ref already
        // exists — a create-vs-create race on the same issue id.
        let err = GitError::Cli {
            cmd: "git update-ref refs/jjf/issues/abc1234 NEW ZEROS".into(),
            status: Some(128),
            stderr: "fatal: cannot lock ref 'refs/jjf/issues/abc1234': \
                     reference already exists\n"
                .into(),
        };
        assert!(err.is_concurrent_write());
    }

    /// Unrelated git failures (network, parse, etc.) must NOT trip
    /// the concurrent-write detector — retrying them would just
    /// re-fail the same way.
    #[test]
    fn unrelated_git_errors_are_not_concurrent_write() {
        let bad = GitError::Cli {
            cmd: "git mktree".into(),
            status: Some(128),
            stderr: "fatal: not a valid object name\n".into(),
        };
        assert!(!bad.is_concurrent_write());

        let io = GitError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "git binary not on PATH",
        ));
        assert!(!io.is_concurrent_write());
    }
}
