# git-issues Rename + Total jj Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove every `jj`-CLI dependency from the project and rename it from `jjforge`/`jjf` to `git-issues`/`iss`.

**Architecture:** Two sequenced workstreams. **J (jj divorce)** deletes v2-legacy/migration code, reroutes the `remote` verbs and actor/preflight probes from `jj` subprocesses to plain `git`, and removes `JjRepo`; the v3 data layer already uses git plumbing so nothing on the live path changes. **R (rename)** is a mechanical sweep of crate names, identifiers, env vars, and docs. The on-disk wire format (`refs/jjf/*` refs, `Jjf-*` trailers) is deliberately kept unchanged as vestigial tokens.

**Tech Stack:** Rust (Cargo workspace), `git` CLI plumbing, `cargo nextest`.

## Global Constraints

- **Never touch the wire format.** `refs/jjf/*` ref literals, `Jjf-*` trailer-key constants, and `ISSUES_BOOKMARK`/`*_REVSET` constants that survive J must NOT be renamed. They are vestigial wire tokens. (Spec option A.)
- **J before R.** Complete all of Workstream J (and its green test gate) before starting any of Workstream R.
- **Commit discipline:** add files by explicit name; never `git add .` / `git add -A`. End commit messages with `Claude-Session: https://claude.ai/code/session_01VGhMvpYpR2YkT6inGLXoUT`.
- **Test gate between tasks:** `cargo nextest run --workspace` (fall back to `cargo test --workspace` if nextest is absent) must be green before moving to the next task.
- **No v2 support after J1.** Deleting the v2→v3 migrator means a v2-shape repo becomes unreadable by the binary. This is accepted; both live host repos are v3.
- Spec: `docs/superpowers/specs/2026-06-30-git-issues-rename-jj-removal-design.md`.

---

## Workstream J — total jj divorce

### Task J1: Reroute the `remote` verbs and `jj_repo` preflight to plain git

The `remote add|ls|rm` verbs and the `preflight::jj_repo` probe are the only jj usages that run *before* `iss init` (so they cannot assume a v3 sentinel). Reroute them to `git`. After this task, `remote *` no longer spawns `jj`.

**Files:**
- Modify: `crates/jjf/src/main.rs` (remote-verb fns near lines 3220, 3343, 3407)
- Modify: `crates/jjf/src/preflight.rs:44-64` (`jj_repo`)
- Test: `crates/jjf/tests/remote.rs` (existing — extend)

**Interfaces:**
- Produces: `preflight::jj_repo(cwd: &Path) -> Result<(), CliError>` — unchanged signature, now backed by `git rev-parse --git-dir`.

- [ ] **Step 1: Write the failing test** — assert a remote round-trips in a plain git repo with no jj. Add to `crates/jjf/tests/remote.rs`:

```rust
#[test]
fn remote_add_ls_rm_in_plain_git_repo() {
    // Bare git repo, NO `jj git init`.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::process::Command::new("git").arg("init").arg(root).output().unwrap();

    let add = iss_cmd(root, &["remote", "add", "origin", "https://example.com/x.git"]);
    assert!(add.status.success(), "remote add failed: {}", String::from_utf8_lossy(&add.stderr));

    let ls = iss_cmd(root, &["remote", "ls", "--json"]);
    assert!(ls.status.success());
    assert!(String::from_utf8_lossy(&ls.stdout).contains("origin"));

    let rm = iss_cmd(root, &["remote", "rm", "origin"]);
    assert!(rm.status.success());
}
```

(Use the test helper that invokes the built binary; mirror the existing `remote.rs` helper. If the helper is named differently than `iss_cmd`, match the existing name.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p jjf remote_add_ls_rm_in_plain_git_repo`
Expected: FAIL (current code shells to `jj git remote`, which errors in a non-jj repo).

- [ ] **Step 3: Reroute `preflight::jj_repo` to git.** Replace the body (lines ~44-64) with a `git rev-parse` probe:

```rust
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
```

(Keep the `NotAJjRepo` variant name for now — it is renamed in R, not J. The doc-comment's "jj repo" wording is cosmetic; leave for the R doc sweep.)

- [ ] **Step 4: Reroute the three remote verbs to `git remote`.** In `main.rs`:
  - `remote add` (~3220): `jj --repository <cwd> git remote add <name> <url>` → `git -C <cwd> remote add <name> <url>`.
  - `remote ls` (~3343): `jj --repository <cwd> git remote list` → `git -C <cwd> remote -v`, then parse. `git remote -v` prints two lines per remote (`<name>\t<url> (fetch)` / `(push)`); dedupe by name. The existing JSON shape (list of `{name, url}`) stays; build it from the parsed fetch lines.
  - `remote rm` (~3407): `jj --repository <cwd> git remote remove <name>` → `git -C <cwd> remote remove <name>`.

- [ ] **Step 5: Run the new test + existing remote tests**

Run: `cargo nextest run -p jjf --test remote`
Expected: PASS (all remote tests, including the new plain-git one).

- [ ] **Step 6: Commit**

```bash
git add crates/jjf/src/main.rs crates/jjf/src/preflight.rs crates/jjf/tests/remote.rs
git commit -F - <<'EOF'
j: reroute remote verbs + jj_repo preflight to plain git

`remote add|ls|rm` and preflight::jj_repo no longer spawn `jj`; they
use `git remote` and `git rev-parse --git-dir`. Works in a plain git
repo with jj absent.

Claude-Session: https://claude.ai/code/session_01VGhMvpYpR2YkT6inGLXoUT
EOF
```

---

### Task J2: Reroute the actor-identity chain from `jj config get` to `git config`

The `--claim` / comment-attribution chain resolves the current user via `jj config get user.name` / `user.email`. Reroute to `git config`.

**Files:**
- Modify: `crates/jjf/src/main.rs` (`jj_config_get` ~3854, and the actor-resolution fns ~3773-3812)
- Test: `crates/jjf/tests/actor.rs` (existing — extend)

**Interfaces:**
- Produces: a `git_config_get(key: &str) -> Result<Option<String>, CliError>` replacing `jj_config_get`. Same `Option` semantics (absent key → `Ok(None)`).

- [ ] **Step 1: Write the failing test** — claim attribution resolves from `git config user.name` with jj absent. Add to `crates/jjf/tests/actor.rs`:

```rust
#[test]
fn claim_uses_git_config_user_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::process::Command::new("git").arg("init").arg(root).output().unwrap();
    std::process::Command::new("git").args(["-C", root.to_str().unwrap(),
        "config", "user.name", "Git Person"]).output().unwrap();
    std::process::Command::new("git").args(["-C", root.to_str().unwrap(),
        "config", "user.email", "git@example.com"]).output().unwrap();

    iss_cmd(root, &["init"]);
    let id = create_issue(root, "claim me"); // mirror existing helper
    // No JJF_ACTOR / ISS_ACTOR env, no --actor: must fall back to git config.
    let out = iss_cmd_env(root, &[], &["update", &id, "--claim", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let show = iss_cmd(root, &["show", &id, "--json"]);
    assert!(String::from_utf8_lossy(&show.stdout).contains("Git Person"));
}
```

(Match existing helper names in `actor.rs`; `iss_cmd_env` clears `JJF_ACTOR`/`ISS_ACTOR` for the call.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p jjf claim_uses_git_config_user_name`
Expected: FAIL (resolution still calls `jj config get`).

- [ ] **Step 3: Replace `jj_config_get` with `git_config_get`.** In `main.rs` (~3854):

```rust
/// `git config <key>` exits non-zero when the key is absent — we treat
/// that as "not configured" (Ok(None)) rather than a hard probe failure.
fn git_config_get(key: &str) -> Result<Option<String>, CliError> {
    let out = std::process::Command::new("git")
        .args(["config", key])
        .output()
        .map_err(CliError::Probe)?;
    if !out.status.success() {
        return Ok(None);
    }
    let val = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if val.is_empty() { Ok(None) } else { Ok(Some(val)) }
}
```

Update the two call sites that read `user.name` / `user.email` to call `git_config_get` instead of `jj_config_get`. (Leave the `JJF_ACTOR` env name as-is — it is renamed in R.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p jjf --test actor`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jjf/src/main.rs crates/jjf/tests/actor.rs
git commit -F - <<'EOF'
j: resolve actor identity via git config, not jj config

The --claim / attribution chain reads user.name/user.email from
`git config` instead of `jj config get`. No jj on the actor path.

Claude-Session: https://claude.ai/code/session_01VGhMvpYpR2YkT6inGLXoUT
EOF
```

---

### Task J3: Collapse `issues_bookmark` preflight to the v3 sentinel only

`preflight::issues_bookmark` currently checks the v3 sentinel, then falls back to the v2 `issues` bookmark and the v1 `bugs` bookmark (both via `jj bookmark list`). Drop the v2/v1 fallback; v3 is the only supported shape.

**Files:**
- Modify: `crates/jjf/src/preflight.rs:76-128` (`issues_bookmark`)
- Test: `crates/jjf/tests/init.rs` (existing — extend)

**Interfaces:**
- Produces: `preflight::issues_bookmark(cwd: &Path) -> Result<(), CliError>` — unchanged signature; now only `jj_repo(cwd)?` + the v3 sentinel check.

- [ ] **Step 1: Write the failing test** — a git repo that is NOT `iss init`-ed is rejected without spawning jj. Add to `crates/jjf/tests/init.rs`:

```rust
#[test]
fn verb_on_uninitialized_repo_rejects_without_jj() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::process::Command::new("git").arg("init").arg(root).output().unwrap();
    // No `iss init` → no refs/jjf/meta/format-version sentinel.
    let out = iss_cmd(root, &["ls"]);
    assert!(!out.status.success(), "ls should refuse on uninitialized repo");
    // After init, it works:
    assert!(iss_cmd(root, &["init"]).status.success());
    assert!(iss_cmd(root, &["ls"]).status.success());
}
```

- [ ] **Step 2: Run test to verify it fails or errors via jj**

Run: `cargo nextest run -p jjf verb_on_uninitialized_repo_rejects_without_jj`
Expected: FAIL or error (current code calls `jj bookmark list`, which errors in a plain-git repo rather than returning the clean `MissingIssuesBookmark`).

- [ ] **Step 3: Rewrite `issues_bookmark`** (lines ~76-128). Keep only the jj_repo probe + sentinel:

```rust
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
```

Remove the now-unused `V1_BUGS_BOOKMARK` / `ISSUES_BOOKMARK` imports from `preflight.rs` if they become unused (compiler will flag). Do NOT delete the constants from their defining module — they may still be referenced by surviving v3 code; only drop the unused `use` in `preflight.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p jjf --test init`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jjf/src/preflight.rs crates/jjf/tests/init.rs
git commit -F - <<'EOF'
j: collapse issues_bookmark preflight to v3 sentinel only

Drops the v2 `issues` / v1 `bugs` bookmark fallbacks (both jj-backed).
The v3 sentinel ref is the only init marker; uninitialized repos get a
clean MissingIssuesBookmark instead of a jj error.

Claude-Session: https://claude.ai/code/session_01VGhMvpYpR2YkT6inGLXoUT
EOF
```

---

### Task J4: Delete the v2-legacy push/pull paths

Remove `run_push_v2_legacy`, the v2 branch of `run_pull` / `probe_pull_mode`, and collapse `run_push`/`run_pull` to the v3 path unconditionally.

**Files:**
- Modify: `crates/jjf/src/main.rs` (`run_push` ~4616, `run_push_v2_legacy` ~4662, `run_pull` ~4849, `probe_pull_mode` ~4893, the v2 merge/bookmark dance ~5033-5140)
- Test: `crates/jjf/tests/push_pull.rs` (existing — should stay green)

**Interfaces:**
- Produces: `run_push` / `run_pull` call only their v3 implementations; no `PullMode` enum branch for v2.

- [ ] **Step 1: Delete `run_push_v2_legacy` and simplify `run_push`.** Replace `run_push` (~4616) so it always calls `run_push_v3`:

```rust
fn run_push(json: bool, remote: String) -> Result<(), CliError> {
    let cwd: PathBuf = std::env::current_dir().map_err(CliError::Cwd)?;
    let cwd = std::fs::canonicalize(&cwd).map_err(CliError::Cwd)?;
    preflight::issues_bookmark(&cwd)?;
    let storage = Storage::open(&cwd).map_err(CliError::Storage)?;
    run_push_v3(json, &remote, &storage)
}
```

Delete the entire `run_push_v2_legacy` fn.

- [ ] **Step 2: Simplify `run_pull` to the v3 path and delete `probe_pull_mode`.** Replace `run_pull` (~4849) so it always runs the v3 pull, and remove `probe_pull_mode` (~4893) and the `PullMode` enum plus the v2 merge/`bookmark set`/`jj new root()` block (~5033-5140). The v3 pull body that exists today (`run_pull_v3`) stays.

- [ ] **Step 3: Build and run push/pull tests**

Run: `cargo nextest run -p jjf --test push_pull`
Expected: PASS (these tests exercise v3 round-trips; v2 cases, if any, are removed in the test sweep at J7).

- [ ] **Step 4: Verify no jj subprocess remains in push/pull.**

Run: `grep -n 'Command::new("jj")' crates/jjf/src/main.rs`
Expected: only the remaining occurrences are NONE (all of main.rs's jj calls were in remote/config/v2 paths now removed). If any remain, they belong to a path not yet handled — stop and reconcile.

- [ ] **Step 5: Commit**

```bash
git add crates/jjf/src/main.rs
git commit -F - <<'EOF'
j: delete v2-legacy push/pull paths

run_push/run_pull always use the v3 git-refspec path. Removed
run_push_v2_legacy, probe_pull_mode, the PullMode enum, and the v2
`jj bookmark set` / `jj new root()` merge dance. No jj in sync.

Claude-Session: https://claude.ai/code/session_01VGhMvpYpR2YkT6inGLXoUT
EOF
```

---

### Task J5: Delete the v2→v3 migrator and v2 cache probes

Remove `migrate_v2_v3.rs` entirely and the v2-only cache probes that shell to jj.

**Files:**
- Delete: `crates/jjf-storage/src/migrate_v2_v3.rs`
- Modify: `crates/jjf-storage/src/lib.rs` (remove `mod migrate_v2_v3;` and the call that invokes the migrator from `Storage::open`)
- Modify: `crates/jjf-storage/src/cache.rs` (delete `probe_head_commit` ~286 and the v2 `jj file show` path ~825+; keep `rebuild_v3` / `load_or_rebuild`)
- Test: `crates/jjf-storage/tests/v2_to_v3_migration.rs` (delete — covers removed behavior)

**Interfaces:**
- Produces: `Storage::open` no longer attempts a v2→v3 migration; it opens v3 repos directly. A v2-shape repo now surfaces as an error from the v3 read path rather than auto-migrating.

- [ ] **Step 1: Delete the migration test and the migrator module.**

```bash
git rm crates/jjf-storage/tests/v2_to_v3_migration.rs
git rm crates/jjf-storage/src/migrate_v2_v3.rs
```

- [ ] **Step 2: Remove the migrator wiring from `lib.rs`.** Delete `mod migrate_v2_v3;` and the `Storage::open` block that detects v2 shape and calls the migrator. `Storage::open` should open the v3 layout directly (the existing v3 path).

- [ ] **Step 3: Remove the v2 cache probes from `cache.rs`.** Delete `probe_head_commit` (~286-300) and the `jj file show` fallback path (~825+). Verify `rebuild_v3` / `load_or_rebuild` (the v3 ref-set cache, ~433) remain and don't reference the deleted fns.

- [ ] **Step 4: Build and run the storage tests.**

Run: `cargo nextest run -p jjf-storage`
Expected: PASS. (The v3 read/write/sync tests stay green; the deleted migration test is gone.)

- [ ] **Step 5: Commit**

```bash
git add crates/jjf-storage/src/lib.rs crates/jjf-storage/src/cache.rs
git commit -F - <<'EOF'
j: delete v2->v3 migrator and v2 cache probes

Removes migrate_v2_v3.rs, its test, and the jj-backed v2 cache probes
(probe_head_commit, `jj file show`). Storage::open opens v3 directly.
v2-shape repos are no longer auto-migrated (accepted, all live hosts
are v3).

Claude-Session: https://claude.ai/code/session_01VGhMvpYpR2YkT6inGLXoUT
EOF
```

---

### Task J6: Delete `JjRepo` and strip the self-host guard

After J1-J5, `JjRepo` and the `JJF_ALLOW_SELF_HOST` guard have no live callers.

**Files:**
- Delete: `crates/jjf-storage/src/jj.rs`
- Modify: `crates/jjf-storage/src/lib.rs` (remove `mod jj;`, `pub use jj::JjError`, `use jj::JjRepo`, and the three `JjRepo::open` sites ~1565/1621/1673)
- Modify: callers of `JjError` (`cache.rs`, `read.rs`, `merge_ops.rs`, `history.rs` per the imports) — repoint to the git error type
- Modify: `crates/jjf/src/main.rs` / `preflight.rs` — remove the `JJF_ALLOW_SELF_HOST` env read and `refuse_self_hosted_write` preflight if present

**Interfaces:**
- Produces: no `JjRepo` type; storage error surface is the git error type (`GitError`). Repo handle on the data path is `GitRepo`.

- [ ] **Step 1: Find every `JjRepo` / `JjError` / `JJF_ALLOW_SELF_HOST` reference.**

Run: `grep -rn 'JjRepo\|JjError\|JJF_ALLOW_SELF_HOST\|refuse_self_hosted' crates --include='*.rs' | grep -v '/tests/'`
Expected: a finite list — the imports in `history.rs`, `cache.rs`, `read.rs`, `merge_ops.rs`, `lib.rs`, and any guard in `main.rs`/`preflight.rs`.

- [ ] **Step 2: Repoint `JjError` usages to `GitError`.** For each file importing `crate::jj::JjError`, switch the error match/return to the git error type. The `JjError::Cli { stderr, .. }` matches (in `cache.rs`/`migrate` — migrate already deleted) become the equivalent `GitError` variant. Where a `JjRepo` field on a struct is no longer constructed, remove the field and its `JjRepo::open` initializer.

- [ ] **Step 3: Delete `jj.rs` and its module wiring.**

```bash
git rm crates/jjf-storage/src/jj.rs
```

Remove `mod jj;`, `pub use jj::JjError;`, and `use jj::JjRepo;` from `lib.rs`.

- [ ] **Step 4: Remove the self-host guard.** Delete the `JJF_ALLOW_SELF_HOST` env read and the `refuse_self_hosted_write` preflight (and its call sites on mutating verbs) if they still exist.

- [ ] **Step 5: Build the workspace.**

Run: `cargo build --workspace`
Expected: clean compile (no `JjRepo`/`JjError` unresolved).

- [ ] **Step 6: Commit**

```bash
git add crates/jjf-storage/src/lib.rs crates/jjf-storage/src/cache.rs crates/jjf-storage/src/read.rs crates/jjf-storage/src/merge_ops.rs crates/jjf-storage/src/history.rs crates/jjf/src/main.rs crates/jjf/src/preflight.rs
git commit -F - <<'EOF'
j: delete JjRepo and the self-host guard

No code path constructs JjRepo or reads JJF_ALLOW_SELF_HOST after the
v2 removal. Storage errors flow through GitError. The binary no longer
shells to jj anywhere.

Claude-Session: https://claude.ai/code/session_01VGhMvpYpR2YkT6inGLXoUT
EOF
```

---

### Task J7: Add the jj-absent acceptance test and sweep test fixtures

Prove the divorce: a full lifecycle in a plain git repo with jj scrubbed from `PATH`, and remove any test fixtures that depend on jj setup.

**Files:**
- Create: `crates/jjf/tests/no_jj.rs`
- Modify: any test helper that runs `jj git init` to set up a repo (switch to `git init`)

**Interfaces:**
- Consumes: the built `iss`/`jjf` binary (still named `jjf` until R).

- [ ] **Step 1: Write the acceptance test.** `crates/jjf/tests/no_jj.rs`:

```rust
// Full lifecycle with jj absent from PATH.
#[test]
fn full_lifecycle_without_jj_on_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::process::Command::new("git").arg("init").arg(root).output().unwrap();
    std::process::Command::new("git").args(["-C", root.to_str().unwrap(),
        "config", "user.name", "Tester"]).output().unwrap();
    std::process::Command::new("git").args(["-C", root.to_str().unwrap(),
        "config", "user.email", "t@example.com"]).output().unwrap();

    // Scrub jj from PATH: point PATH at a dir containing only the symlinks
    // we need (git, the binary under test). Simplest portable approach:
    // run with a PATH that excludes any dir containing `jj`. Here we set
    // PATH to the dir of `git` plus the target dir, deliberately omitting
    // any jj location.
    let bin = env!("CARGO_BIN_EXE_jjf"); // binary exe path; rename to _iss in R
    let run = |args: &[&str]| {
        std::process::Command::new(bin)
            .current_dir(root)
            .args(args)
            .env("PATH", minimal_path_without_jj()) // helper below
            .output().unwrap()
    };

    assert!(run(&["init"]).status.success());
    let new = run(&["new", "-t", "first issue", "-F", "-"]); // body via stdin empty
    assert!(new.status.success());
    assert!(run(&["ls", "--json"]).status.success());
    // show roadmap-less repo: just ensure ls + a created issue read back
    let ls = run(&["ls", "--json"]);
    assert!(String::from_utf8_lossy(&ls.stdout).contains("first issue"));
}

fn minimal_path_without_jj() -> String {
    // Keep only directories that don't contain a `jj` executable.
    std::env::var("PATH").unwrap_or_default()
        .split(':')
        .filter(|d| !std::path::Path::new(d).join("jj").exists())
        .collect::<Vec<_>>()
        .join(":")
}
```

(If `new -F -` needs stdin, pipe an empty body; adapt to the existing `new` test helper's stdin pattern.)

- [ ] **Step 2: Run it — expect PASS.**

Run: `cargo nextest run -p jjf --test no_jj`
Expected: PASS. If it spawns jj anywhere, the test fails because jj is unreachable on the scrubbed PATH — that failure is the signal to find the stray call.

- [ ] **Step 3: Sweep test setup helpers.** Find fixtures that initialize repos via jj:

Run: `grep -rn 'jj git init\|"jj"' crates/jjf/tests crates/jjf-storage/tests crates/jjf-merge/tests`
For each, switch repo setup to `git init` (v3 needs no jj). Delete any test asserting v2/v1 behavior removed in J4/J5.

- [ ] **Step 4: Full workspace test run.**

Run: `cargo nextest run --workspace`
Expected: PASS (green gate closing Workstream J).

- [ ] **Step 5: Commit**

```bash
git add crates/jjf/tests/no_jj.rs
# plus any modified test files, by explicit name
git commit -F - <<'EOF'
j: add jj-absent acceptance test; migrate fixtures off jj

Full init→new→ls lifecycle passes with jj scrubbed from PATH. Test
fixtures set up repos via `git init` instead of `jj git init`. This
closes the jj divorce.

Claude-Session: https://claude.ai/code/session_01VGhMvpYpR2YkT6inGLXoUT
EOF
```

---

## Workstream R — rename to git-issues / `iss`

Mechanical. Each task ends green. Do NOT start until J7's workspace test is green.

### Task R1: Rename the crates and binary

**Files:**
- Rename dirs: `crates/jjf/` → `crates/iss/`, `crates/jjf-storage/` → `crates/iss-storage/`, `crates/jjf-merge/` → `crates/iss-merge/`
- Modify: workspace `Cargo.toml`, each crate `Cargo.toml`, all `use jjf_storage::` / `use jjf_merge::`

- [ ] **Step 1: Move the crate directories with git.**

```bash
git mv crates/jjf crates/iss
git mv crates/jjf-storage crates/iss-storage
git mv crates/jjf-merge crates/iss-merge
```

- [ ] **Step 2: Update Cargo manifests.** In the workspace root `Cargo.toml`, update `members` paths. In each crate `Cargo.toml`: set `name` (`iss`, `iss-storage`, `iss-merge`), the `[[bin]] name = "iss"` in the binary crate, and every `[dependencies]` entry referencing the renamed crates (e.g. `jjf-storage = { path = "../jjf-storage" }` → `iss-storage = { path = "../iss-storage" }`).

- [ ] **Step 3: Update crate-path imports in Rust source.**

```bash
grep -rl 'jjf_storage\|jjf_merge' crates --include='*.rs'
```

For each file, replace `jjf_storage` → `iss_storage` and `jjf_merge` → `iss_merge` (crate names with `-` become `_` in `use` paths). Also fix the `CARGO_BIN_EXE_jjf` reference in `crates/iss/tests/no_jj.rs` → `CARGO_BIN_EXE_iss`.

- [ ] **Step 4: Build and test.**

Run: `cargo build --workspace && cargo nextest run --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates
git commit -F - <<'EOF'
r: rename crates jjf/jjf-storage/jjf-merge -> iss/iss-storage/iss-merge

Binary is now `iss`. Crate-path imports updated. Wire format
(refs/jjf/*, Jjf-* trailers) deliberately unchanged.

Claude-Session: https://claude.ai/code/session_01VGhMvpYpR2YkT6inGLXoUT
EOF
```

---

### Task R2: Rename in-code identifiers (env var, cache path, user-facing strings)

**Files:**
- Modify: `crates/iss/src/main.rs` (`JJF_ACTOR` → `ISS_ACTOR`, help/version/usage strings)
- Modify: cache-path code in `crates/iss-storage/src/cache.rs` (`.jj/jjforge-cache.json` → `.git/iss-cache.json`)
- Modify: error-message and help strings across `crates/iss*/src`

**Interfaces:**
- Produces: env var `ISS_ACTOR`; cache file at `.git/iss-cache.json`.

- [ ] **Step 1: Write/adjust a test pinning the new env var.** In `crates/iss/tests/actor.rs`, add (or rename the existing JJF_ACTOR test):

```rust
#[test]
fn claim_uses_iss_actor_env() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::process::Command::new("git").arg("init").arg(root).output().unwrap();
    iss_cmd(root, &["init"]);
    let id = create_issue(root, "env claim");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_iss"))
        .current_dir(root)
        .args(["update", &id, "--claim", "--json"])
        .env("ISS_ACTOR", "Fan Out 3")
        .output().unwrap();
    assert!(out.status.success());
    let show = iss_cmd(root, &["show", &id, "--json"]);
    assert!(String::from_utf8_lossy(&show.stdout).contains("Fan Out 3"));
}
```

- [ ] **Step 2: Run — expect FAIL** (env still read as `JJF_ACTOR`).

Run: `cargo nextest run -p iss claim_uses_iss_actor_env`
Expected: FAIL.

- [ ] **Step 3: Rename the env var.** In `main.rs`, change every `std::env::var("JJF_ACTOR")` / `remove_var("JJF_ACTOR")` and the doc-comments to `ISS_ACTOR`. Update the "no current user" error message to reference `ISS_ACTOR` and `git config user.name`.

- [ ] **Step 4: Relocate the cache file.** In `cache.rs`, change `cache_path` to return `<repo>/.git/iss-cache.json` (was `.jj/jjforge-cache.json`). Use the git-dir (`.git`), not `.jj`.

- [ ] **Step 5: Sweep user-facing strings.** Replace `jjf`/`jjforge` in help text, `--version` banner, usage output, and error messages with `iss`/`git-issues`. Do NOT touch `refs/jjf/*` or `Jjf-*` literals.

Run: `grep -rn 'jjf\|jjforge' crates/iss*/src --include='*.rs' | grep -iv 'refs/jjf\|Jjf-\|ISSUES_BOOKMARK\|_REVSET'`
Address each hit (user-facing string). The filtered-out lines are the allowed wire tokens.

- [ ] **Step 6: Run tests.**

Run: `cargo nextest run --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/iss/src/main.rs crates/iss-storage/src/cache.rs crates/iss/tests/actor.rs
git commit -F - <<'EOF'
r: rename JJF_ACTOR->ISS_ACTOR, cache to .git/iss-cache.json, strings

In-code identifiers and user-facing copy now say iss/git-issues. Cache
moved off .jj/ to .git/. Wire tokens (refs/jjf/*, Jjf-*) untouched.

Claude-Session: https://claude.ai/code/session_01VGhMvpYpR2YkT6inGLXoUT
EOF
```

---

### Task R3: Rename docs, the shim, and the skill

**Files:**
- Modify: `README.md`, `CLAUDE.md`, all of `docs/`, `blog/` references
- Rename: `bin/jjf` → `bin/iss`
- Rename: the `subagent-working-a-jjforge-issue` skill dir + update its trigger keywords
- Fix: the dangling `docs/storage-format.md` reference in `CLAUDE.md`

- [ ] **Step 1: Rename the shim.**

```bash
git mv bin/jjf bin/iss
```

Update its internals: it prefers `target/release/iss`, falls back to `target/debug/iss`, builds release on demand (was `jjf`).

- [ ] **Step 2: Rename the skill and update triggers.** `git mv` the skill directory `subagent-working-a-jjforge-issue` → `subagent-working-a-git-issues-issue` (or similar), and in its frontmatter/description change trigger keywords `jjf`/`jjforge` → `iss`/`git-issues`.

- [ ] **Step 3: Sweep docs.** Replace `jjf`→`iss` and `jjforge`→`git-issues` across `README.md`, `CLAUDE.md`, `docs/**`, `blog/**`. In `CLAUDE.md`, repoint the broken `docs/storage-format.md` reference to `docs/architecture.md` + `docs/storage-out-of-tree.md` (the real storage spec). Leave `refs/jjf/*` / `Jjf-*` mentions where they describe the wire format, but add a one-line note in `docs/architecture.md` that these are vestigial (named before the rename).

Run: `grep -rln 'jjforge\|\bjjf\b' README.md CLAUDE.md docs blog`
Address each.

- [ ] **Step 4: Verify the binary still runs via the shim.**

Run: `cargo build --release && ./bin/iss --version`
Expected: prints the version banner naming `iss` / `git-issues`.

- [ ] **Step 5: Commit**

```bash
git add README.md CLAUDE.md docs blog bin .claude
git commit -F - <<'EOF'
r: rename docs, bin/iss shim, and the subagent skill

Docs and shim say iss/git-issues. Fixed the dangling
docs/storage-format.md reference (real spec: architecture.md +
storage-out-of-tree.md). Noted refs/jjf/* / Jjf-* as vestigial.

Claude-Session: https://claude.ai/code/session_01VGhMvpYpR2YkT6inGLXoUT
EOF
```

---

### Task R4: Final verification sweep + operator-action note

**Files:**
- Create: `docs/handoffs/2026-06-30-git-issues-operator-actions.md` (the manual steps)

- [ ] **Step 1: Run the verification grep.** Confirm only intentional wire tokens remain:

Run: `grep -rn 'jjf\|jjforge\|JjRepo\|JJF_' crates --include='*.rs' | grep -iv 'refs/jjf\|Jjf-\|ISSUES_BOOKMARK\|_REVSET'`
Expected: **empty.** Any hit outside the allowed wire tokens is a missed rename — fix it, then re-run.

- [ ] **Step 2: Confirm no jj anywhere in source.**

Run: `grep -rn 'Command::new("jj")\|"jj"\|jj git\|jj bookmark' crates --include='*.rs' | grep -v '/tests/'`
Expected: **empty** (tests may reference jj only to assert its absence).

- [ ] **Step 3: Full clean build + test.**

Run: `cargo build --workspace && cargo nextest run --workspace`
Expected: PASS.

- [ ] **Step 4: Write the operator-actions note** at `docs/handoffs/2026-06-30-git-issues-operator-actions.md`:

```markdown
# git-issues — operator actions (run manually)

The rename is code-complete. These touch your filesystem / Forgejo and
are NOT automated. The code works regardless of the directory name.

1. Rename the working directory:
   mv ~/p/jjforge ~/p/git-issues
2. Rename the Forgejo repo jjforge -> git-issues in the web UI, then:
   cd ~/p/git-issues
   git remote set-url origin git@github.com:myers/git-issues.git
3. (Deferred, separate ticket) Wire-format rename: refs/jjf/* ->
   refs/iss/*, Jjf-* -> Iss-* via a vN->vN+1 migrator across all live
   host repos. Not part of this change.
```

- [ ] **Step 5: Commit**

```bash
git add docs/handoffs/2026-06-30-git-issues-operator-actions.md
git commit -F - <<'EOF'
r: final verification sweep + operator-actions note

Grep confirms only vestigial wire tokens (refs/jjf/*, Jjf-*) remain;
no jj subprocess anywhere in source. Operator dir/remote rename steps
documented for manual execution.

Claude-Session: https://claude.ai/code/session_01VGhMvpYpR2YkT6inGLXoUT
EOF
```

---

## Self-Review

**Spec coverage:**
- Workstream J (J1-J5 in spec) → Tasks J1-J7. J1 spec (delete v2-legacy) → Tasks J4+J5. J2 (reroute wrappers) → J1+J2. J3 (delete JjRepo) → J6. J4 (strip guard) → J6. J5 (verify) → J7. ✓
- Workstream R (R1-R4 in spec) → Tasks R1-R4. Cargo/crates → R1. Identifiers/env/cache → R2. Docs/shim/skill + dangling-ref fix → R3. Operator actions (not auto-run) → R4 note. ✓
- Wire-format preservation (Global Constraint) → enforced by the grep gates in R2-Step5, R4-Step1. ✓
- jj-absent acceptance → J7. ✓

**Placeholder scan:** No "TBD"/"handle edge cases". The one runtime-discovery item (exact remote-list parsing) has the concrete `git remote -v` format documented. Test helper names flagged as "match existing" because the real names live in current test files the implementer will open. ✓

**Type consistency:** `git_config_get` (J2) replaces `jj_config_get` consistently; `GitError`/`GitRepo` (J6) replace `JjError`/`JjRepo`; `preflight::jj_repo` and `preflight::issues_bookmark` keep their signatures across J1/J3. `CARGO_BIN_EXE_jjf` → `_iss` rename handled in R1-Step3. ✓
