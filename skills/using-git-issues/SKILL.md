---
name: using-git-issues
description: Use when working in a project that uses git-issues (the `iss` CLI) as its issue tracker — reading the roadmap, creating or updating issues, finding the next unblocked work, or scripting against git-issues JSON. Triggers on "iss", "git-issues", "ready", "remember", or any `iss <verb>` invocation.
---

# Using git-issues

git-issues is a jj-native, agent-first issue tracker. The CLI is `iss`.
Every verb takes `--json` for scripting. There are no interactive
prompts and no editor launches — bodies come from `-F <path>` or
`-F -` (stdin).

## Entry point

In a new session, read the roadmap first:

```bash
iss show roadmap --include-memories
```

The roadmap is one issue (slug `roadmap`) that the project's
maintainer keeps current. It names open epics in priority order with
an above-the-line / below-the-line cut for what's shipping now vs.
queued. `--include-memories` appends every `iss remember` entry —
short declarative facts that travel with the planner. Read them so
you don't re-derive what an earlier session learned.

## Finding the next thing to work on

```bash
iss ready                         # everything unblocked
iss ready --limit 1               # just the next one
iss ready --json --limit 1        # machine-readable
iss ready --type bug              # bugs first
iss ready --label backend         # intersect with a label
```

`iss ready` returns issues whose dependencies are all closed,
excludes the roadmap, and sorts by type priority (bug > feature >
research > epic > unspecified), then FIFO by `created_at`.

## Issue handles: ids and slugs

git-issues issue ids are **7-char lowercase hex** (e.g. `c6aed85`).
Slugs are kebab-case names (e.g. `proptest-multi-issue-generator`)
set with `--slug` on create. **Use slugs whenever possible** — they
read in dispatch messages, commits, and comments; ids don't.

Most verbs accept either: `iss show c6aed85` and `iss show
proptest-multi-issue-generator` both work. Partial-prefix lookup is
NOT supported — pass the full 7 chars.

## Creating an issue

`-F -` reads body from stdin (no editor). Labels with `-l`,
dependencies with `-d`, `--parent <id>` for a parent-child edge to
an epic, `--type` and `--slug` recommended on every new ticket.

```bash
cat <<'EOF' | iss new --json -t "Real title" --type feature \
    --slug add-new-feature --parent foo -F -
# Goal

What done looks like.

# Approach

How we get there.
EOF
```

Stdout: `{"ok":true,"id":"a3f9c01"}`. Capture with
`jq -r .id` to script around it.

## Updating an issue

```bash
iss update <handle> --title "..."        # rename
iss update <handle> --status closed      # change status
iss update <handle> --body-file body.md  # rewrite body in place
iss update <handle> --assignee alice     # assign
iss assign <handle> alice                # shorthand
iss close <handle>                       # convenience for closed
iss open <handle>                        # convenience for open
iss block <handle> --reason "<why>"      # park; excluded from ready
iss unblock <handle>                     # unpark
iss label add <handle> <label>           # add a label
iss label rm  <handle> <label>           # remove
cat body.md | iss comment <handle> -F -  # append a comment
```

Every mutating verb takes `--json` and emits an `{"ok": true, ...}`
envelope.

## Dependencies

```bash
iss dep add  <child> <parent> --kind blocks         # default
iss dep add  <child> <parent> --kind parent-child   # epic child
iss dep tree <handle>                                # walk children
```

Four edge kinds: `blocks`, `parent-child`, `related`,
`discovered-from`. On `iss new`: `--parent <id>` is shorthand for
`-d parent-child:<id>`. `-d <id>` defaults to `blocks`.

## Persistent memory (project-scoped)

```bash
iss remember "<insight>"                  # write; key auto-slugged
iss remember "<insight>" --key <slug>     # write with explicit key
iss memories                              # list all
iss memories <substring>                  # filter
iss recall <key>                          # read one
iss forget <key>                          # remove
```

Save a memory when you've learned something the next session would
otherwise re-derive (a non-obvious workflow rule, a codebase
gotcha, a constraint not visible from the code alone). Memories
ride the planner via `iss push` / `pull`.

## Common queries

```bash
iss ls --type bug                                  # all bugs
iss ls --parent foo --status open                  # work under epic
iss ls --status all                                # everything
iss ls --json --type epic | jq '.[] | .id'         # script-friendly
iss show <handle>                                  # one issue + comments
iss search "needle"                                # titles + bodies
iss search "needle" --include-comments             # plus comments
iss stale --days 14                                # untouched recently
```

## Push / pull

`iss push <remote>` and `iss pull <remote>` round-trip the planner
via standard git transport. `iss remote add|ls|rm` manages remotes.
The planner rides alongside code; one push per closed issue is the
norm so the remote tracks orchestration cadence.

## Subagent work on a single issue

When dispatched to do focused work on one issue, the
[[subagent-working-a-git-issues-issue]] skill carries the contract:
the four-section closing-comment recipe (Findings / Recommendation /
Confidence / Open follow-ups), the boundaries (don't edit the body,
don't push, don't close other issues), and the actor-attribution
rules for parallel dispatch.

## Common mistakes

| Mistake | Fix |
|---|---|
| Used a 7-char id in a dispatch message or commit when the issue has a slug | Use the slug; both at once is fine when grep-anchoring matters |
| `iss new --dep <epic>` when meaning "child of epic" | Use `--parent <epic>` — `-d` defaults to `blocks`, which prevents the new issue from ever appearing in `iss ready` |
| Used a label to attach a child to an epic | The label-based epic convention was retired. Use `--parent <epic>` on `iss new`, or `iss dep add --kind parent-child <child> <epic>` after the fact. Filter with `iss ls --parent <epic>`. |
| Looked for an editor to pop up | Bodies are `-F <path>` or `-F -`. There are no prompts |
| Tried partial-id lookup like `iss show a3f` | Pass the full 7-char id, or use the slug |
| Closed an issue with `--status closed` instead of `iss close` | Both work; `iss close <id>` is shorter and reads better in scripts |
