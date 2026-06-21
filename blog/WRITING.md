# How to write a jjforge blog post

The order of operations. Steps 1–3 are planning (stamp, plan,
checkpoint the opening). Steps 4–5 are writing. Step 6 is review.
Each step points at the file that holds the canonical rules — read
those at that step, not before.

## 1. Stamp the artifacts

Run `scripts/new-blog-post.py "<Title>"`. It creates:

- The post stub at `blog/content/posts/YYYY-MM-DD-<slug>.md`.
- The image directory at `blog/static/img/YYYY-MM-DD-<slug>/`.
- The planning sibling at `blog/plans/YYYY-MM-DD-<slug>.md`.

Don't write prose yet.

## 2. Fill out the planning sibling

Open `blog/plans/<stem>.md`. Each field has its own discipline-prompt
in italics; the prompts are the rules. The shape:

- **Story served** — which `STORY-NN` from `USER_STORIES.md` does
  the work serve.
- **Opening** — the reader's foothold. Hook + orientation in one
  paragraph, above the fold. (This is the field most worth
  iterating; see "Iterate" below.)
- **Delta** — what *this* post adds, three sentences max.
- **Posts to point back to** — up to three prior posts. Link text
  is the work, never the issue id.
- **Dead-ends and detours** — things you tried that didn't work.
  Filled from session memory; the issue and commit don't always
  carry these. Single most educational part of a devlog post.
- **What I'm cutting** — temptations you're refusing to write.
- **Outline** — three to seven section headings, each a small
  claim.
- **What landed in code** — files / commits this post documents.
- **Open questions for the writer pass** — unresolved decisions.

Empty fields are fine when honestly empty. A field that just
contains its italic prompt is a field you haven't answered.

### Iterate the plan until it coheres

The plan is **not done** when every field has a non-italic answer.
The plan is done when the fields cohere *with each other*. After a
first pass, read the plan back end-to-end and ask:

- **Story → Opening alignment.** Does the opening's hook actually
  surface this post's work?
- **Delta → Outline alignment.** Does each outline beat advance
  the delta, or are some beats off-topic?
- **Outline length.** Three to seven beats. If you wrote nine, two
  are subsections of one or the post is too big.
- **Dead-ends honesty.** Did you list the dead-ends from the
  session, or skip the field? If you wrote "clean first-try," is
  that actually true?
- **What I'm cutting honesty.** Did you list the things you're
  *actually* tempted to write, or just the things easy to cut?
  The cuts that matter hurt.
- **Hook freshness.** Is this opening's hook redundant with the
  previous one or two posts?
- **Posts to point back to discipline.** Are these the
  load-bearing prior posts or just the most recent? Is each link
  text *the work*, never the issue id?

Revise. Plans that read clean on the first pass usually weren't
ambitious enough to need revising.

The plan is ready when reading it end-to-end produces no flinches.

### When *Story served* stays empty

If *Story served* stays empty after honest effort, the right
answer may be **no post**. Per STYLE.md "Per-issue post is a
default, not a requirement," composition issues, refactors, and
issues that introduce no new concept the reader can hold onto are
*encouraged* to ship without a post. The next post that *does*
introduce real new content can name the milestone in passing.
Skipping is honest; forcing a thin post is not.

## 3. Re-read the *Opening* (checkpoint)

This is a checkpoint, not a fresh action. Re-read the opening cold:

- Does it hook *and* orient in one paragraph, above the fold?
- Does the hook vary from the last one or two posts?
- Does the link text in any embedded link describe **the work**,
  not the issue id?

If any of those bounce, fix them in the plan first. Don't carry an
opening you already know is wrong into drafting.

`STYLE.md` "Anti-patterns" applies the moment the prose starts —
re-skim it before drafting if you haven't recently.

## 4. Draft the prose

Read `STYLE.md` end-to-end before drafting. The rules that bite
hardest mid-draft:

- **Audience** — working Rust dev comfortable with git, new to jj.
- **Sourcing** — every non-obvious factual claim walks back to a
  prior post, a commit, a log, or a direct observation.
- **Tone** — plain. No superlatives.
- **Anti-patterns** — full catalog. Read it.

Read `tropes.md` before drafting — the AI-writing patterns it
catalogs keep slipping in.

Each section of the post should land one outline beat from the
plan. When a paragraph starts feeling familiar, check whether it's
something you already named in *What I'm cutting*.

## 5. Cross-check, then revise the plan to match

When the prose is in shape, read it alongside the plan:

- Did every outline beat land in a section?
- Did every cut on *What I'm cutting* stay cut?
- Did the post add a section not in the outline? If so: earned
  its place, or scope creep?
- Did the delta still describe what the post delivered, or did
  the post drift?
- Did the hook you committed to in §3 still land at the top of
  the prose?

**The cross-check is bidirectional.** Sometimes the prose is wrong
and needs to match the plan; sometimes drafting surfaced something
the plan missed and the *plan* needs to match the prose. Either is
fine. What's not fine is leaving the two out of sync.

When in doubt: the prose is what ships, but the plan is the
record of what the writer thought they were doing. Updating the
plan to reflect what they *actually* did is honest. Pretending
the first draft of the plan was right is not.

## 6. Run the reviewer

Dispatch the `blog-post-reviewer` agent. It reads the canonical
files and the post's plan, then walks the post against every rule.
Returns a Critical / Important / Suggestion report.

Apply the fixes. Re-dispatch only if the changes are substantive
(new sections added/removed, not single-line copy edits).
