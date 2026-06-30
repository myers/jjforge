# git-issues

> **Alpha** — written by a coding agent, has been only lightly used so far. YOU HAVE BEEN WARNED.

## Quickstart

You'll need a recent Rust toolchain and [jj](https://github.com/jj-vcs/jj)
on your PATH (git-issues shells out to `jj`).

```bash
cargo install --git https://github.com/myers/jjforge.git iss
cd /your/jj/repo
iss init                              # plant refs/jjf/* namespace
echo "the body" | iss new -t "first issue" -F -
iss ls
```

For the full walk-through (push/pull across clones, dep edges,
the agent-ergonomics verbs like `ready` and `remember`), see
[docs/quickstart.md](docs/quickstart.md). Every verb takes
`--json` and emits the envelope shape documented in
[docs/cli-json.md](docs/cli-json.md).

## What this is

A jj-native, agent-first issue tracker. CLI: `iss`.

The Rust binary covers the full verb set with `--json` on every
command, push/pull over standard git transport, and an op-space
merge driver so two clones can edit issues offline and converge
without human intervention. Output contract in
[docs/cli-json.md](docs/cli-json.md); architecture in
[docs/architecture.md](docs/architecture.md).

It tracks its own work — see the roadmap (`iss show roadmap`)
for what's open and what's next.

## Inspirations

- **[git-bug](https://github.com/git-bug/git-bug)** —
  DAG-of-operations, fast-forward push, content-hash IDs that
  survive concurrent edits, separation of identity from issue.
  Distributed semantics that work.
- **[Beads](https://github.com/steveyegge/beads)** (Steve
  Yegge) — `ready` as a first-class agent primitive,
  `remember` for persistent project memory, `--json` on every
  command, hierarchical issues, compaction-aware. Agents are
  the primary user.
- **[jj](https://github.com/jj-vcs/jj)** — stable change IDs
  that survive rewrites, operation log, automatic conflict
  resolution baked in. Don't reinvent what jj already does
  better than git.

## Architecture

Issues live in a `refs/jjf/*` ref namespace; every mutation is a
git commit; sync rides standard git transport. The merge driver is
deterministic — no human-resolvable conflicts. See
[docs/architecture.md](docs/architecture.md) for the full picture
and the rationale for leaning on jj rather than git alone.

## Planning lives in git-issues

Self-hosted on this very binary. The roadmap is the entry
point in any new session:

```bash
iss show roadmap
```

## Agent skills

git-issues ships skills for agents driving it (Claude Code, Codex,
Cursor, Gemini, Copilot CLI, and others that read the SKILL.md
convention).

Install with Vercel's `skills` CLI:

```bash
npx skills add myers/git-issues
```

The CLI auto-detects which agents you have installed and asks
which to target. The two skills that ship today:

- **`using-git-issues`** — how to read the roadmap, find unblocked
  work, create / update / query issues. Auto-loads on `iss`,
  `git-issues`, `ready`, `remember`, or any `iss <verb>` invocation.
- **`subagent-working-a-git-issues-issue`** — the closing-comment
  recipe and boundaries for a subagent dispatched to do focused
  work on a single issue.

Claude Code users can also install via the plugin marketplace
shape — this repo's `.claude-plugin/plugin.json` makes the same
`skills/` tree loadable as a plugin.

## Repo layout

- [`crates/iss-storage/`](crates/iss-storage/) — storage
  primitives (init / read / write / history / list / merge).
- [`crates/iss-merge/`](crates/iss-merge/) — file-bytes merge
  driver, kept as a library.
- [`crates/iss/`](crates/iss/) — the `iss` binary (clap-derive
  CLI over the storage layer).
- [`skills/`](skills/) — agent skills, one SKILL.md per skill
  (Vercel + Claude Code compatible).
- [`docs/quickstart.md`](docs/quickstart.md) — five-minute
  walkthrough.
- [`docs/architecture.md`](docs/architecture.md) — storage
  layout, write path, merge model, why jj-native.
- [`docs/cli-json.md`](docs/cli-json.md) — CLI output contract.

## Building from source

Stable Rust toolchain (1.75+); `jj` on your PATH (Quickstart
covers this).

```bash
cargo build --release -p iss
cargo nextest run --workspace
# fall back to `cargo test --workspace` if nextest isn't installed
```

The binary lands at `target/release/iss`. `cargo nextest` is
preferred — it isolates test processes, which matters for the
integration tests that spawn real `jj` subprocesses (install:
`cargo install cargo-nextest --locked`).

## License

TBD.
