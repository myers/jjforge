---
name: using-jjforge
description: Use when working in a project that uses jjforge (the `jjf` CLI) as its issue tracker — reading the roadmap, creating or updating issues, finding the next unblocked work, or scripting against jjforge JSON. Triggers on "jjf", "jjforge", "ready", "remember", or any `jjf <verb>` invocation.
---

# Using jjforge

jjforge is a jj-native, agent-first issue tracker. The CLI is `jjf`.
Every verb takes `--json` for scripting. There are no interactive
prompts and no editor launches — bodies come from `-F <path>` or
`-F -` (stdin).

## Entry point

In a new session, read the roadmap first:

```bash
jjf show roadmap --include-memories
```

The roadmap is one issue (slug `roadmap`) that the project's
maintainer keeps current. It names open epics in priority order with
an above-the-line / below-the-line cut for what's shipping now vs.
queued. `--include-memories` appends every `jjf remember` entry —
short declarative facts that travel with the planner. Read them so
you don't re-derive what an earlier session learned.

## Finding the next thing to work on

```bash
jjf ready                         # everything unblocked
jjf ready --limit 1               # just the next one
jjf ready --json --limit 1        # machine-readable
jjf ready --type bug              # bugs first
jjf ready --label backend         # intersect with a label
```

`jjf ready` returns issues whose dependencies are all closed,
excludes the roadmap, and sorts by type priority (bug > feature >
research > epic > unspecified), then FIFO by `created_at`.

## Issue handles: ids and slugs

jjforge issue ids are **7-char lowercase hex** (e.g. `c6aed85`).
Slugs are kebab-case names (e.g. `proptest-multi-issue-generator`)
set with `--slug` on create. **Use slugs whenever possible** — they
read in dispatch messages, commits, and comments; ids don't.

Most verbs accept either: `jjf show c6aed85` and `jjf show
proptest-multi-issue-generator` both work. Partial-prefix lookup is
NOT supported — pass the full 7 chars.

## Creating an issue

`-F -` reads body from stdin (no editor). Labels with `-l`,
dependencies with `-d`, `--parent <id>` for a parent-child edge to
an epic, `--type` and `--slug` recommended on every new ticket.

```bash
cat <<'EOF' | jjf new --json -t "Real title" --type feature \
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
jjf update <handle> --title "..."        # rename
jjf update <handle> --status closed      # change status
jjf update <handle> --body-file body.md  # rewrite body in place
jjf update <handle> --assignee alice     # assign
jjf assign <handle> alice                # shorthand
jjf close <handle>                       # convenience for closed
jjf open <handle>                        # convenience for open
jjf block <handle> --reason "<why>"      # park; excluded from ready
jjf unblock <handle>                     # unpark
jjf label add <handle> <label>           # add a label
jjf label rm  <handle> <label>           # remove
cat body.md | jjf comment <handle> -F -  # append a comment
```

Every mutating verb takes `--json` and emits an `{"ok": true, ...}`
envelope.

## Dependencies

```bash
jjf dep add  <child> <parent> --kind blocks         # default
jjf dep add  <child> <parent> --kind parent-child   # epic child
jjf dep tree <handle>                                # walk children
```

Four edge kinds: `blocks`, `parent-child`, `related`,
`discovered-from`. On `jjf new`: `--parent <id>` is shorthand for
`-d parent-child:<id>`. `-d <id>` defaults to `blocks`.

## Persistent memory (project-scoped)

```bash
jjf remember "<insight>"                  # write; key auto-slugged
jjf remember "<insight>" --key <slug>     # write with explicit key
jjf memories                              # list all
jjf memories <substring>                  # filter
jjf recall <key>                          # read one
jjf forget <key>                          # remove
```

Save a memory when you've learned something the next session would
otherwise re-derive (a non-obvious workflow rule, a codebase
gotcha, a constraint not visible from the code alone). Memories
ride the planner via `jjf push` / `pull`.

## Common queries

```bash
jjf ls --type bug                                  # all bugs
jjf ls --parent foo --status open                  # work under epic
jjf ls --status all                                # everything
jjf ls --json --type epic | jq '.[] | .id'         # script-friendly
jjf show <handle>                                  # one issue + comments
jjf search "needle"                                # titles + bodies
jjf search "needle" --include-comments             # plus comments
jjf stale --days 14                                # untouched recently
```

## Push / pull

`jjf push <remote>` and `jjf pull <remote>` round-trip the planner
via standard git transport. `jjf remote add|ls|rm` manages remotes.
The planner rides alongside code; one push per closed issue is the
norm so the remote tracks orchestration cadence.

## Subagent work on a single issue

When dispatched to do focused work on one issue, the
[[subagent-working-a-jjforge-issue]] skill carries the contract:
the four-section closing-comment recipe (Findings / Recommendation /
Confidence / Open follow-ups), the boundaries (don't edit the body,
don't push, don't close other issues), and the actor-attribution
rules for parallel dispatch.

## Common mistakes

| Mistake | Fix |
|---|---|
| Used a 7-char id in a dispatch message or commit when the issue has a slug | Use the slug; both at once is fine when grep-anchoring matters |
| `jjf new --dep <epic>` when meaning "child of epic" | Use `--parent <epic>` — `-d` defaults to `blocks`, which prevents the new issue from ever appearing in `jjf ready` |
| Used a label to attach a child to an epic | The label-based epic convention was retired. Use `--parent <epic>` on `jjf new`, or `jjf dep add --kind parent-child <child> <epic>` after the fact. Filter with `jjf ls --parent <epic>`. |
| Looked for an editor to pop up | Bodies are `-F <path>` or `-F -`. There are no prompts |
| Tried partial-id lookup like `jjf show a3f` | Pass the full 7-char id, or use the slug |
| Closed an issue with `--status closed` instead of `jjf close` | Both work; `jjf close <id>` is shorter and reads better in scripts |
