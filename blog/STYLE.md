# Blog voice & style — jjforge

Source of truth for the jjforge devlog's voice, structure, and
conventions. Devlog posts under `blog/content/posts/` document the
project as it's built — one post per closed research issue, design
decision, or shipped piece of code.

This file stands on its own. The reviewer agent at
`.claude/agents/blog-post-reviewer.md` walks every post against the
rules below, plus the anti-AI-writing-pattern catalog in
`blog/tropes.md`.

`blog/WRITING.md` is the **process** companion to this file. STYLE.md
is *what good prose looks like*; WRITING.md is *how to produce it*
(plan first, draft second, review third). Read WRITING.md at the
start of writing a post; consult STYLE.md for the rules a sentence
or section has to satisfy.

The reader-anchor for posts is the user-story corpus at
`blog/USER_STORIES.md`. Every implementation-shaped post should
ground in at least one story; the planning template at
`blog/plans/_template.md` makes the link explicit before drafting
begins. Research and design posts that don't ship code can name the
story they're moving the project *toward*.

## Who writes it

Posts are co-authored by Claude (the assistant) and Myers (the
human). Claude does the typing: writing code, running commands,
reading bytes, debugging whatever crashed. Myers directs: he chooses
what to work on and asks "why?" until the explanation actually
explains something. Posts must reflect that honestly. When something
parses, Claude made it parse. When Myers "does" something, it usually
means he chose it or directed it, which is different from typing it.
Don't inflate Myers's code-writing share to make the posts sound more
hands-on.

Myers isn't a fictional naive interviewer. He's the real person
making directional decisions on this project. He asks the questions
he genuinely didn't know the answer to. If he already understood
something, don't invent confusion; let Claude just explain it in
prose.

## Audience

A working Rust developer who:

- Is comfortable with git and the command line.
- Has used a few coding agents (Claude Code, Codex, OpenCode) and has
  opinions about what works and doesn't.
- Has heard of jj but hasn't built anything on top of it.
- May or may not have used git-bug. Doesn't know the data model.
- Cares about agent-driven workflows but doesn't need them sold from
  scratch.

Don't condescend. Don't over-explain git basics. Do explain jj's
distinctives (operation log, change ids, fileset language) when the
post leans on them — most readers haven't internalized those yet.

Posts are devlogs *with* a teaching streak, not pure teaching
artifacts. The implication: spend words on the parts a reader
couldn't figure out from the code, and let the code speak for
itself elsewhere.

## Dialogue conventions

When a post includes a Claude↔Myers exchange, use plain markdown
with speaker labels:

```markdown
**Myers:** Question here.

**Claude:** Answer here.
```

(jjforge doesn't ship Zola shortcodes the way zfs-workspace does.
Plain markdown is fine for now; if we add a site generator later,
this is the migration point.)

Plain `> ...` blockquotes are still the right tool for third-party
quotes (RFC excerpts, upstream commit messages, jj docs). Keep the
speaker-label form for the Claude↔Myers interview format.

## Form: dialogue when useful, prose otherwise

A post can lean dialogue or prose, and a given post might use both.

- **Dialogue** — direct quotes from Myers, framed by Claude's
  questions and commentary. Good when Myers's real words say
  something better than any paraphrase would, or when the
  conversation itself is the story.
- **Prose narration** — Claude writing about what happened, what it
  meant, and what's next. Good when the story is mostly about code,
  mechanics, or explanation.

Default is prose. Drop into dialogue when it earns its keep:

- **Surfacing confusion.** A moment where Myers genuinely got stuck
  is more honest as an exchange than as prose.
- **Breaking up density.** A long stretch of exposition benefits from
  a question that forces a restatement in plainer terms.
- **Showing what didn't work.** "We tried X. It broke." lands better
  as Myers reporting and Claude reasoning about why.

Don't force dialogue. A post that's 100% prose is fine if the
material doesn't have a natural "stuck moment" to dramatize.

## Per-issue post is a default, not a requirement

The default is one post per substantive issue or decision arc.
**The default is not a mandate.** An issue whose only contribution
is composition, refactor, or scaffolding is allowed — and
*encouraged* — to ship without a post. The next post that *does*
introduce real new content names the milestone in passing.

If a post would have nothing to teach a reader that the commit
message doesn't already cover, don't ship the post.

## What a post must cover

When a post does ship, it must cover:

1. **What we were trying to do** — one paragraph of context. Name
   the *work*, not the issue id. (See "Issue ids live in the Links
   section" below.)
2. **Why it mattered** — the reader should understand why this piece
   is load-bearing for the project, not just that it happened.
3. **What we tried that didn't work** — dead ends are the most
   educational part. Include them. If the post is a clean first-try
   with no dead ends, say so explicitly.
4. **What ended up working** — and why.
5. **Links section at the bottom** — the issue file (if any), the
   commit(s) (full SHA, not short), and the upstream sources cited.

## Issue ids live in the Links section

The reader doesn't share the project's bookkeeping. They didn't pick
the issue to read; they picked the post. So:

- **Default: the issue id and link both live in the Links section
  at the bottom.** The opening paragraph names *the work* — the
  merge driver, the shell-out wrapper, the storage shape — never
  the issue id.
- **Fallback: in-prose issue references are reserved for cases where
  they earn it.** A closing post disambiguating what *this* multi-
  issue arc shipped; a retrospective naming a specific failed prior
  attempt the post corrects. When an in-prose reference is genuinely
  necessary, the first mention must be glossed (one-line summary of
  what the issue did), and the link text must describe the work
  (not the bare id).

The reflex "this post is for issue dcd4b57, so the opening should
say 'The issue is dcd4b57 — storage shape'" is the exact pattern
these rules exist to prevent. The opening should be the same
whether an issue exists or not.

## Register

- First person plural ("we") for shared actions: *we picked
  shell-out*, *we filed e2e473b after the test*.
- First person singular inside dialogue lines, attributed to the
  speaker.
- Past tense for narrative of what happened; present tense for how
  things work. (*"We wrote the wrapper. `jj` resolves file paths
  against `cwd`, not the repo root."*)
- Contractions are fine.
- No emoji unless the reader would miss the joke without them.

## Tone

Plain. Bold project, humble prose. Avoid superlatives ("definitive,"
"the only," "elegant," "powerful"). State the work and let it
speak. Avoid breathless framing of routine engineering. The reviewer
flags humble-tone violations.

## Sourcing claims

When a post makes a factual claim about how the system works or why
something was written a particular way, show the source. Three
shapes:

- **Link a prior post or commit** that established the fact. Use
  full SHAs; short hashes rot.
- **Show a log line, source snippet, or command output** that proves
  it, inline.
- **Write it as direct observation**, naming how you observed: *"I
  ran `cargo tree --depth=2 | wc -l` against the shell-out example
  and got 12."*

When you don't know, say so or go find the source before claiming.
Unsourced speculation — *"the reason appears to be historical"*,
*"probably because jj used to do X"*, *"which might have been true
in some older version"* — is a writing smell. A future session
re-reading the post will end up quoting the guess as fact. When in
doubt, err toward fewer claims, better grounded.

The test: for every non-obvious factual sentence, could a reader
walk it back to *something* — a prior post, a commit, a log, an
observation? If not, find the source or cut the sentence.

## Topic and scope

This blog narrates building **jjforge**, a jj-native, agent-first
issue tracker. CLI is `jjf`. Posts cover the research, design, and
shipping work as it happens, not retroactively.

Issue ids in this project are jjforge ids — 7-character lowercase
hex strings like `2130de1`, `dcd4b57`. They're stable across the
issue's life. (Pre-cutover posts used git-bug ids in the same
shape; that's intentional — same 7-hex format, different storage
substrate.) When a post in prose has to name one (rare; see "Issue
ids live in the Links section"), the first mention gets a one-line
gloss describing what the issue was about.

## Code-fence languages

Use the actual language id (`rust`, `bash`, `text`, `json`, …) for
syntax highlighting where it helps. Use `text` or `txt` for plain
output (shell transcripts, log dumps, ASCII tables).

## Creating a new post

Use `scripts/new-blog-post.py "<Human Title>"` from the project
root. It writes `blog/content/posts/YYYY-MM-DD-<slug>.md` with
the current local ISO-8601 timestamp in the `date` frontmatter,
creates the matching `blog/static/img/YYYY-MM-DD-<slug>/`
directory, and stamps the planning sibling at
`blog/plans/YYYY-MM-DD-<slug>.md` from `blog/plans/_template.md`.

Don't hand-edit the date to be in the future; the timestamp should
reflect when the post was actually written so it stays close to its
commit time. The default `authors = ["Claude"]` should be widened
to `["Claude", "Myers"]` before saving when the post is co-authored.

## Reviewer agent

A `blog-post-reviewer` agent at `.claude/agents/blog-post-reviewer.md`
reviews every post before commit. It reads this file, `tropes.md`,
`USER_STORIES.md`, and the post's planning sibling, then walks the
post against every rule. The dispatching agent fixes whatever the
reviewer flags before committing.

## Anti-patterns

- **Fake-naive Myers.** Don't put words in Myers's mouth that real
  Myers wouldn't say. If you're unsure whether Myers was confused
  about something, write it as prose instead.
- **Claude as oracle.** Claude is the explainer, not infallible.
  When Claude was wrong during the work (bad hypothesis, wrong
  tool, missed a flag), say so in the post. Claude learning is
  part of the pedagogy.
- **Assuming the reader remembers the last post.** Each post stands
  alone. Re-introduce jargon, re-link the issue, re-state the goal.
- **Burying the outcome.** The top of the post should make clear
  what was accomplished. Don't make the reader scroll to find out
  if it worked.
- **"Obvious in hindsight."** If something was obvious in
  hindsight, it was not obvious while you were doing it. Write
  from inside the confusion, not from after it.
- **Bare issue ids in prose.** When a post does mention an issue
  in prose (rare), the first mention gets a one-line gloss. An
  ungloss first mention is **Important**; a bare id in the opening
  paragraph is also **Important** unless the post has a real
  reason to name it there.
- **Time-language that doesn't match the dates.** "Last week",
  "today's issue", "earlier this session" all need to agree with
  the post's frontmatter dates. Cross-check before shipping.
- **Placeholders left in the body.** `[TODO]`, `[FIXME]`,
  `[banner goes here]` must never ship.
- **Issue ids as link text.** `[dcd4b57](...)` is wrong; the
  reader doesn't know what `dcd4b57` is and won't click. Link text
  should be **the work**: `[the storage-shape decision](...)`.
  Never make the issue id itself the clickable text.
- **Talking about the post itself.** *"That's the whole shape of
  the post"*, *"This section is about…"*, *"In this post we'll
  see…"*, *"The post is short because the issue was."* The reader
  is already inside the post.
- **Project-internal organization as content.** *"…with the issue
  each one is calling into"*, *"the prior three issues…"*. Cite
  the work, not the bookkeeping.
- **Workarounds without a follow-up.** If the post documents a
  workaround — a shell-out hack instead of a proper API call, a
  test that doesn't really cover the case, a known-incomplete
  implementation — the post must name the open issue id that
  fixes it. No silent dodging.

A separate file, `blog/tropes.md`, catalogs the AI-writing-pattern
anti-patterns. Read it before writing posts. Read it again. Then
write the post. The reviewer agent flags hits.
