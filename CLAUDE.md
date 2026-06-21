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

## What's next

Read the meta-epic `04e1dac` and pick the next concrete piece
of work. The MVP path is `mvp-storage` → `mvp-cli` → `mvp-sync`
(`e2e473b` is the first ticket under `mvp-sync` and the only
"do this now" concrete ticket currently filed).

Don't expand scope into the speculative epics
(`multi-client`, `project-agent-orchestration`) until the MVP
epics ship. Their sketches are intentionally rough.
