# jjforge

A jj-native, agent-first issue tracker. CLI: `jjf`.

**Status:** scoping. See the meta-epic `04e1dac` for the plan.

Inspirations and what we take from each:

- **[git-bug](https://github.com/git-bug/git-bug)** — DAG-of-operations, fast-forward
  push, content-hash IDs that survive concurrent edits, separation of identity
  from issue. Distributed semantics that work.
- **[Beads](https://github.com/steveyegge/beads)** (Steve Yegge) — `ready` as a
  first-class agent primitive, `remember` for persistent project memory, `--json`
  on every command, MCP-shaped, hierarchical issues, compaction-aware. Agents are
  the primary user.
- **[jj](https://github.com/jj-vcs/jj)** — stable change IDs that survive
  rewrites, operation log, automatic conflict resolution baked in. Don't
  reinvent what jj already does better than git.
- **zfs-workspace markdown-in-repo convention** — disciplines emerge from
  incidents, closure ties to a physical artifact, "no parser depends on the
  format."

## First-time setup

Stable Rust toolchain (1.75+ should be fine; project is currently
on whatever rustup gives you).

Install once:

```bash
# git-bug — the interim issue tracker (we'll replace it with jjf)
brew install git-bug

# jj — Jujutsu, the substrate we shell out to from jjf
brew install jj   # or: cargo install --git https://github.com/jj-vcs/jj.git --locked --bin jj

# nextest — preferred test runner; isolates test processes, which matters for
# the integration tests that spawn real `jj` subprocesses
cargo install cargo-nextest --locked
```

Run the workspace tests:

```bash
cargo nextest run --workspace
# fall back to `cargo test --workspace` if nextest isn't available
```

## Planning lives in git-bug

Plans, decisions, and work items live in `git-bug` issues in this repo,
not in markdown files. The meta-epic `04e1dac` is the entry point;
read it first.

Quick reference:

```
git-bug bug                            # list everything
git-bug bug --label roadmap            # the priority order — read this first
git-bug bug --label meta-epic          # the index of every issue
git-bug bug --label epic               # the six epics
git-bug bug --label epic:mvp-storage   # one epic + its related issues
git-bug bug --label research           # historical research record
git-bug bug show <id>                  # one issue
git-bug bug new -t "<title>" -F -      # new, body on stdin
git-bug bug comment new <id> -F -      # comment, body on stdin
```

Label scheme:

- `roadmap` — the project's running priority list (one ticket, never
  closes; latest comment is the truth).
- `meta-epic` — the index of every issue, by label.
- `epic` — the six top-level epic issues.
- `epic:<slug>` — every issue belonging to an epic (the epic itself
  plus its research and child tickets).
- `research` — historical research issues.

## Why git-bug for planning

We're using git-bug to plan its jj-native successor. If the
experience is rough, that's exactly the input we need: the friction
shows up in detail, and jjforge can specifically improve on it.

The CLI shim at `bin/jjf` already wraps git-bug, so the verb shape
of the eventual Rust binary stays consistent through the planning
phase.
