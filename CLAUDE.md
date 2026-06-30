# git-issues — Claude operating notes

A jj-native, agent-first issue tracker. CLI: `iss`. The project's
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

- **Status:** post-MVP. The Rust binary at `crates/iss/` covers
  the full verb set (`init`, `new`, `show`, `ls`, `ready`,
  `update`, `comment`, `close`/`open`, `block`/`unblock`,
  `abandon`, `label add|rm`, `dep add|rm|tree`,
  `remote add|ls|rm`, `push`, `pull`, `remember`/`memories`/
  `recall`/`forget`) with `--json` on every verb. Storage spec
  pinned in `docs/architecture.md` + `docs/storage-out-of-tree.md`; CLI output contract in
  `docs/cli-json.md`.
- **Planning surface:** `git-issues` itself, on the `issues` bookmark
  in this repo. As of 2026-06-22 the project's own planning runs
  on `iss`. Pre-cutover history lives in archived git-bug refs
  (`refs/bugs/*`); the bridge is `docs/git-bug-cutover.md`. (The
  v1 → v2 rename in `docs/architecture.md` + `docs/storage-out-of-tree.md` moved the live planner
  data from `bugs` to `issues` automatically — the storage layer
  detects v1-shape repos and migrates on first `Storage::open`.)
- **Entry point:** the roadmap. Read it first via
  `iss show roadmap` (the issue's slug is `roadmap` and its
  type is `roadmap` — both let you skip the 7-char id).
- **CI:** `.woodpecker/blog.yaml` builds and pushes a Zola site
  image. Mirrors zfs-workspace's pattern except for the
  notify-flux hook (git-issues isn't a Flux deployment target).

## Multiple host repos — auto-migration matters now

As of 2026-06-29, git-issues is no longer a single-host tool. Live
host repos using `refs/jjf/issues/*`:

- **`~/p/jjforge`** (self-hosted, since 2026-06-22; `issues`
  bookmark migrated to v3 refs in `bd98097`).
- **`~/p/asterinas-workspace`** (trial migration started
  2026-06-28; 18 issues seeded across `epic-01-qemu-first-light`
  and `epic-01-host-net`; bulk run pending per
  `docs/host-asterinas-dispatch.md` and `cc2fa96`).

More are likely soon — every workspace that wants `iss ready`
is a candidate. This changes the math on storage migrations:

### What's at stake

Any breaking change to the storage format — schema bump, new
required field, ref-namespace move, JSON-envelope tweak — now
needs to land cleanly in **every** host repo a user has on
disk, not just git-issues's own. The v2 → v3 storage move
(`bd98097`) was tractable because there was exactly one live
host repo (git-issues itself) and the migration ran on first
`Storage::open`. We can't keep that pattern naively: an
operator with five host repos shouldn't have to remember which
ones need a migration pass before they next use them, and they
shouldn't have to re-pull git-issues to find out a migration was
even needed.

### Migration-design rules (provisional)

These are the operating instincts; they're not yet a
formalized policy and the first real cross-repo migration will
sharpen them.

1. **Detect at `Storage::open` time.** Every host repo's first
   open of a session checks the on-disk format version and
   compares to the binary's expected version. If they differ,
   migrate or refuse with a clear message — never silently
   read a stale shape.
2. **Migrations should be self-contained.** The migration code
   for `vN → vN+1` lives in the `iss` binary and runs from the
   binary alone. No external script, no out-of-band data file.
   Operators upgrade their `iss` binary and migrations Just Run.
3. **Never assume single-host.** Don't store global state in
   `~/.config/iss/` that a per-host migration might depend on.
   Each host's `refs/jjf/*` IS the state; the binary is the
   actor.
4. **Version on every record, not just at the repo level.**
   The v3 storage already does this — each issue record
   carries its schema version. Cross-version reads can degrade
   gracefully (warn-and-skip) rather than crash; cross-version
   writes refuse until migrated.
5. **`iss migrate` as an explicit verb is on the table.** For
   migrations that are expensive or destructive (anything
   beyond a JSON re-shape), an explicit verb operators run
   beats silent open-time migration. The trade-off: explicit
   = visible cost + audit trail; silent = no operator
   friction but invisible breakage if it fails partway.
6. **Cross-repo broadcast is the operator's job.** git-issues
   does not phone home or maintain a registry of host repos.
   When a migration ships, the user knows their own host repos
   and runs the upgrade. CLAUDE.md (this file) and
   `docs/architecture.md` + `docs/storage-out-of-tree.md` are the source of truth for
   "which version is current."

### Open questions

- Does `iss migrate` need to be a verb today, or wait for the
  first real breaking change? (Today: no breaking change is
  imminent.)
- How does an operator find every host repo on their disk?
  (`find ~ -name '.jj' -type d` finds candidates; we don't
  have a registry.)
- Read-only access from an older `iss` to a newer-format repo:
  refuse with a clear message, or downgrade-degrade? (Lean:
  refuse — agents misreading a newer shape is worse than a
  noisy error.)

The first cross-repo migration that lands after this section
gets to update or refute these rules in place.

## How to use git-issues

The project is dogfooding `iss` as its own planner. Treat every
rough edge as data, not as something to fix around.

### Entry point and discovery

The roadmap is the orientation document. Start there in any
new session before touching anything else; surface persistent
memories at the same time:

```bash
iss show roadmap --include-memories
```

(The roadmap issue has slug `roadmap` and type `roadmap`;
either resolves it. Its 7-char id is `9566f52` if you ever
need it for archival cross-references.)

The `--include-memories` flag appends a `## Persistent
Memories` block listing every `iss remember` entry on the
bookmark. These are short declarative facts that travel with
the planner via `iss push`/`pull` (operational rules,
codebase folklore, architectural decisions). Read them; they
exist so you don't have to re-derive what an earlier session
already learned.

Manage memories with:

```bash
iss remember "<insight>"                 # write; key auto-slugged
iss remember "<insight>" --key <slug>    # write with explicit key
iss memories                             # list all
iss memories <substring>                 # filter
iss recall <key>                         # read one
iss forget <key>                         # remove
```

Save a memory when you've learned something the next session
would otherwise re-derive (a non-obvious workflow rule, a
gotcha in the codebase, a constraint that's not visible from
the code alone). Memories are project-scoped; do not save
operator preferences here — those go in `~/.claude/projects/`.

**Memories don't auto-decay.** When an invariant they
describe gets refactored away (e.g. an env var retired, a
workflow rule lifted, a file path moved), `iss forget <key>`
is the right move. Skim the memory list when you finish a
session that touched anything load-bearing; a stale memory
is worse than no memory.

To navigate from the roadmap, see the Queries section at the
bottom of this file.

### Label scheme

- **`roadmap`** — the running plan (one ticket, never closes;
  body edited in place via `iss update --body-file`).
- Issues that belong to an epic attach via a `parent-child` dep
  edge. Use `--parent <epic>` on `iss new`, or `iss dep add
  --kind parent-child <child> <epic>` after the fact. Filter
  with `iss ls --parent <epic>` / `iss ready --parent <epic>`.
- Epics themselves are typed `epic` (`--type epic`). The
  label-based epic convention is retired (replaced by the
  parent-child edge above).

### Epics vs child issues

Epics describe a **goal**: what "done" looks like, the approach
sketch, dependency edges to other epics. Child tickets are
**how we get there** — each one a unit of work.

Keep the two layers separate. The epic body must NOT enumerate
its children or absorb their findings: no "Child tickets:
`abc1234` — …" lists, no "Post-X sweep" subsections naming the
shipped tickets. The child inventory is discovered via
`iss ls --parent <epic> --status all`; the epic body is
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
The slug lets you say `iss show agent-ready` instead of `iss
show 69d5e1b`, and the type drives `iss ls --type bug` /
`iss ready`'s priority sort.

```bash
cat <<'EOF' | iss new --json -t "Real title goes here" --parent mvp-cli -F - -l epic
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
NEW_ID=$(iss new --json -t "..." -F body.txt -l epic | jq -r .id)
```

### Updating issues

```bash
iss update <id> --title "New title"          # rename
iss update <id> --status closed              # change status
iss update <id> --body-file body.md          # rewrite body in place
iss update <id> --assignee alice             # assign
iss update <id> --unset-assignee             # unassign
iss assign <id> alice                        # shorthand for --assignee
iss assign <id> ""                           # shorthand for --unset-assignee

ISS_ACTOR=haiku-slice-3 iss update <id> --claim   # multi-agent fan-out
iss update <id> --claim --actor haiku-slice-3     # per-invocation override

iss close <id>                               # convenience for status closed
iss open <id>                                # convenience for status open

iss label add <id> <label>                   # add a label
iss label rm <id> <label>                    # remove a label

cat body.md | iss comment <id> -F -          # append a comment
iss comment <id> -F body.md                  # ... from a file
```

Every mutating verb takes `--json` and emits the
`{"ok": true, ...}` envelope shape documented in
`docs/cli-json.md`. Multiple `--title`/`--status`/`--body-file`/
`--assignee` flags on the same `update` call land as a single
multi-op commit (one trailer per field).

### Bodies are editable now

Unlike pre-cutover git-bug, **git-issues supports editing an issue's
body in place** via `iss update <id> --body-file <path>`. This
is the right way to revise a roadmap, fix an epic's plan, or
restate scope. Comments are still useful for status updates and
mid-stream findings — but the body can be made authoritative
without an append-only comment trail.

The pre-cutover roadmap convention ("latest comment is the
truth, body is stale") was a workaround for git-bug's missing
edit-body command. We can stop doing that. When the priority
order shifts, edit the roadmap body; a single `iss comment`
on the roadmap announcing the change is courtesy, not contract.

### Issue-id length

git-issues ids are **7-character lowercase hex** by design (28 bits,
generated at create time per `docs/architecture.md` + `docs/storage-out-of-tree.md` §2). The
displayed id IS the full id — there is no prefix convention to
worry about, no SHA stem to lengthen.

CLI verbs do NOT accept partial-id prefixes; pass the full
7-char id or a slug. A 7-char hex handle that doesn't match
any issue surfaces as `issue_not_found` (exit 1, runtime). A
non-hex handle with no matching slug surfaces as
`slug_not_found` (exit 2, preflight). Slugs are the
human-friendly short form — use `--slug <kebab>` on
`iss new` so future commands can say `iss show agent-ready`
instead of `iss show 69d5e1b`.

### Push / pull

`iss push <remote>` and `iss pull <remote>` round-trip the `issues`
bookmark via standard git transport. No special refspec config
needed — jj 0.40 carries the bookmark automatically (finding
verified in archived `refs/bugs/07780aa`). `iss remote
add|ls|rm` wraps `jj git remote *` for managing remotes.

**The `origin` remote (`git@github.com:myers/git-issues.git`,
a Forgejo on the user's infrastructure) IS configured.** It's
the canonical home of the code; `main` tracks `origin/main`.
Push at the end of every issue (see Commits section). The
git-issues `issues` bookmark also rides this remote — `iss push
origin` round-trips the planner data alongside.

### Reading historical (pre-cutover) git-bug data

The `bin/iss` shim now delegates to the Rust binary (prefers
`target/release/iss`, falls back to `target/debug/iss`, builds
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
**`subagent-working-a-git-issues-issue`** skill auto-loads on
keywords like "issue", "ticket", "git-issues", or "iss". It enforces:

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
(gitignored — not part of git-issues). They're cloned in-tree so
sessions can `grep` / `Read` them directly instead of round-
tripping through `WebFetch`. Cheaper, more reliable, and the
source-of-truth (the actual code or docs) is more authoritative
than rendered web pages.

Currently in there:

- `./reference/beads/` — Steve Yegge's [beads](https://github.com/steveyegge/beads)
  (`bd` CLI). The primary inspiration for `iss ready` / `iss
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
  (`git@github.com:myers/git-issues.git`,
  Forgejo on the user's infra) is canonical. After the
  commit(s) that close an issue land on `main`:
  ```bash
  git push origin main
  ```
  And, when the issue's work mutated git-issues data on the
  `issues` bookmark (status comments, new tickets, etc.):
  ```bash
  iss push origin
  ```
  Don't batch these across multiple issues — one push per
  closed issue, so the remote tracks the orchestration
  cadence and any rollback is per-issue, not per-session.

## Orchestrating work

When the user asks you to "orchestrate" or "make progress" or
"dispatch subagents," the loop is:

1. **Read the roadmap first** (`iss show roadmap`) to orient on
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
   body via `iss new`; let the subagent close it.

5. **Dispatch serially, not in parallel.** Subagents writing to
   the `issues` bookmark race each other — concurrent commits on
   the same bookmark force one to lose and re-run. Parallel is
   fine ONLY when the subagents have disjoint write targets
   (different issue ids, different files in `experiments/<topic>/`).
   When in doubt, serial.

   **Multi-agent attribution.** When dispatching N parallel
   agents on disjoint issue ids, set `ISS_ACTOR=<distinct-name>`
   in each agent's environment so `iss update --claim` and
   `iss comment` attribute work to that agent specifically.
   Without it, every agent claims under the same shared
   `jj user.name` and the `assignee` column can't tell them
   apart. The `--actor <name>` flag on `iss update` is the
   per-invocation override if you don't want to set env. Chain
   precedence: `--actor` > `ISS_ACTOR` > `jj user.name`.

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
   ticket closes (`iss comment <epic-id> -F -`). The comment
   names the closed ticket, links the commit if one landed, and
   notes what's still unfiled. **If the priority order changed
   during the round** — a sketched epic earned a promotion, a
   closed epic falls off — update the roadmap body
   via `iss update roadmap --body-file ...`. A short comment
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

git-issues data lives on the `issues` bookmark in the same repo; it
travels between worktrees automatically with the bookmark.
Code and experiments do not.

### Operating in a colocated jj+git repo

git-issues v3 writes to `refs/jjf/*` via plain git plumbing and
never moves git HEAD. No special handling is needed for any
host repo, including git-issues's own source tree. Mutating verbs
(`new`, `update`, `comment`, `close`, ...) run from inside this
repo without drift; no env-var opt-in, no sibling working dir,
no HEAD recovery dance.

### Additive field policy

A new field that's empty-by-default and read-tolerant via
`#[serde(default)]` does NOT bump the record version. Every
additive field shipped to date follows this rule:

- `priority` (v2.8, additive tolerated)
- `slug` (v2.1, additive tolerated)
- `type` (v2.1, additive tolerated)
- `metadata` (2026-06-29, additive tolerated)

Reserve version bumps for breaking changes — removed fields,
semantic changes to existing fields, ref-namespace moves. The
asymmetric-read-tolerance trade-off (older binaries silently
drop the new field on write-back, losing the data) is the
explicit cost.

If a peer agent or operator ships a breaking change, that
DOES bump the version, AND it ships with a migration recipe
(per any migration documentation).

### Referring to issues: prefer slugs

When you name an issue in user-facing text — dispatch announcements,
task subjects, status updates, commit messages, comments — lead with
the **slug**. The 7-char id is supporting detail. "Dispatching
subagent on `proptest-multi-issue-generator`" reads cleaner than
"Dispatching subagent on `c6aed85`"; the user can predict what's
about to happen from the first form and can't from the second.

Both at once is fine when context warrants it:
`c6aed85 proptest-multi-issue-generator` in a status comment is
helpful — the slug names the work, the id makes the cross-reference
unambiguous if a future reader greps. But in a one-line update,
just the slug.

Slug-less issues exist (older tickets, anything created without
`--slug`). For those, the id is the only handle — use it. When
filing new tickets, set `--slug <kebab-case>` so the next
orchestrator can refer to your work without reaching for the id.

### Dispatch prompt template

When dispatching a subagent on an issue, include:

- The issue id and the command to view it
  (`cd ~/p/jjforge && iss show <id>`).
- A one-sentence summary of why this work is happening now
  (which epic, what's blocked on it).
- Pointers to prior-subagent findings as paths or issue ids
  (`see ~/p/jjforge/experiments/.../README.md` or
  `the closing comment on <prior-id> records the verdict`).
- The housekeeping note about stripping nested `.git/`/`.jj/`
  from experiment dirs.
- An explicit "report back under 200 words" cap.

The `subagent-working-a-git-issues-issue` skill auto-loads from
keywords ("issue" / "ticket" / "git-issues" / "iss") and enforces
the four-section closing-comment recipe. Don't re-explain that
in the dispatch prompt; let the skill carry it. (See "Subagent
discipline" above.)

## What's next

The project's running roadmap is a single ticket of type
`roadmap` (also slug `roadmap`):

```bash
iss show roadmap
```

It lists the open epics in priority order, with an "above
the line" / "below the line" cut for what's shipping now vs.
queued. The ticket stays open for the life of the project.
Body-edit it (`iss update roadmap --body-file <path>`) when the
order shifts; fall back to comments for finer-grained changes.

For "what exists" — every issue, by label, by status — use
the queries below, not a maintained index.

Don't expand scope into epics below the roadmap's "above the
line" cut until the roadmap explicitly pulls them up.

## Queries

Useful invocations for navigating git-issues. See
`docs/cli-json.md` for the output shapes.

```bash
# Roadmap — priority order, blocking judgment
iss show roadmap                  # by slug (also `--type roadmap`)

# Epics — the six top-level milestones
iss ls --type epic

# All tickets of a given type (v2.1)
iss ls --type bug
iss ls --type epic --type feature # OR-semantics across types

# Slug substring lookup (v2.1)
iss ls --slug agent

# Next unblocked thing to work on (v2.1) — agent-ergonomics
# headline verb. Returns issues whose deps are all closed,
# sorted by type priority (bug > feature > research > epic >
# unspecified; roadmap excluded), then FIFO by created_at.
iss ready                         # everything that's unblocked
iss ready --limit 1               # just the next one
iss ready --json --limit 1        # machine-readable, one issue
iss ready --parent backend        # filter by parent epic
iss ready --type bug              # bugs only

# Work under one epic — open tickets only
iss ls --parent mvp-storage --status open

# Everything ever attached to an epic — open and closed
iss ls --parent mvp-sync --status all

# Closed tickets
iss ls --status closed

# JSON for scripting
iss ls --json --type epic | jq '.[] | {id, title}'

# Substring search across titles, bodies, and (optionally)
# comment bodies (v2.9). Case-insensitive, NOT regex. Default
# limit 20, default snippet window ±40 chars. matched_field
# priority: title > body > comments.
iss search "concurrent_write"                      # titles + bodies
iss search "body cap" --include-comments           # plus comments
iss search "needle" --status open --label backend  # filters compose AND
iss search --json "needle" --limit 5 \
    | jq -r '.results[] | "\(.id)\t\(.matched_field)\t\(.title)"'

# Issues not touched in the last N days (v2.10). Default
# --days 14; default --status open. Sorted oldest first.
# Plain-text age column: Nd (<30d) / Nw (30-90d) / Nmo (>=90d).
iss stale --days 14                                # default; open issues only
iss stale --days 1 --parent host-asterinas --status open --json  # compose with filters
```

Filters git-issues doesn't yet ship that we want (file as
agent-ergonomics tickets when needed):

- `--unblocked-by <id>`: "tell me what would become ready if X
  closes." Useful follow-up to `iss ready` for planning.
- `iss dep ls <id>` — flat list of an issue's edges, by kind.
  `iss dep tree` walks parent-child only; auditing `blocks`
  edges currently requires `iss show` per issue. With
  `dep-cycle-undetected` (`43c7615`) now rejecting cycles at
  write time, this is mostly a diagnostic aid for understanding
  why something is blocked; still worth filing.

If a useful filter isn't here, add it. If `iss` can't express
it, that's a feature request — file it.

## History

This project ran on `git-bug` (in `refs/bugs/*`) for three
sessions before cutover on 2026-06-22. The pre-cutover history
is preserved in git and remains readable via `git-bug bug show
<id>`. The mapping from old to new issue ids — and the
rationale for "start fresh" vs. "migrate" — lives in
`docs/git-bug-cutover.md` and in archived `d12031c`.

The `bin/iss` shim now delegates to the Rust binary; reach
the pre-cutover archive via `git-bug` directly. See the
"Reading historical git-bug data" section above.

<!-- rtk-instructions v2 -->
# RTK (Rust Token Killer) - Token-Optimized Commands

## Golden Rule

**Always prefix commands with `rtk`**. If RTK has a dedicated filter, it uses it. If not, it passes through unchanged. This means RTK is always safe to use.

**Important**: Even in command chains with `&&`, use `rtk`:
```bash
# ❌ Wrong
git add . && git commit -m "msg" && git push

# ✅ Correct
rtk git add . && rtk git commit -m "msg" && rtk git push
```

## RTK Commands by Workflow

### Build & Compile (80-90% savings)
```bash
rtk cargo build         # Cargo build output
rtk cargo check         # Cargo check output
rtk cargo clippy        # Clippy warnings grouped by file (80%)
rtk tsc                 # TypeScript errors grouped by file/code (83%)
rtk lint                # ESLint/Biome violations grouped (84%)
rtk prettier --check    # Files needing format only (70%)
rtk next build          # Next.js build with route metrics (87%)
```

### Test (60-99% savings)
```bash
rtk cargo test          # Cargo test failures only (90%)
rtk go test             # Go test failures only (90%)
rtk jest                # Jest failures only (99.5%)
rtk vitest              # Vitest failures only (99.5%)
rtk playwright test     # Playwright failures only (94%)
rtk pytest              # Python test failures only (90%)
rtk rake test           # Ruby test failures only (90%)
rtk rspec               # RSpec test failures only (60%)
rtk test <cmd>          # Generic test wrapper - failures only
```

### Git (59-80% savings)
```bash
rtk git status          # Compact status
rtk git log             # Compact log (works with all git flags)
rtk git diff            # Compact diff (80%)
rtk git show            # Compact show (80%)
rtk git add             # Ultra-compact confirmations (59%)
rtk git commit          # Ultra-compact confirmations (59%)
rtk git push            # Ultra-compact confirmations
rtk git pull            # Ultra-compact confirmations
rtk git branch          # Compact branch list
rtk git fetch           # Compact fetch
rtk git stash           # Compact stash
rtk git worktree        # Compact worktree
```

Note: Git passthrough works for ALL subcommands, even those not explicitly listed.

### GitHub (26-87% savings)
```bash
rtk gh pr view <num>    # Compact PR view (87%)
rtk gh pr checks        # Compact PR checks (79%)
rtk gh run list         # Compact workflow runs (82%)
rtk gh issue list       # Compact issue list (80%)
rtk gh api              # Compact API responses (26%)
```

### JavaScript/TypeScript Tooling (70-90% savings)
```bash
rtk pnpm list           # Compact dependency tree (70%)
rtk pnpm outdated       # Compact outdated packages (80%)
rtk pnpm install        # Compact install output (90%)
rtk npm run <script>    # Compact npm script output
rtk npx <cmd>           # Compact npx command output
rtk prisma              # Prisma without ASCII art (88%)
```

### Files & Search (60-75% savings)
```bash
rtk ls <path>           # Tree format, compact (65%)
rtk read <file>         # Code reading with filtering (60%)
rtk grep <pattern>      # Search grouped by file (75%). Format flags (-c, -l, -L, -o, -Z) run raw.
rtk find <pattern>      # Find grouped by directory (70%)
```

### Analysis & Debug (70-90% savings)
```bash
rtk err <cmd>           # Filter errors only from any command
rtk log <file>          # Deduplicated logs with counts
rtk json <file>         # JSON structure without values
rtk deps                # Dependency overview
rtk env                 # Environment variables compact
rtk summary <cmd>       # Smart summary of command output
rtk diff                # Ultra-compact diffs
```

### Infrastructure (85% savings)
```bash
rtk docker ps           # Compact container list
rtk docker images       # Compact image list
rtk docker logs <c>     # Deduplicated logs
rtk kubectl get         # Compact resource list
rtk kubectl logs        # Deduplicated pod logs
```

### Network (65-70% savings)
```bash
rtk curl <url>          # Compact HTTP responses (70%)
rtk wget <url>          # Compact download output (65%)
```

### Meta Commands
```bash
rtk gain                # View token savings statistics
rtk gain --history      # View command history with savings
rtk discover            # Analyze Claude Code sessions for missed RTK usage
rtk proxy <cmd>         # Run command without filtering (for debugging)
rtk init                # Add RTK instructions to CLAUDE.md
rtk init --global       # Add RTK to ~/.claude/CLAUDE.md
```

## Token Savings Overview

| Category | Commands | Typical Savings |
|----------|----------|-----------------|
| Tests | vitest, playwright, cargo test | 90-99% |
| Build | next, tsc, lint, prettier | 70-87% |
| Git | status, log, diff, add, commit | 59-80% |
| GitHub | gh pr, gh run, gh issue | 26-87% |
| Package Managers | pnpm, npm, npx | 70-90% |
| Files | ls, read, grep, find | 60-75% |
| Infrastructure | docker, kubectl | 85% |
| Network | curl, wget | 65-70% |

Overall average: **60-90% token reduction** on common development operations.
<!-- /rtk-instructions -->