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

- **Status:** scoping. No Rust binary yet; `bin/jjf` is a shell
  shim around `git-bug` so we can plan jjforge with the same
  verb shape we want the eventual binary to have.
- **Planning surface:** `git-bug` issues in this repo. Plans
  live in the issues, not in markdown files. The blog
  (`blog/content/posts/`) captures milestones for the public
  record.
- **Entry point:** the meta-epic `04e1dac`. Read it first.
- **CI:** `.woodpecker/blog.yaml` builds and pushes a Zola site
  image. Mirrors zfs-workspace's pattern except for the
  notify-flux hook (jjforge isn't a Flux deployment target).

## How to use git-bug (before we replace it with our own)

We are dogfooding `git-bug` as the planner for the jj-native
tracker that will eventually replace it. Treat every rough edge
as data, not as something to fix in the planner.

### Entry point and discovery

The meta-epic at `04e1dac` is the orientation document. Start
there in any new session before touching anything else:

```bash
git-bug bug show 04e1dac
```

To navigate from there:

```bash
git-bug bug                                # everything, oldest first
git-bug bug --label meta-epic              # just the entry point
git-bug bug --label epic                   # the six top-level epics
git-bug bug --label epic:mvp-storage       # one epic + its related issues
git-bug bug --label research               # historical research
git-bug bug --status open                  # filter by status
git-bug bug show <prefix>                  # one issue, 7-char prefix is enough
```

### Label scheme

- **`meta-epic`** — the entry-point issue (one only).
- **`epic`** — the six top-level epic issues. Each carries the
  goal, the sketched approach, the tickets we expect to file
  under it, and its dependency graph.
- **`epic:<slug>`** — every issue belonging to an epic (the epic
  itself, plus research issues that informed it, plus child
  tickets when they're filed). Use the colon-prefixed form
  always; bare `<slug>` labels were tried briefly and removed.
- **`research`** — historical research record. The five
  research issues filed and worked on 2026-06-21 pinned the
  load-bearing decisions; they're closed, but their closing
  comments are the source of truth for the verdicts the epics
  reference.

### Creating a new bug

Use stdin for multi-line bodies. The interactive flow is off-limits
for agents.

```bash
cat <<'EOF' | git-bug bug new --non-interactive --title "Real title goes here" --file -
# Goal

What does done look like.

# Approach

How we get there.
EOF
```

**Important gotcha.** The `--title` flag is silently ignored when
`--file -` is also given — git-bug takes the first line of the
body as the title instead. The two-step pattern that works:

```bash
# Step 1: create with placeholder title
cat <<'EOF' | git-bug bug new --non-interactive --title "x" --file -
# Goal
...
EOF
# Step 2: capture the printed id and edit the title
ID=<from-stdout-of-step-1>
git-bug bug title edit "$ID" -t "Real title goes here" --non-interactive
```

### Capturing a newly-created bug's id

`git-bug bug new` prints the new id to stdout on a line like
`<id> created`. **Capture from that output.** Do NOT use:

```bash
# WRONG: returns the first id in the list (oldest), not newest
ID=$(git-bug bug -f id | head -1)
```

This footgun bit us when filing the meta-epic — we accidentally
overwrote the title of an unrelated existing issue. Always
capture the printed id:

```bash
CREATE_OUT=$(cat <<'EOF' | git-bug bug new --non-interactive --title "x" --file -
...
EOF
)
ID=$(echo "$CREATE_OUT" | awk '/created$/ {print $1}')
```

### Updating bugs

```bash
git-bug bug title edit <id> -t "New title" --non-interactive
git-bug bug status close <id>
git-bug bug status open <id>
git-bug bug label new <id> <label>           # add a label
git-bug bug label rm <id> <label>            # remove a label
git-bug bug comment new <id> --non-interactive --file -    # body on stdin
```

### Bodies vs comments — the editing limitation

**`git-bug` has no "edit body" command.** The original body is
the first comment; every subsequent update is an appended
comment. Implications:

- If you need to revise an epic's plan after filing it, post a
  follow-up comment. The original goal statement stays put.
  This is one of the things jjforge should improve on.
- For the meta-epic specifically: the body is the placeholder
  goal sentence; the populated epic index lives as comment #1.
  Both are visible in `bug show`, but the chronology can
  confuse a reader. State the comment-#1 location explicitly
  when pointing someone at the meta-epic.

### Issue-id length

`git-bug` accepts any unambiguous prefix. The convention in this
repo is to use the **7-character prefix** (e.g. `04e1dac`,
`72638a0`) in prose, just like git short SHAs. Full hex ids
appear only in `git-bug bug show` headers and when collision
risk matters (which is essentially never at this scale).

### Push / pull

`git-bug push` and `git-bug pull` round-trip the issue data via
the `refs/bugs/*` namespace. No remote is configured on this
repo yet, so don't try to push.

### Wiping

`git-bug wipe` deletes all git-bug data from the repo. Catastrophic.
Never run it without explicit user approval.

## Subagent discipline

When dispatching a subagent to work an issue, the
**`subagent-working-a-git-bug-issue`** skill auto-loads. It
enforces:

- The closing comment uses the four-section recipe: Findings,
  Recommendation, Confidence, Open follow-ups.
- The agent closes the issue when work is complete, or leaves it
  open with the Findings explaining why.
- The agent does not edit the original body, does not touch other
  issues unless cross-link is warranted, and does not push.
- The closing return-value to the orchestrator is under 200
  words.

If you're dispatching subagents and the skill isn't loading, name
it explicitly in the dispatch prompt — it should auto-load on
"git-bug" or "issue" keywords but isn't guaranteed.

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
- Don't push to remote (none configured).

## Orchestrating work

When the user asks you to "orchestrate" or "make progress" or
"dispatch subagents," the loop is:

1. **Read the meta-epic `04e1dac` first** to orient.

2. **Find the next concrete ticket.** Either there's a named
   "do this now" ticket (today: `e2e473b`, the merge driver) or
   there isn't. If there is one, work backward: are its
   prerequisites filed and viable, or do they need detailing
   first?

3. **File any missing prerequisite tickets yourself before
   dispatching subagents.** The orchestrator owns the ticket
   graph. Subagents own the work inside one ticket. A subagent
   asked to "build X and file the ticket for it" will either
   skip the ticket or write it badly. Pre-file with a sketched
   body; let the subagent close it.

4. **Dispatch serially, not in parallel.** Subagents writing to
   `refs/bugs/*` race each other — git-bug's underlying refs
   aren't atomic across processes. The earlier session learned
   this the hard way. Parallel is fine ONLY when the subagents
   have disjoint write targets (different bug ids, different
   files in `experiments/<topic>/`). When in doubt, serial.

5. **Commit between dispatches.** Each subagent's experiments,
   docs, or other artifacts get committed before the next is
   dispatched. The next agent reads a clean tree; commit
   messages double as a worklog. Use the explicit-filename
   discipline from the Commits section.

6. **Run the workspace tests between dispatches.** After
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

7. **Post a status comment to each affected epic** when a child
   ticket closes. The comment goes on the epic issue (e.g.
   `72638a0`), names the closed ticket, links the commit if one
   landed, and notes what's still unfiled. **When the orchestration
   round ends, also post a status comment to the meta-epic
   `04e1dac`** linking any newly-filed or newly-closed tickets —
   epic-level comments aren't enough; the meta-epic is the index
   readers (and future orchestrators) hit first.

8. **Surface follow-ups to the user.** Stop and report when:
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

`git-bug` data lives in `refs/bugs/*` and is shared across
worktrees automatically; bug edits travel between them. Code
and experiments do not.

### Dispatch prompt template

When dispatching a subagent on an issue, include:

- The issue id and the command to view it
  (`cd ~/p/jjforge && git-bug bug show <id>`).
- A one-sentence summary of why this work is happening now
  (which epic, what's blocked on it).
- Pointers to prior-subagent findings as paths or issue ids
  (`see ~/p/jjforge/experiments/.../README.md` or
  `the closing comment on <prior-id> records the verdict`).
- The housekeeping note about stripping nested `.git/`/`.jj/`
  from experiment dirs.
- An explicit "report back under 200 words" cap.

The `subagent-working-a-git-bug-issue` skill auto-loads from
keywords ("git-bug", "issue") and enforces the four-section
closing-comment recipe. Don't re-explain that in the dispatch
prompt; let the skill carry it.

## What's next

The project's running roadmap is a single ticket labeled
`roadmap`:

```bash
git-bug bug --label roadmap
```

It lists the open epics in the order they should be tackled.
The order shifts as the project learns; the ticket itself
stays open for the life of the project. The latest comment
is the truth (git-bug has no edit-body command; the roadmap
gets updated by appending a new ordering as a comment).

For the broader index — every issue, open and closed, by
label — read the meta-epic `04e1dac`. The roadmap tells you
*what to work on next*; the meta-epic tells you *what
exists*.

Don't expand scope into epics below the roadmap's "above the
line" cut until the roadmap explicitly pulls them up.
