# jjforge

A jj-native, agent-first issue tracker. CLI: `jjf`.

**Status:** scoping. This README will grow as decisions land.

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

## Planning

Planning happens in `git-bug` issues in this repo, not in markdown files. Use
`git-bug` to list, show, and update issues:

```
git-bug bug                  # list
git-bug bug show <prefix>    # one
git-bug bug add ...          # new
git-bug bug comment <id> ... # discuss
```

The first issues are the operational unknowns we need to research before we
can credibly write a v1 of the tool itself. See those for the current shape of
the work.

## Why git-bug for planning

This is deliberate eat-our-own-dogfood: we will use git-bug to plan a
jj-native tracker. If the experience is rough, we'll have learned exactly
where the friction is, which is the input we need.
