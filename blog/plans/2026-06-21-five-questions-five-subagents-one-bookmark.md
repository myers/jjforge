# Post plan: 2026-06-21-five-questions-five-subagents-one-bookmark

---

## Story served

STORY-00 — know which technology stack jjforge is built on.

This is a kickoff post. It serves the reader who lands on the
blog cold and wants to understand the decisions that shape
everything else: why jj, why shell out, why a bookmark for
storage, why a merge driver matters. It moves the project toward
STORY-09 (work on a project from two machines without losing
edits) by naming what we found that prevents naive cross-machine
edits from converging.

## Opening

The hook is the *idea*: dispatching five subagents in serial to
research the load-bearing questions, with each agent's findings
shaping the next agent's prompt. The orientation re-grounds from
that to what we actually decided.

Sketch:

> Five questions blocked any honest answer to "what does jjforge
> look like." Yesterday we filed them as git-bug issues in a brand
> new repo, then handed them to five subagents in serial. Each
> one closed its issue with a verdict that reshaped the next
> agent's prompt. By the end we had a tool sketched in enough
> detail to start building it, and one new issue we hadn't
> anticipated: jj's automatic merge isn't sufficient for bug
> data.

The issue ids do not appear in the opening.

## Delta

This post adds the project's first written explanation of *what
jjforge is*, plus the four research verdicts that pin the design:
shell out to `jj`, bookmark-branch storage, `Jjf-Op:` trailers as
the audit log, and a merge driver as the next concrete piece of
work. The reader who finishes this post has the same model the
authors have today.

## Posts to point back to

(none — kickoff post.)

1.
2.
3.

## Dead-ends and detours

- Considered storing bugs in markdown files on `main` like
  zfs-workspace does. Ruled out because bug-edit races would
  couple to code merges; reviewed under `dcd4b57`.
- Considered side-channel storage attached to jj operations.
  Mooted because jj's operation store is local-only and isn't
  carried by `jj git push` — confirmed empirically (alice had 10
  ops, bob saw 3 after fetch).
- Assumed jj's automatic conflict resolution would handle two
  agents editing the same bug field on different clones. It
  doesn't, unless the edits land on byte-identical content or
  separated lines. Surfaced by `8d3e045`.
- Assumed `jj op log` would be the per-bug audit surface. It
  isn't — wrong granularity, local-only, can't path-filter.
  Replaced with `jj log <path> -T 'json(self)'` against the
  embedded trailers.

## What I'm cutting

- The full backstory of how we got to jj as the substrate (came
  out of a different conversation thread).
- A detailed comparison with Beads — that lives in
  `notes/2026-06-21-beads-vs-git-bug.md`, doesn't fit a kickoff
  post.
- The story of writing the subagent skill that made the
  dispatches work — interesting but its own post.
- A walkthrough of each of the five issues' findings — the
  consolidated verdict is what the reader wants, not five mini
  retellings.
- The "Claude isn't perfect, so my stuff doesn't have to be"
  framing line — true and load-bearing for the project, but a
  reader-facing post is the wrong place for project meta.

## Outline

1. Five subagents, in serial, on a fresh repo.
2. Shell out to `jj`, don't link `jj-lib`.
3. Bugs as commits on a dedicated `bugs` bookmark.
4. `Jjf-Op:` trailers carry the audit log; `jj log <path>` reads
   it.
5. The merge driver is the surprise that wasn't on the original
   list.
6. What's next.

## What landed in code

- `~/p/jjforge/README.md` (`6739b37`)
- `~/p/jjforge/bin/jjf` (`d95f083`)
- Five research issues filed via `git-bug` and closed-or-acted-on
  through the run.
- `experiments/jj-cli-overhead/`.
- `experiments/op-log/`.
- `experiments/jj-shellout-hello/`.
- `experiments/storage-shape/`.
- `experiments/distributed-edit/`.
- New follow-up issue `e2e473b` for the merge driver.

## Open questions for the writer pass

- How much should the post explain what git-bug is? The audience
  has heard of it. Leaning: one sentence, link to the project for
  more.
  - Resolved at draft: linked once in the opening paragraph along
    with Beads and jj, no further explanation.
- Should the post show the four-section recipe the subagents
  used? Leaning: no — that's a skill-and-process story, separate
  post.
  - Resolved at draft: the four-section heading list (Findings,
    Recommendation, Confidence, Open follow-ups) appears once;
    the mechanics live in a future post.

## Reviewer-pass changes (post-draft)

Applied after the blog-post-reviewer agent ran:

- "Yesterday we filed them" → "This morning we filed them" (M5
  date-language fix; everything happened on 2026-06-21).
- Bold-first bullet lists (two of them) rewritten as prose
  paragraphs (tropes.md / Bold-First Bullets).
- "works beautifully" → "the markdown is the data"; "would have
  been a beautiful answer" → "the cheapest possible design"
  (STYLE.md tone: no superlatives).
- Em-dash density reduced from 17 in prose to 0 (tropes.md /
  Em-Dash Addiction). Replaced with commas, parens, colons, and
  separate sentences as appropriate.
- "made this worse, not better" → "aggravates this"; "It isn't
  the answer." / "That isn't how it turned out." removed as
  punchy one-line paragraphs.
- gg's "monthly catch-up schedule" claim re-sourced from the
  GitHub project page (832 stars, 30 releases, latest v0.39.1).
- "operation log" first mention now glossed in parens.
- Skill-and-discipline paragraph trimmed (cut named it; cut
  partially failed at first draft).
- Closing paragraph "the day after picking the substrate" → "on
  the first day" (time-language).
- Tracker-bookkeeping mention in "What's next" trimmed.
