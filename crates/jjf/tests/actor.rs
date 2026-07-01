//! Integration tests for the v2.12 actor-override-chain
//! (`actor-override-chain`, ticket `ae0866b`).
//!
//! Covers the precedence chain shared by `jjf update --claim` (and
//! `jjf comment` for its author field):
//!
//! ```text
//! --actor flag > JJF_ACTOR env > git config user.name > error
//! ```
//!
//! Plus the empty-string fall-through rule (`--actor ""` and
//! `JJF_ACTOR=""` skip the slot rather than claim with an empty
//! assignee).
//!
//! Env-var hygiene: every test scopes its env tweaks to the child
//! `Command::env(...)` / `Command::env_remove(...)`. Tests do NOT
//! call `std::env::set_var` (would leak across nextest's
//! process-shared tests). Tests that exercise the "JJF_ACTOR
//! unset" path explicitly `env_remove` so the orchestrator's env
//! can't poison the assertion.
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as the other
//! test files in this crate.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

mod common;
use common::{scratch, JJF_BIN};

/// Build a jj repo with the given user.name + user.email pinned in
/// repo-local config. Pass `user = None` to leave user.name unset
/// (for the "no current user" / chain-runs-dry tests).
fn make_jj_repo_with_user(name: &str, user: Option<&str>) -> PathBuf {
    let dir = scratch(name);
    let out = Command::new("jj")
        .args(["git", "init"])
        .current_dir(&dir)
        .output()
        .expect("spawn jj");
    assert!(
        out.status.success(),
        "jj git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    if let Some(user) = user {
        let out = Command::new("jj")
            .args(["config", "set", "--repo", "user.name", user])
            .current_dir(&dir)
            .output()
            .expect("spawn jj config set name");
        assert!(
            out.status.success(),
            "jj config set user.name failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = Command::new("jj")
            .args(["config", "set", "--repo", "user.email", "test@example.com"])
            .current_dir(&dir)
            .output()
            .expect("spawn jj config set email");
        assert!(
            out.status.success(),
            "jj config set user.email failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // J2: also set git config so the binary reads identity from git
        // (the actor chain now calls `git config user.name` instead of
        // `jj config get user.name`). Setting both keeps jj-init working
        // and makes git the authoritative source of truth post-J2.
        let out = Command::new("git")
            .args(["config", "user.name", user])
            .current_dir(&dir)
            .output()
            .expect("spawn git config user.name");
        assert!(
            out.status.success(),
            "git config user.name failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&dir)
            .output()
            .expect("spawn git config user.email");
        assert!(
            out.status.success(),
            "git config user.email failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    dir
}

fn make_initialized_repo_with_user(name: &str, user: Option<&str>) -> PathBuf {
    let repo = make_jj_repo_with_user(name, user);
    let out = Command::new(JJF_BIN)
        .arg("init")
        .current_dir(&repo)
        .output()
        .expect("spawn jjf init");
    assert!(
        out.status.success(),
        "jjf init in {} failed: {}",
        repo.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    repo
}

fn run_jjf_with_env(cwd: &Path, args: &[&str], env: &[(&str, Option<&str>)]) -> Output {
    let mut cmd = Command::new(JJF_BIN);
    cmd.args(args).current_dir(cwd);
    for (k, v) in env {
        match v {
            Some(val) => {
                cmd.env(k, val);
            }
            None => {
                cmd.env_remove(k);
            }
        }
    }
    cmd.output().expect("spawn jjf")
}

fn run_jjf_with_stdin_env(
    cwd: &Path,
    args: &[&str],
    stdin_bytes: &[u8],
    env: &[(&str, Option<&str>)],
) -> Output {
    let mut cmd = Command::new(JJF_BIN);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        match v {
            Some(val) => {
                cmd.env(k, val);
            }
            None => {
                cmd.env_remove(k);
            }
        }
    }
    let mut child = cmd.spawn().expect("spawn jjf");
    child
        .stdin
        .as_mut()
        .expect("stdin handle")
        .write_all(stdin_bytes)
        .expect("write stdin");
    child.wait_with_output().expect("wait for jjf")
}

/// Create an issue via `jjf new`. Always strips `JJF_ACTOR` so the
/// test fixture isn't accidentally created under an actor override.
fn create_issue(repo: &Path, title: &str, extra_args: &[&str]) -> String {
    let mut args: Vec<&str> = vec!["new", "--json", "-t", title, "-F", "-"];
    args.extend_from_slice(extra_args);
    let out = run_jjf_with_stdin_env(repo, &args, b"", &[("JJF_ACTOR", None)]);
    assert!(
        out.status.success(),
        "jjf new failed during setup: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    // `new --json` envelope: `{"ok":true,"id":"abcdef0"}`
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("parse new --json");
    v["id"].as_str().expect("id field").to_owned()
}

/// Read assignee via `show --json` (with `JJF_ACTOR` stripped so the
/// read path is hermetic).
fn show_assignee(repo: &Path, id: &str) -> Option<String> {
    let out = run_jjf_with_env(repo, &["show", "--json", id], &[("JJF_ACTOR", None)]);
    assert!(
        out.status.success(),
        "jjf show failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("parse show --json");
    v["assignee"].as_str().map(|s| s.to_owned())
}

/// Last comment's author via `show --json`.
fn last_comment_author(repo: &Path, id: &str) -> Option<String> {
    let out = run_jjf_with_env(repo, &["show", "--json", id], &[("JJF_ACTOR", None)]);
    assert!(
        out.status.success(),
        "jjf show failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("parse show --json");
    let comments = v["comments"].as_array().expect("comments array");
    comments
        .last()
        .and_then(|c| c["author"].as_str().map(|s| s.to_owned()))
}

// --- update --claim chain ----------------------------------------

#[test]
fn claim_actor_flag_overrides_env_and_config() {
    let repo = make_initialized_repo_with_user("actor_flag_wins", Some("config-user"));
    let id = create_issue(&repo, "flag-wins", &[]);

    // Flag wins even when env is set to a different name.
    let out = run_jjf_with_env(
        &repo,
        &["update", &id, "--claim", "--actor", "flag-bob"],
        &[("JJF_ACTOR", Some("env-alice"))],
    );
    assert!(
        out.status.success(),
        "update --claim --actor failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let assignee = show_assignee(&repo, &id);
    assert_eq!(
        assignee.as_deref(),
        Some("flag-bob"),
        "--actor flag must win over JJF_ACTOR env and jj config"
    );
}

#[test]
fn claim_env_used_when_flag_absent() {
    let repo = make_initialized_repo_with_user("actor_env_wins", Some("config-user"));
    let id = create_issue(&repo, "env-wins", &[]);

    let out = run_jjf_with_env(
        &repo,
        &["update", &id, "--claim"],
        &[("JJF_ACTOR", Some("env-alice"))],
    );
    assert!(
        out.status.success(),
        "update --claim with env failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let assignee = show_assignee(&repo, &id);
    assert_eq!(
        assignee.as_deref(),
        Some("env-alice"),
        "JJF_ACTOR env must beat jj config when flag is absent"
    );
}

#[test]
fn claim_config_used_when_flag_and_env_absent() {
    let repo = make_initialized_repo_with_user("actor_config_fallback", Some("config-user"));
    let id = create_issue(&repo, "config-fallback", &[]);

    let out = run_jjf_with_env(
        &repo,
        &["update", &id, "--claim"],
        &[("JJF_ACTOR", None)],
    );
    assert!(
        out.status.success(),
        "update --claim with config fallback failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let assignee = show_assignee(&repo, &id);
    assert_eq!(
        assignee.as_deref(),
        Some("config-user"),
        "jj config user.name must win when no flag/env override"
    );
}

#[test]
fn claim_empty_env_falls_through_to_config() {
    let repo = make_initialized_repo_with_user("actor_empty_env", Some("config-user"));
    let id = create_issue(&repo, "empty-env", &[]);

    let out = run_jjf_with_env(
        &repo,
        &["update", &id, "--claim"],
        &[("JJF_ACTOR", Some(""))],
    );
    assert!(
        out.status.success(),
        "update --claim with empty env failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let assignee = show_assignee(&repo, &id);
    assert_eq!(
        assignee.as_deref(),
        Some("config-user"),
        "JJF_ACTOR=\"\" must fall through, not write an empty assignee"
    );
}

#[test]
fn claim_empty_flag_falls_through_to_env() {
    let repo = make_initialized_repo_with_user("actor_empty_flag", Some("config-user"));
    let id = create_issue(&repo, "empty-flag", &[]);

    let out = run_jjf_with_env(
        &repo,
        &["update", &id, "--claim", "--actor", ""],
        &[("JJF_ACTOR", Some("env-alice"))],
    );
    assert!(
        out.status.success(),
        "update --claim with empty flag failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let assignee = show_assignee(&repo, &id);
    assert_eq!(
        assignee.as_deref(),
        Some("env-alice"),
        "--actor \"\" must fall through to JJF_ACTOR env, not write empty"
    );
}

#[test]
fn claim_all_slots_empty_errors_no_current_user() {
    // Set up a fixture where every chain slot is empty: no
    // `--actor` (or empty), no `JJF_ACTOR`, and no user.name in
    // ANY config scope. The repo-local config is unset by construction
    // (we pass `user = None` to the fixture). We block both global
    // config sources: `JJ_CONFIG` → nonexistent path (for legacy jj
    // paths), `GIT_CONFIG_GLOBAL` → /dev/null (for the new git-config
    // resolution path added in J2). v2.12 (`actor-override-chain`).
    let repo = make_initialized_repo_with_user("actor_no_current_user", None);

    // Create the issue under a JJF_ACTOR fallback so the
    // create_issue helper doesn't itself hit the no-user error.
    let mut cmd = Command::new(JJF_BIN);
    cmd.args(["new", "--json", "-t", "no-user", "-F", "-"])
        .current_dir(&repo)
        .env("JJF_ACTOR", "setup-actor")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn jjf new");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"")
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "setup failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("parse");
    let id = v["id"].as_str().expect("id").to_owned();

    // Now run --claim with JJF_ACTOR explicitly unset AND both config
    // escape hatches engaged:
    //   - `JJ_CONFIG` → nonexistent path: blocks jj's global config
    //     (`~/.config/jj/config.toml`, which carries `user.name = Tester`).
    //   - `GIT_CONFIG_GLOBAL` → /dev/null: blocks git's global config
    //     (`~/.gitconfig`, which may carry `user.name`). After J2 the
    //     binary reads from `git config`, so this prevents the global
    //     git identity from satisfying the chain.
    // The repo-local git config is unset by construction (None was passed
    // to make_initialized_repo_with_user, so no git config user.name was
    // written to .git/config). Together these ensure every chain slot is
    // genuinely empty and the binary must error.
    let empty_config = repo.join("nonexistent-config.toml");
    let empty_config_str = empty_config.to_string_lossy();
    let out = run_jjf_with_env(
        &repo,
        &["update", "--json", &id, "--claim", "--actor", ""],
        &[
            ("JJF_ACTOR", None),
            ("JJ_CONFIG", Some(empty_config_str.as_ref())),
            ("GIT_CONFIG_GLOBAL", Some("/dev/null")),
        ],
    );
    assert!(
        !out.status.success(),
        "expected non-zero exit when chain runs dry, got success: stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    // Per CliError::NoCurrentUser, exit code is 2 (preflight).
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit 2, stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("JJF_ACTOR") || stderr.contains("no current user"),
        "expected no_current_user hint mentioning JJF_ACTOR, got: {stderr}"
    );
}

#[test]
fn claim_actor_conflicts_with_unclaim() {
    let repo = make_initialized_repo_with_user("actor_conflict_unclaim", Some("config-user"));
    let id = create_issue(&repo, "conflict-unclaim", &[]);

    let out = run_jjf_with_env(
        &repo,
        &["update", &id, "--unclaim", "--actor", "flag-bob"],
        &[("JJF_ACTOR", None)],
    );
    assert!(
        !out.status.success(),
        "--actor + --unclaim must be a clap conflict, got success: stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot be used with") || stderr.contains("conflict"),
        "expected clap conflict-with message, got: {stderr}"
    );
}

#[test]
fn claim_actor_conflicts_with_assignee() {
    let repo = make_initialized_repo_with_user("actor_conflict_assignee", Some("config-user"));
    let id = create_issue(&repo, "conflict-assignee", &[]);

    let out = run_jjf_with_env(
        &repo,
        &[
            "update",
            &id,
            "--actor",
            "flag-bob",
            "--assignee",
            "explicit",
        ],
        &[("JJF_ACTOR", None)],
    );
    assert!(
        !out.status.success(),
        "--actor + --assignee must be a clap conflict, got success: stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn claim_actor_conflicts_with_unset_assignee() {
    let repo = make_initialized_repo_with_user("actor_conflict_unset", Some("config-user"));
    let id = create_issue(&repo, "conflict-unset", &[]);

    let out = run_jjf_with_env(
        &repo,
        &["update", &id, "--actor", "flag-bob", "--unset-assignee"],
        &[("JJF_ACTOR", None)],
    );
    assert!(
        !out.status.success(),
        "--actor + --unset-assignee must be a clap conflict, got success: stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// --- comment author chain ----------------------------------------

#[test]
fn comment_env_drives_author_when_flag_absent() {
    let repo = make_initialized_repo_with_user("comment_env_author", Some("config-user"));
    let id = create_issue(&repo, "comment-env", &[]);

    let out = run_jjf_with_stdin_env(
        &repo,
        &["comment", &id, "-F", "-"],
        b"hello from env-alice",
        &[("JJF_ACTOR", Some("env-alice"))],
    );
    assert!(
        out.status.success(),
        "comment with JJF_ACTOR failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let author = last_comment_author(&repo, &id).expect("author present");
    // The synthesized author is `env-alice <test@example.com>`
    // (user.email is set in the fixture).
    assert!(
        author.starts_with("env-alice"),
        "expected author to start with `env-alice`, got: {author}"
    );
    assert!(
        author.contains("<test@example.com>"),
        "expected synthesized email suffix, got: {author}"
    );
}

#[test]
fn comment_author_flag_still_wins_over_env() {
    let repo = make_initialized_repo_with_user("comment_flag_wins", Some("config-user"));
    let id = create_issue(&repo, "comment-flag-wins", &[]);

    let out = run_jjf_with_stdin_env(
        &repo,
        &[
            "comment",
            &id,
            "--author",
            "Flag Author <flag@example.com>",
            "-F",
            "-",
        ],
        b"hello from --author",
        &[("JJF_ACTOR", Some("env-alice"))],
    );
    assert!(
        out.status.success(),
        "comment with --author failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let author = last_comment_author(&repo, &id).expect("author present");
    assert_eq!(
        author, "Flag Author <flag@example.com>",
        "--author must win over JJF_ACTOR env"
    );
}

#[test]
fn comment_falls_back_to_config_when_no_env_or_flag() {
    let repo = make_initialized_repo_with_user("comment_config_fallback", Some("config-user"));
    let id = create_issue(&repo, "comment-config", &[]);

    let out = run_jjf_with_stdin_env(
        &repo,
        &["comment", &id, "-F", "-"],
        b"hello from config",
        &[("JJF_ACTOR", None)],
    );
    assert!(
        out.status.success(),
        "comment with config fallback failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let author = last_comment_author(&repo, &id).expect("author present");
    assert!(
        author.starts_with("config-user"),
        "expected author to start with config-user, got: {author}"
    );
}

// --- J2: git config resolution ------------------------------------

/// Attribution resolves from `git config user.name` when jj config has
/// NO user.name but git config does. Tests Task J2 (jj-divorce): after
/// rerouting resolution from `jj config get` to `git config`, the binary
/// reads identity from git, not jj.
#[test]
fn claim_uses_git_config_user_name() {
    // jj git init (required for jjf init), but do NOT set jj user config.
    let root = make_jj_repo_with_user("claim_uses_git_config", None);

    // Set user identity via git config (repo-local) only — jj config
    // user.name is deliberately unset above. Pre-J2, the binary reads
    // `jj config get user.name`, which returns nothing (no jj config).
    // Post-J2, it reads `git config user.name`, which returns "Git Person".
    let out = std::process::Command::new("git")
        .args(["-C", root.to_str().unwrap(), "config", "user.name", "Git Person"])
        .output()
        .unwrap();
    assert!(out.status.success(), "git config user.name failed");

    let out = std::process::Command::new("git")
        .args(["-C", root.to_str().unwrap(), "config", "user.email", "git@example.com"])
        .output()
        .unwrap();
    assert!(out.status.success(), "git config user.email failed");

    // Initialize jjf storage.
    let out = std::process::Command::new(JJF_BIN)
        .arg("init")
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "jjf init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Create issue under a setup actor so `new` doesn't hit the no-user error.
    let out = run_jjf_with_env(
        &root,
        &["new", "--json", "-t", "claim me", "-F", "-"],
        &[("JJF_ACTOR", Some("setup-actor"))],
    );
    assert!(out.status.success(), "jjf new failed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("parse new --json");
    let id = v["id"].as_str().expect("id field").to_owned();

    // --claim with no JJF_ACTOR / --actor: must fall back to git config user.name.
    // Pre-J2 this would fail (jj config has no user.name → NoCurrentUser).
    // Post-J2 this must succeed (git config has "Git Person").
    let out = run_jjf_with_env(
        &root,
        &["update", &id, "--claim", "--json"],
        &[("JJF_ACTOR", None)],
    );
    assert!(
        out.status.success(),
        "update --claim via git config failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let assignee = show_assignee(&root, &id).expect("assignee should be set");
    assert_eq!(
        assignee, "Git Person",
        "--claim must resolve from git config user.name, got: {assignee}"
    );
}
