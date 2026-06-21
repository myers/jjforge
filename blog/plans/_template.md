# Post plan: <post stem>

This file is a "write to think" artifact. It does **not** ship — Zola
doesn't render anything outside `blog/content/`, and that's
deliberate. Fill this out *before* writing the post itself. The
discipline is what counts; the file is just the place the discipline
happens.

When the post is committed, this plan stays here as a record of the
thinking that produced it. Future sessions reading the post can read
the plan alongside if they want to see what was on the author's
mind.

Delete the italic prompt under each heading once you've answered it.
A field that just contains the prompt is a field you haven't
answered.

---

## Story served

*Which `STORY-NN` from `blog/USER_STORIES.md` does the work this
post documents serve? State the id and the one-line goal. Most
posts serve one or two. Posts are about jjforge code (or research
that shaped it); every implementation-shaped post should ground in
at least one user-facing story.*

*Concrete example. A post about wiring the `jjf ready` command
serves:*

- *STORY-04 — pick up the next ready piece of work.*

*Research posts that don't ship code can name the story they're
moving the project toward.*

*If a post genuinely serves no listed story, that's a flag — either
the story belongs in `USER_STORIES.md` and should be added there
first, or the post is composition / refactor / scaffolding that
shouldn't ship (see `STYLE.md` "Per-issue post is a default, not a
requirement").*

## Opening

*The reader's foothold. One paragraph (3–5 sentences) that lands
the hook **and** the orientation together, above the fold. Pattern:*

> *"`jjf ready` is what we're working on. To do that we need to
> know which open issues have all their `blocked-by` dependencies
> closed. This post is about the query."*

*The first sentence is the hook (the command, the bytes, the
question, or the problem). The next sentences re-ground the reader
from that down to where the post actually sits, ending at what's
new in this post. The whole paragraph is the orientation; the post
body picks up from there without a separate "for a reader landing
cold" preamble.*

*Hook menu (vary across posts; if the last two posts opened with a
`jjf` command, pick something else this round):*

- *A `jjf` command the reader could imagine running.*
- *A snippet of repo state — a `git log refs/...` or `jj log` line,
  a transcript, a commit description — when the post is about the
  bytes.*
- *A short problem statement: "The hello-world prototype said we
  could write a commit on a bookmark without checking it out. It
  half-worked."*
- *A diagram (Mermaid) — when the diagram carries shape the prose
  can't.*
- *A question the post answers: "Why are we shelling out to `jj`
  instead of linking `jj-lib`?"*
- *A direct quote from upstream source or commit message, when
  the upstream decision is the post's load-bearing context.*

*Sketch the opening paragraph here. The exact wording can change
during drafting; what locks now is the **shape** — what's the
hook, what concepts does the orientation pass through, what does
it end on.*

**The issue id does not appear in the opening paragraph.** Default
is that issue id + link live in the Links section at the bottom of
the post (per `STYLE.md` "Issue ids live in the Links section").
The opening names the *work* — the merge driver, the storage
shape, the shell-out wrapper. The reflex *"the issue is X — Y"* is
exactly the phrasing the discipline is meant to prevent.

## Delta

*What does **this** post add to the story it serves? Be specific
about the delta: "the storage-shape decision had narrowed to
bookmark-branch; this post tests it under distributed-edit
pressure and discovers jj's auto-merge isn't enough." Three
sentences max. If you can't write the delta in three sentences,
the post probably has scope problems.*

## Posts to point back to

*Up to **three** prior posts that are load-bearing context for
this one. Not all of them — your editorial pick of which posts the
reader should know exist. Often one or two; three is the ceiling.*

*Critical discipline: **the link text must be what the reader
cares about, not the issue id.** `[the shell-out hello-world
post](...)` is right; `[380c4e2](...)` is wrong. The reader
doesn't know what `380c4e2` is and won't click. Three links read
as three doors; five read as zero.*

*Leave empty if the post genuinely doesn't continue from a prior
post — kickoff posts, brand-new sub-systems. An empty field is
honest; a padded one isn't.*

1.
2.
3.

## Dead-ends and detours

*The things you tried that didn't work, in order, with one-line
outcomes. Examples: a wrong API shape that didn't pan out, a test
that passed for the wrong reason and had to be redone, a misread
of upstream docs, a session-debugging path that turned out to be
a tooling issue, a first-pass design the second prototype
rejected.*

*Dead-ends are the single most educational part of a devlog post.
They live here in the plan because they come from the writer's
session memory — the issue and the commit don't always have them;
sometimes the dead-end happened in conversation or in a scratch
file. The writer is the only person who knows.*

*If the work genuinely had no dead-ends, write "clean first-try"
**and the post should say so explicitly**. An unsourced claim of
clean-first-try in prose, without surfacing the planning honesty
behind it, reads as a writing smell.*

## What I'm cutting

*The author's instinct will be to include "the full chain of why."
Resist. List the things you're tempted to write that **aren't
going to be in the post**, with one line each saying why you're
cutting them. Common cuts:*

- *Upstream jj history that doesn't change what the reader sees.*
- *Cross-references to issues the reader hasn't read.*
- *Project-internal organization references — "the prior three
  issues," "the issue each one is calling into," "design doc §7."
  Reader doesn't share these. Cite the work, not the bookkeeping.*
- *"Here's how I felt when I realized…" beats that don't teach.*
- *The third or fourth way of explaining the same point.*
- *Internal issue ids and short hashes in prose. Issue ids belong
  in the Links section; SHAs are written full when cited.*

*Naming the cuts before you write makes them stick. Half of "less
assumed context" is "less unnecessary context."*

## Outline

*Three to seven section headings, in order. Each one a sentence —
not a noun phrase like "The merge driver" but a small claim like
"The merge driver is the layer that turns conflict markers into
resolved bytes." The heading shouldn't tell the reader to expect a
section; it should tell the reader what they'll know after the
section.*

*If the outline doesn't land cleanly in three to seven beats, the
post is either too small (one or two beats — consider bundling
with the next post or shipping no post) or too large (eight+ beats
— split?).*

1. ...
2. ...
3. ...

## What landed in code

*The bullet-list of files / commits / experiments this post
documents. Won't all make the post — but listing them here forces
honesty about what the post is summarizing. If a code change isn't
in this list, it shouldn't appear in the post; if it's in this
list and *not* in the post, that's intentional context-trimming,
not omission.*

## Open questions for the writer pass

*Anything you couldn't decide while planning that the writing pass
will resolve. Format: question + the option you're leaning toward.*

*Empty is fine here — many posts plan cleanly without dangling
questions. Don't manufacture questions to fill the field.*
