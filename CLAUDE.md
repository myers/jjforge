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
  the full v1 verb set (`init`, `new`, `show`, `ls`, `update`,
  `comment`, `close`/`open`, `label add|rm`, `remote add|ls|rm`,
  `push`, `pull`) with `--json` on every verb. Storage spec
  pinned in `docs/storage-format.md`; CLI output contract in
  `docs/cli-json.md`. 178 workspace tests green.
- **Planning surface:** `jjforge` itself, on the `issues` bookmark
  in this repo. As of 2026-06-22 the project's own planning runs
  on `jjf`. Pre-cutover history lives in archived git-bug refs
  (`refs/bugs/*`); the bridge is `docs/git-bug-cutover.md`. (The
  v1 → v2 rename in `docs/storage-format.md` moved the live planner
  data from `bugs` to `issues` automatically — the storage layer
  detects v1-shape repos and migrates on first `Storage::open`.)
- **Entry point:** the roadmap (`9566f52`). Read it first.
- **CI:** `.woodpecker/blog.yaml` builds and pushes a Zola site
  image. Mirrors zfs-workspace's pattern except for the
  notify-flux hook (jjforge isn't a Flux deployment target).

## How to use jjforge

The project is dogfooding `jjf` as its own planner. Treat every
rough edge as data, not as something to fix around.

### Entry point and discovery

The roadmap is the orientation document. Start there in any
new session before touching anything else:

```bash
jjf show 9566f52
```

To navigate from there, see the Queries section at the bottom
of this file.

### Label scheme

- **`roadmap`** — the running plan (one ticket, never closes;
  body edited in place via `jjf update --body-file`).
- **`epic`** — the top-level epic issues. Each carries the goal,
  the sketched approach, the tickets we expect to file under it,
  and its dependency graph.
- **`epic:<slug>`** — every issue belonging to an epic (the epic
  itself plus child tickets when they're filed). Use the
  colon-prefixed form always.

### Creating a new issue

Use `-F -` to read the body from stdin (the recommended pattern
— no interactive flow, no editor invocation, scripts cleanly).
Pass labels with `-l` (repeatable). Optional flags: `-d <id>`
(repeatable, dependencies), `-a <name>` (assignee). Use
`--json` for the machine-readable envelope.

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
worry about, no SHA stem to lengthen. CLI verbs accept any
unambiguous prefix (often 4 characters is enough), but the
canonical id is the 7-char string `jjf ls` prints.

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

The `bin/jjf` shell shim still delegates to `git-bug` against
`refs/bugs/*` (the pre-2026-06-22 planner data). It stays in
place as a **read-only window** until `cli-replace-shim`
flips it to the Rust binary. The archived data covers:

- Every status-update comment on each pre-cutover epic.
- The five 2026-06-21 research tickets and their full closing
  comments (the source of truth for the storage and sync
  verdicts pinned in the current epic bodies).
- Every closed child ticket (the workshop floor: subagent
  finds, follow-ups, debate notes).

Useful:

```bash
git-bug bug show <old-7hex-id>      # one ticket
git-bug bug --label epic            # archived epics
git-bug bug --label research        # the research record
git-bug bug --status closed         # archived closed tickets
```

The cutover doc at `docs/git-bug-cutover.md` carries the
old → new id mapping and the historical-bug recipe.

**Never run `git-bug wipe`**. The archived data is the only
copy of pre-cutover history; once gone, it's gone.

## Subagent discipline

When dispatching a subagent to work an issue, the
**`subagent-working-a-git-bug-issue`** skill auto-loads on
keywords like "issue" or "git-bug". It enforces:

- The closing comment uses the four-section recipe: Findings,
  Recommendation, Confidence, Open follow-ups.
- The agent closes the issue when work is complete, or leaves it
  open with the Findings explaining why.
- The agent does not edit the original body, does not touch other
  issues unless cross-link is warranted, and does not push.
- The closing return-value to the orchestrator is under 200
  words.

**Heads-up (2026-06-22):** the skill's *body* still talks about
`git-bug` commands and `refs/bugs/*` semantics — it's stale
post-cutover. The closure recipe is still correct; the verb
shape needs updating to `jjf comment`, `jjf close`, etc. A
follow-up to rewrite this skill is tracked under
`epic:agent-ergonomics` (`5a755ec`) as `ergo-subagent-skill`.
Until that lands: the skill auto-loads and gives the right
discipline, but mentally translate `git-bug X` → `jjf X` when
following its instructions.

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

1. **Read the roadmap first** (`jjf show 9566f52`) to orient on
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
   closed epic falls off — update the roadmap (`9566f52`) body
   via `jjf update 9566f52 --body-file ...`. A short comment
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

This repo is colocated (`jj git init --colocate` was run during
the cutover). The colocate setup creates a footgun: the storage
layer's 4-CLI write dance moves the jj working copy onto an
empty descendant of the `issues` bookmark, which in a colocated
repo also drives **git** HEAD onto `refs/jj/root` — a phantom
empty root commit. Recovery is destructive
(`git symbolic-ref HEAD refs/heads/main && git reset --hard main`)
and the symptoms (whole tree shows as deleted, `git commit` lands
on a phantom root) cost two recovery rounds before the guard
landed.

**The guard.** Per issue `08cf14b`, every mutating `jjf` verb
(`init`, `new`, `update`, `comment`, `close`, `open`, `label`,
`push`, `pull`) refuses to run from inside the source repo —
detected by the presence of both `crates/jjf/Cargo.toml` and
`docs/storage-format.md` at some ancestor of cwd. The refusal
is a typed preflight error (`self_hosted_write_refused`, exit 2).
Read verbs (`show`, `ls`, `remote ls`) pass through unguarded.

**Canonical operator pattern: sibling working dir.** Clone the
repo into a second location (e.g. `~/p/jjforge-data`), `cd`
there, and run mutating `jjf` verbs against that sibling. The
`bugs` bookmark refs live in the underlying git database and
travel between siblings via `jjf push` / `jjf pull` (or via a
shared remote). This keeps the source tree's HEAD on `main`
where the rust workspace expects it.

**Bypass (orchestrator authorized).** When the orchestrator
*genuinely* needs to write from inside the source repo (e.g. to
file a status comment on a roadmap ticket as part of a dispatch
cycle), set `JJF_ALLOW_SELF_HOST=1` in the environment. The
bypass emits a stderr line announcing itself (text mode only;
silent under `--json`), and the drift WILL happen — so after the
mutating verb completes, restore git HEAD:

```bash
git symbolic-ref HEAD refs/heads/main
git reset --hard main
```

Note: `jj edit main` rebases the descendant chain and re-SHAs
the commits we just landed; **do not** use it as the recovery
step on the orchestrator's own commits. The git-symbolic-ref +
reset path is the correct recovery.

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

The `subagent-working-a-git-bug-issue` skill auto-loads from
keywords ("issue" most reliably) and enforces the four-section
closing-comment recipe. Don't re-explain that in the dispatch
prompt; let the skill carry it. (See "Subagent discipline"
above for the stale-name note.)

## What's next

The project's running roadmap is a single ticket labeled
`roadmap`:

```bash
jjf show 9566f52
```

It lists the open epics in priority order, with an "above
the line" / "below the line" cut for what's shipping now vs.
queued. The ticket stays open for the life of the project.
Body-edit it (`jjf update 9566f52 --body-file <path>`) when the
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
jjf show 9566f52

# Epics — the six top-level milestones
jjf ls --label epic

# Work under one epic — open tickets only
jjf ls --label epic:mvp-storage --status open

# Everything ever attached to an epic — open and closed
jjf ls --label epic:mvp-sync --status all

# Closed tickets
jjf ls --status closed

# JSON for scripting
jjf ls --json --label epic | jq '.[] | {id, title}'
```

Two filters jjforge doesn't yet ship that we want (file as
agent-ergonomics tickets when needed):

- A real `blocks` / `blocked-by` relation. `jjf ls --ready`
  filtering open issues whose dependencies are all closed is
  the headline agent-ergonomics primitive (`jjf ready`).
- Full-text search across bodies and comments. git-bug had a
  query language; jjforge has none yet.

If a useful filter isn't here, add it. If `jjf` can't express
it, that's a feature request — file it.

## History

This project ran on `git-bug` (in `refs/bugs/*`) for three
sessions before cutover on 2026-06-22. The pre-cutover history
is preserved in git and remains readable via `git-bug bug show
<id>`. The mapping from old to new issue ids — and the
rationale for "start fresh" vs. "migrate" — lives in
`docs/git-bug-cutover.md` and in archived `d12031c`.

The `bin/jjf` shell shim still points at `git-bug` as a
read-only window into the archive; `cli-replace-shim`
(under `epic:mvp-cli`, ticket `a5f8122`) flips it to the
Rust binary.
