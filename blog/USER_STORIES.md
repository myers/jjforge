# User stories — jjforge

Why this file exists: a jjforge blog post explains a piece of
building the tool, and the reader is welcome to that explanation,
but the reader is *not* showing up to read about an issue. They're
showing up because at some point they want to use a coding agent and
keep track of work. Posts work when they connect what we built back
to a thing the reader can imagine *doing* — ideally a `jjf` command
they could type, or an agent-loop they could orchestrate.

This file enumerates the stories. Every post should serve at least
one. The reviewer agent checks coverage. Authors can frame their
post-plan around the canonical id (`STORY-04 — pick up ready work`)
so the writing is tethered to a reader-visible outcome, not a
project-internal milestone.

Stories are written as **a user wants to**: short, concrete, with
the command (or shape of command) the user would actually run when
they're doing this. The command isn't the story; it's the surface
where the story is felt.

## Format

Each entry has:

- An id (`STORY-NN`) for cross-referencing.
- A one-line user goal.
- The surface — the `jjf` command(s) the user would run, or the
  agent flow they'd kick off.
- A status: `research` (the foundational decisions before code
  ships), `mvp` (the minimum tool that does the thing), `agents`
  (the agent-facing ergonomics), `multi-client` (when more than
  one front-end matters), `distributed` (cross-machine sync /
  conflict resolution).

Stories may belong to multiple statuses. The status reflects when
the story first becomes *available* in jjforge, not when it's
polished.

---

## Foundations

### STORY-00 — know which technology stack jjforge is built on

A user (or contributor) wants to understand the early decisions —
why Rust, why shelling out to `jj`, why a bookmark branch for
storage — without having to read the whole codebase or guess from
the project name.

**Surface:** the README, the early devlog posts.

**Status:** research.

---

## Filing and reading

### STORY-01 — file an issue for a project I'm working on

A user has a thought ("I need to refactor the session manager") and
wants to put it on the project so they can pick it up later.

**Surface:** `jjf new -t "<title>" -F -` (body on stdin).

**Status:** mvp.

### STORY-02 — list what's open

A user opens a project and wants to see what's outstanding.

**Surface:** `jjf ls`, `jjf ls --status open`.

**Status:** mvp.

### STORY-03 — read one issue in detail

A user has an issue id and wants the full body and comments.

**Surface:** `jjf show <id>`.

**Status:** mvp.

---

## Working an issue

### STORY-04 — pick up the next ready piece of work

A user (or an agent) opens the project and wants to know what's
unblocked and ready. They don't want to manually scan the open
list and trace dependencies.

**Surface:** `jjf ready`, optionally `jjf ready --claim`.

**Status:** agents.

### STORY-05 — capture findings back on the issue

A user (or agent) has done the work for an issue and wants to
record the outcome so the next session can pick up cleanly.

**Surface:** `jjf comment <id> -F -`, then `jjf close <id>`.

**Status:** mvp.

### STORY-06 — record a project-wide note that's not tied to an issue

A user (or agent) has a piece of context that should persist across
sessions but doesn't belong to a single issue — "we picked X over
Y," "the build needs the experimental rustc flag," etc.

**Surface:** `jjf remember "<note>"`.

**Status:** agents.

---

## Multi-agent orchestration

### STORY-07 — dispatch a subagent to work an issue without micromanaging

A user wants to launch a coding agent on a specific issue and trust
that the agent will produce a structured outcome the user can read,
not just a dump.

**Surface:** dispatch from the parent agent; the
`subagent-working-a-git-bug-issue` skill (or its jjforge successor)
enforces the closure recipe.

**Status:** agents.

### STORY-08 — see what every agent on the project is currently doing

A user opens the project and wants a live view of which agents are
working which issues, and what they've produced so far.

**Surface:** the PWA / desktop view (out of scope for v1; informs
data model).

**Status:** multi-client.

---

## Distributed

### STORY-09 — work on a project from two machines without losing edits

A user edits an issue on their laptop, edits the same issue from
the desktop while offline, then syncs. Both edits land somewhere
sensible.

**Surface:** `jjf push`, `jjf pull`, and the merge driver from
`e2e473b`.

**Status:** distributed.

### STORY-10 — back up a project

A user wants a full backup of project state — code, issues, history.

**Surface:** `git push <remote> --mirror` to a backup destination.

**Status:** distributed (works the moment refs/bookmarks sync
cleanly).

---

## Voice and mobile (later)

### STORY-11 — file an issue from the phone with voice input

A user has an idea while away from the laptop and wants to dictate
it into a session.

**Surface:** the PWA, on-device Whisper or equivalent, POSTs a new
issue to the running daemon.

**Status:** multi-client.

### STORY-12 — check the ecosystem view from the phone

A user wants to look at what's running on the laptop from their
phone — which agents are working what, latest outputs.

**Surface:** PWA dashboard, read-only at first.

**Status:** multi-client.

---

Stories that don't yet have an id are fine. As we build, add new
ones rather than retrofitting an existing entry to match. The
canonical ids are referenced from the planning siblings, so
renaming them breaks history.
