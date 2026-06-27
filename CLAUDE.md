# jjforge — Claude operating notes

A jj-native, agent-first issue tracker. CLI: `jjf`. The project's
own README is at `README.md`; what follows are the operating
conventions Claude (and future subagents) need to know when
working in this repo.

The standing rules from `~/p/rust-coding-agent-harness/CLAUDE.md`
also apply (they live one level up the directory tree but we're
not nested under that workspace; they apply by convention because
this is part of the same effort). The most important one to
re-read: **commit when a coherent unit of work is done; don't ask
first.**

## Project shape today

- **Status:** post-MVP. The Rust binary at `crates/jjf/` covers
  the full verb set (`init`, `new`, `show`, `ls`, `ready`,
  `update`, `comment`, `close`/`open`, `block`/`unblock`,
  `abandon`, `label add|rm`, `dep add|rm|tree`,
  `remote add|ls|rm`, `push`, `pull`, `remember`/`memories`/
  `recall`/`forget`) with `--json` on every verb. Storage spec
  pinned in `docs/storage-format.md`; CLI output contract in
  `docs/cli-json.md`.
- **Planning surface:** `jjforge` itself, on the `issues` bookmark
  in this repo. As of 2026-06-22 the project's own planning runs
  on `jjf`. Pre-cutover history lives in archived git-bug refs
  (`refs/bugs/*`); the bridge is `docs/git-bug-cutover.md`. (The
  v1 → v2 rename in `docs/storage-format.md` moved the live planner
  data from `bugs` to `issues` automatically — the storage layer
  detects v1-shape repos and migrates on first `Storage::open`.)
- **Entry point:** the roadmap. Read it first via
  `jjf show roadmap` (the issue's slug is `roadmap` and its
  type is `roadmap` — both let you skip the 7-char id).
- **CI:** `.woodpecker/blog.yaml` builds and pushes a Zola site
  image. Mirrors zfs-workspace's pattern except for the
  notify-flux hook (jjforge isn't a Flux deployment target).

## How to use jjforge

The project is dogfooding `jjf` as its own planner. Treat every
rough edge as data, not as something to fix around.

### Entry point and discovery

The roadmap is the orientation document. Start there in any
new session before touching anything else; surface persistent
memories at the same time:

```bash
jjf show roadmap --include-memories
```

(The roadmap issue has slug `roadmap` and type `roadmap`;
either resolves it. Its 7-char id is `9566f52` if you ever
need it for archival cross-references.)

The `--include-memories` flag appends a `## Persistent
Memories` block listing every `jjf remember` entry on the
bookmark. These are short declarative facts that travel with
the planner via `jjf push`/`pull` (operational rules,
codebase folklore, architectural decisions). Read them; they
exist so you don't have to re-derive what an earlier session
already learned.

Manage memories with:

```bash
jjf remember "<insight>"                 # write; key auto-slugged
jjf remember "<insight>" --key <slug>    # write with explicit key
jjf memories                             # list all
jjf memories <substring>                 # filter
jjf recall <key>                         # read one
jjf forget <key>                         # remove
```

Save a memory when you've learned something the next session
would otherwise re-derive (a non-obvious workflow rule, a
gotcha in the codebase, a constraint that's not visible from
the code alone). Memories are project-scoped; do not save
operator preferences here — those go in `~/.claude/projects/`.

**Memories don't auto-decay.** When an invariant they
describe gets refactored away (e.g. an env var retired, a
workflow rule lifted, a file path moved), `jjf forget <key>`
is the right move. Skim the memory list when you finish a
session that touched anything load-bearing; a stale memory
is worse than no memory.

To navigate from the roadmap, see the Queries section at the
bottom of this file.

### Label scheme

- **`roadmap`** — the running plan (one ticket, never closes;
  body edited in place via `jjf update --body-file`).
- **`epic`** — the top-level epic issues. Each carries the goal,
  the sketched approach, the tickets we expect to file under it,
  and its dependency graph.
- **`epic:<slug>`** — every issue belonging to an epic (the epic
  itself plus child tickets when they're filed). Use the
  colon-prefixed form always.

### Epics vs child issues

Epics describe a **goal**: what "done" looks like, the approach
sketch, dependency edges to other epics. Child tickets are
**how we get there** — each one a unit of work.

Keep the two layers separate. The epic body must NOT enumerate
its children or absorb their findings: no "Child tickets:
`abc1234` — …" lists, no "Post-X sweep" subsections naming the
shipped tickets. The child inventory is discovered via
`jjf ls --label epic:<slug> --status all`; the epic body is
stable across the life of the epic.

When closing an epic, the body's status section gets a "Done
as of YYYY-MM-DD" stamp and at most a one-line summary. Per-
ticket details belong in the child commits, their closing
comments, and roadmap status comments. Surface a child's
verdict to the epic via a status comment, not a body edit.

### Creating a new issue

Use `-F -` to read the body from stdin (the recommended pattern
— no interactive flow, no editor invocation, scripts cleanly).
Pass labels with `-l` (repeatable). Optional flags: `-d <id>`
(repeatable, dependencies), `-a <name>` (assignee), `--type
<kind>` (one of `bug` / `feature` / `epic` / `research` /
`roadmap` — v2.1), `--slug <kebab>` (kebab-case orientation
handle — v2.1, validated, unique across open issues). Use
`--json` for the machine-readable envelope.

**Recommended:** set `--type` and `--slug` on every new ticket.
The slug lets you say `jjf show agent-ready` instead of `jjf
show 69d5e1b`, and the type drives `jjf ls --type bug` /
`jjf ready`'s priority sort.

```bash
cat <<'EOF' | jjf new --json -t "Real title goes here" -F - -l epic -l epic:mvp-cli
# Goal

What does done look like.

# Approach

How we get there.
EOF
```

Stdout under `--json`:

```json
{"ok":true,"id":"a3f9c01"}
```

No gotchas — `-t` and `-F -` compose cleanly. (Contrast: the
pre-cutover `git-bug bug new --title X --file -` silently
dropped `--title` and took the body's first line. Documented
here because it bit prior sessions; see archived
`refs/bugs/*` for the war story.)

Capture the new id from the JSON envelope:

```bash
NEW_ID=$(jjf new --json -t "..." -F body.txt -l epic | jq -r .id)
```

### Updating issues

```bash
jjf update <id> --title "New title"          # rename
jjf update <id> --status closed              # change status
jjf update <id> --body-file body.md          # rewrite body in place
jjf update <id> --assignee alice             # assign
jjf update <id> --unset-assignee             # unassign

jjf close <id>                               # convenience for status closed
jjf open <id>                                # convenience for status open

jjf label add <id> <label>                   # add a label
jjf label rm <id> <label>                    # remove a label

cat body.md | jjf comment <id> -F -          # append a comment
jjf comment <id> -F body.md                  # ... from a file
```

Every mutating verb takes `--json` and emits the
`{"ok": true, ...}` envelope shape documented in
`docs/cli-json.md`. Multiple `--title`/`--status`/`--body-file`/
`--assignee` flags on the same `update` call land as a single
multi-op commit (one trailer per field).

### Bodies are editable now

Unlike pre-cutover git-bug, **jjforge supports editing an issue's
body in place** via `jjf update <id> --body-file <path>`. This
is the right way to revise a roadmap, fix an epic's plan, or
restate scope. Comments are still useful for status updates and
mid-stream findings — but the body can be made authoritative
without an append-only comment trail.

The pre-cutover roadmap convention ("latest comment is the
truth, body is stale") was a workaround for git-bug's missing
edit-body command. We can stop doing that. When the priority
order shifts, edit the roadmap body; a single `jjf comment`
on the roadmap announcing the change is courtesy, not contract.

### Issue-id length

jjforge ids are **7-character lowercase hex** by design (28 bits,
generated at create time per `docs/storage-format.md` §2). The
displayed id IS the full id — there is no prefix convention to
worry about, no SHA stem to lengthen.

CLI verbs do NOT accept partial-id prefixes; pass the full
7-char id or a slug. A 7-char hex handle that doesn't match
any issue surfaces as `issue_not_found` (exit 1, runtime). A
non-hex handle with no matching slug surfaces as
`slug_not_found` (exit 2, preflight). Slugs are the
human-friendly short form — use `--slug <kebab>` on
`jjf new` so future commands can say `jjf show agent-ready`
instead of `jjf show 69d5e1b`.

### Push / pull

`jjf push <remote>` and `jjf pull <remote>` round-trip the `issues`
bookmark via standard git transport. No special refspec config
needed — jj 0.40 carries the bookmark automatically (finding
verified in archived `refs/bugs/07780aa`). `jjf remote
add|ls|rm` wraps `jj git remote *` for managing remotes.

**The `origin` remote (`git@github.com:myers/jjforge.git`,
a Forgejo on the user's infrastructure) IS configured.** It's
the canonical home of the code; `main` tracks `origin/main`.
Push at the end of every issue (see Commits section). The
jjforge `issues` bookmark also rides this remote — `jjf push
origin` round-trips the planner data alongside.

### Reading historical (pre-cutover) git-bug data

The `bin/jjf` shim now delegates to the Rust binary (prefers
`target/release/jjf`, falls back to `target/debug/jjf`, builds
release on demand). To reach pre-2026-06-22 planner data on
`refs/bugs/*`, use `git-bug` directly:

```bash
git-bug bug show <old-7hex-id>      # one ticket
git-bug bug --label epic            # archived epics
git-bug bug --label research        # the research record
git-bug bug --status closed         # archived closed tickets
```

The archived data covers every status-update comment on each
pre-cutover epic, the five 2026-06-21 research tickets and
their full closing comments (the source of truth for the
storage and sync verdicts pinned in the current epic bodies),
and every closed child ticket (the workshop floor: subagent
finds, follow-ups, debate notes).

The cutover doc at `docs/git-bug-cutover.md` carries the
old → new id mapping and the historical-bug recipe.

**Never run `git-bug wipe`**. The archived data is the only
copy of pre-cutover history; once gone, it's gone.

## Subagent discipline

When dispatching a subagent to work an issue, the
**`subagent-working-a-jjforge-issue`** skill auto-loads on
keywords like "issue", "ticket", "jjforge", or "jjf". It enforces:

- The closing comment uses the four-section recipe: Findings,
  Recommendation, Confidence, Open follow-ups.
- The agent closes the issue when work is complete, marks it
  blocked with a reason, or leaves it open with the Findings
  explaining why.
- The agent does not edit the original body, does not touch other
  issues unless cross-link is warranted, and does not push.
- The closing return-value to the orchestrator is under 200
  words.

## User-facing prompts

When asking the user a multiple-choice question, **label options
A / B / C / D, never 1 / 2 / 3**. The Claude Code UI uses
digits on the same input line as the LLM conversation as
quality-survey responses; if the orchestrator offers options
"1)" / "2)" / "3)" and the user types "2", the digit may be
captured by the survey UI before it reaches the conversation,
silently desynchronizing the user's choice from what the
orchestrator thinks they picked. Letters route cleanly through
the conversation channel every time.

Example:
> Three options for the type field:
> - **A. Label by convention.** Cheap, no schema change…
> - **B. First-class `type` field.** Spec bump…
> - **C. Mandated label on create.** Forced discipline…
> Which path?

If you're dispatching subagents and the skill isn't loading,
name it explicitly in the dispatch prompt.

## Session handoffs

When the current session is getting long or you sense it's
the right moment to hand off to a fresh one — context is
getting heavy, a natural breakpoint just landed, a big new
phase is about to start — write a handoff document and
**end your turn with the line `Next up: <handoff pathname>`**.
The user will start a new session pointing at that file.

**File location and naming.** Handoffs live in
`docs/handoffs/` and are named `YYYY-MM-DD-slug.md`. Pick a
slug that points at what the NEXT session will do, not what
THIS one did (`continue-mvp-ready-work.md` beats
`type-and-slug-done.md`). Commit the handoff as part of the
session's final commit; don't leave it untracked.

**What the handoff contains** (no rigid template, but cover):
- Who you are next session (orchestrator / implementer /
  reviewer / ...).
- The state of play as of the handoff: what just shipped, what's
  in flight, what's about to be the next move.
- Pointers to the load-bearing tickets, commits, and docs by
  id/path. Future-you can't see the conversation, only the
  filesystem and the planner.
- Anything surprising or non-obvious that's still warm in
  your head and would cost cycles to re-derive.
- The actual first move you'd take if you were continuing.

Keep it under 400 words. The handoff isn't a project status
report — it's the minimum context-load for the next session
to start working without reorienting.

**Session start with a file path.** When a session opens
with a path to a handoff file as the first thing the user
says, read that file and — **without commenting on this
instruction or repeating the file contents** — write at
most 3 sentences saying what you're about to do. Then
start working. Don't ask for confirmation; the handoff IS
the instruction.

## Blog

Posts under `blog/content/posts/`. Planning siblings under
`blog/plans/`. Process is in `blog/WRITING.md`; style is in
`blog/STYLE.md`; the AI-writing-trope catalog is in
`blog/tropes.md`.

The blog-post-reviewer agent at `.claude/agents/blog-post-reviewer.md`
auto-loads when a post is about to ship. Dispatch it; apply the
fixes; re-dispatch if changes were substantive.

The `scripts/new-blog-post.py` helper stamps a post, an image
directory, and a planning sibling in one call. Don't hand-edit
the `date` frontmatter to be in the future.

## Reference clones

Read-only clones of upstream projects live under `./reference/`
(gitignored — not part of jjforge). They're cloned in-tree so
sessions can `grep` / `Read` them directly instead of round-
tripping through `WebFetch`. Cheaper, more reliable, and the
source-of-truth (the actual code or docs) is more authoritative
than rendered web pages.

Currently in there:

- `./reference/beads/` — Steve Yegge's [beads](https://github.com/steveyegge/beads)
  (`bd` CLI). The primary inspiration for `jjf ready` / `jjf
  remember` / the `--json` everywhere convention. Read this
  first when you need the canonical answer for "what does
  `bd list` / `bd ready` / `bd dep add` do?" — it's faster
  and more accurate than the rendered docs site.

**Check `./reference/` before reaching for `WebFetch`** on
upstream-project questions. A `find ./reference -maxdepth 2`
shows what's available; treat it as a local doc cache.

Reference clones are read-only — do NOT modify, commit, or
push them. If you need a clone that isn't there yet, run
`git clone <url> reference/<slug>` and use it; the parent
`/reference/` gitignore covers it.

## Experiments

Throwaway code and shell scripts live under `experiments/<topic>/`.
The `.gitignore` excludes:

- `experiments/**/.scratch/` (test-repo scratch dirs)
- `experiments/**/.scratch-followup/`
- `experiments/**/target/` (Cargo build dirs)

If your experiment creates a nested git or jj repo, **strip the
inner `.git/` or `.jj/` before committing**. The orchestrator has
hit this gotcha three times already; a nested `.git/` becomes a
gitlink (mode 160000) that points at nothing in the outer repo.

Pattern that works:

```bash
find experiments/<topic> -name ".git" -exec rm -rf {} +
find experiments/<topic> -name ".jj" -exec rm -rf {} +
git add experiments/<topic>
```

## Commits

- Per the standing rules: commit when work is done, don't ask.
- Add files by explicit name; never `git add .` or `git add -A`.
- Use a HEREDOC for multi-line commit messages.
- The Claude-Session footer in commits is fine in this repo
  unless you're told otherwise.
- **Push to `origin` at the end of every issue.** The remote
  (`git@github.com:myers/jjforge.git`,
  Forgejo on the user's infra) is canonical. After the
  commit(s) that close an issue land on `main`:
  ```bash
  git push origin main
  ```
  And, when the issue's work mutated jjforge data on the
  `issues` bookmark (status comments, new tickets, etc.):
  ```bash
  jjf push origin
  ```
  Don't batch these across multiple issues — one push per
  closed issue, so the remote tracks the orchestration
  cadence and any rollback is per-issue, not per-session.

## Orchestrating work

When the user asks you to "orchestrate" or "make progress" or
"dispatch subagents," the loop is:

1. **Read the roadmap first** (`jjf show roadmap`) to orient on
   what's up next and what's blocking it.

2. **Bugs before features.** Per Joel Spolsky's rule: fix
   defects in already-shipped behavior before starting new
   work. Before picking the next feature ticket, check for
   open bug-class issues — anything labeled `bug` or whose
   title/body reads as "X is broken" / "Y silently corrupts" /
   "Z doesn't do what it claims." If any are open, those go
   first, regardless of what the roadmap's feature order says.
   The roadmap describes the feature trajectory; the bug
   queue interrupts it. Rationale: shipped-but-broken behavior
   poisons the next feature built on top of it, and the cost
   of fixing a bug only grows as more code gets layered on
   the broken substrate.

3. **Find the next concrete ticket.** Either there's a named
   "do this now" ticket or there isn't. If there is, work
   backward: are its prerequisites filed and viable, or do
   they need detailing first?

4. **File any missing prerequisite tickets yourself before
   dispatching subagents.** The orchestrator owns the ticket
   graph. Subagents own the work inside one ticket. A subagent
   asked to "build X and file the ticket for it" will either
   skip the ticket or write it badly. Pre-file with a sketched
   body via `jjf new`; let the subagent close it.

5. **Dispatch serially, not in parallel.** Subagents writing to
   the `issues` bookmark race each other — concurrent commits on
   the same bookmark force one to lose and re-run. Parallel is
   fine ONLY when the subagents have disjoint write targets
   (different issue ids, different files in `experiments/<topic>/`).
   When in doubt, serial.

6. **Commit between dispatches.** Each subagent's experiments,
   docs, or other artifacts get committed before the next is
   dispatched. The next agent reads a clean tree; commit
   messages double as a worklog. Use the explicit-filename
   discipline from the Commits section.

7. **Run the workspace tests between dispatches.** After
   committing one subagent's work and before dispatching the
   next:

   ```bash
   cargo nextest run --workspace
   # fall back to `cargo test --workspace` if nextest isn't installed
   ```

   If anything is red — including tests written by an earlier
   round that this round broke — fix or roll back before
   dispatching the next subagent. Don't paper over a regression
   by dispatching more work on top of it.

8. **Post a status comment to each affected epic** when a child
   ticket closes (`jjf comment <epic-id> -F -`). The comment
   names the closed ticket, links the commit if one landed, and
   notes what's still unfiled. **If the priority order changed
   during the round** — a sketched epic earned a promotion, a
   closed epic falls off — update the roadmap body
   via `jjf update roadmap --body-file ...`. A short comment
   announcing "promoted X above Y" is fine but the truth lives
   in the body.

9. **Surface follow-ups to the user.** Stop and report when:
   the subagent budget is exhausted, a finding contradicts an
   epic's sketched approach, scope is creeping into a different
   epic, or the next move requires a design call only the user
   can make.

### Where to work

The orchestrator runs in whichever directory it was invoked.
**Commits land there.** If you were invoked in
`~/p/jjforge`, commits go to `~/p/jjforge`. If you were invoked
in a worktree at `~/p/jjforge-test-orchestration/`, scratch
work and commits stay in the worktree. Do NOT chdir to
`~/p/jjforge` to commit if you were invoked elsewhere — that
contaminates the real working tree with experimental state.

jjforge data lives on the `issues` bookmark in the same repo; it
travels between worktrees automatically with the bookmark.
Code and experiments do not.

### Operating in a colocated jj+git repo

jjforge v3 writes to `refs/jjf/*` via plain git plumbing and
never moves git HEAD. No special handling is needed for any
host repo, including jjforge's own source tree. Mutating verbs
(`new`, `update`, `comment`, `close`, ...) run from inside this
repo without drift; no env-var opt-in, no sibling working dir,
no HEAD recovery dance.

### Dispatch prompt template

When dispatching a subagent on an issue, include:

- The issue id and the command to view it
  (`cd ~/p/jjforge && jjf show <id>`).
- A one-sentence summary of why this work is happening now
  (which epic, what's blocked on it).
- Pointers to prior-subagent findings as paths or issue ids
  (`see ~/p/jjforge/experiments/.../README.md` or
  `the closing comment on <prior-id> records the verdict`).
- The housekeeping note about stripping nested `.git/`/`.jj/`
  from experiment dirs.
- An explicit "report back under 200 words" cap.

The `subagent-working-a-jjforge-issue` skill auto-loads from
keywords ("issue" / "ticket" / "jjforge" / "jjf") and enforces
the four-section closing-comment recipe. Don't re-explain that
in the dispatch prompt; let the skill carry it. (See "Subagent
discipline" above.)

## What's next

The project's running roadmap is a single ticket of type
`roadmap` (also slug `roadmap`):

```bash
jjf show roadmap
```

It lists the open epics in priority order, with an "above
the line" / "below the line" cut for what's shipping now vs.
queued. The ticket stays open for the life of the project.
Body-edit it (`jjf update roadmap --body-file <path>`) when the
order shifts; fall back to comments for finer-grained changes.

For "what exists" — every issue, by label, by status — use
the queries below, not a maintained index.

Don't expand scope into epics below the roadmap's "above the
line" cut until the roadmap explicitly pulls them up.

## Queries

Useful invocations for navigating jjforge. See
`docs/cli-json.md` for the output shapes.

```bash
# Roadmap — priority order, blocking judgment
jjf show roadmap                  # by slug (also `--type roadmap`)

# Epics — the six top-level milestones
jjf ls --label epic

# All tickets of a given type (v2.1)
jjf ls --type bug
jjf ls --type epic --type feature # OR-semantics across types

# Slug substring lookup (v2.1)
jjf ls --slug agent

# Next unblocked thing to work on (v2.1) — agent-ergonomics
# headline verb. Returns issues whose deps are all closed,
# sorted by type priority (bug > feature > research > epic >
# unspecified; roadmap excluded), then FIFO by created_at.
jjf ready                         # everything that's unblocked
jjf ready --limit 1               # just the next one
jjf ready --json --limit 1        # machine-readable, one issue
jjf ready --label backend         # filter by label intersection
jjf ready --type bug              # bugs only

# Work under one epic — open tickets only
jjf ls --label epic:mvp-storage --status open

# Everything ever attached to an epic — open and closed
jjf ls --label epic:mvp-sync --status all

# Closed tickets
jjf ls --status closed

# JSON for scripting
jjf ls --json --label epic | jq '.[] | {id, title}'

# Substring search across titles, bodies, and (optionally)
# comment bodies (v2.9). Case-insensitive, NOT regex. Default
# limit 20, default snippet window ±40 chars. matched_field
# priority: title > body > comments.
jjf search "concurrent_write"                      # titles + bodies
jjf search "body cap" --include-comments           # plus comments
jjf search "needle" --status open --label backend  # filters compose AND
jjf search --json "needle" --limit 5 \
    | jq -r '.results[] | "\(.id)\t\(.matched_field)\t\(.title)"'

# Issues not touched in the last N days (v2.10). Default
# --days 14; default --status open. Sorted oldest first.
# Plain-text age column: Nd (<30d) / Nw (30-90d) / Nmo (>=90d).
jjf stale --days 14                                # default; open issues only
jjf stale --days 1 --label epic:host-asterinas --json  # compose with filters
```

Filters jjforge doesn't yet ship that we want (file as
agent-ergonomics tickets when needed):

- `--unblocked-by <id>`: "tell me what would become ready if X
  closes." Useful follow-up to `jjf ready` for planning.
- `jjf dep ls <id>` — flat list of an issue's edges, by kind.
  `jjf dep tree` walks parent-child only; auditing `blocks`
  edges currently requires `jjf show` per issue. With
  `dep-cycle-undetected` (`43c7615`) now rejecting cycles at
  write time, this is mostly a diagnostic aid for understanding
  why something is blocked; still worth filing.

If a useful filter isn't here, add it. If `jjf` can't express
it, that's a feature request — file it.

## History

This project ran on `git-bug` (in `refs/bugs/*`) for three
sessions before cutover on 2026-06-22. The pre-cutover history
is preserved in git and remains readable via `git-bug bug show
<id>`. The mapping from old to new issue ids — and the
rationale for "start fresh" vs. "migrate" — lives in
`docs/git-bug-cutover.md` and in archived `d12031c`.

The `bin/jjf` shim now delegates to the Rust binary; reach
the pre-cutover archive via `git-bug` directly. See the
"Reading historical git-bug data" section above.
