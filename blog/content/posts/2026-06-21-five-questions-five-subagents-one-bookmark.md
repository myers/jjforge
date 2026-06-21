+++
title = "Five questions, five subagents, one bookmark"
date = 2026-06-21T14:15:45-04:00
authors = ["Claude", "Myers"]
+++

Five questions blocked any honest answer to "what does jjforge look
like." This morning we filed them as issues in a brand new repo,
then handed them to five subagents in serial. Each one closed its
issue with a verdict that reshaped the next agent's prompt. By the
end we had the tool sketched in enough detail to start building
it, and one new issue we hadn't anticipated: jj's automatic merge
isn't sufficient for bug data.

<!-- more -->

This is the first post on this blog. **jjforge** is a jj-native,
agent-first issue tracker we're building in Rust. The CLI is
`jjf`. The shape we want: issues live in your git (well, jj) repo
alongside the code, an agent can `read`/`edit`/`grep` them the way
it reads code, and the whole thing is fast enough that an agent
loop can drive it without flinching. The pitch is borrowed
roughly: the operational ergonomics come from [Beads], the
data-in-the-repo posture comes from [git-bug], the substrate
comes from [jj], and the markdown-in-the-repo writing convention
comes from a Rust port of ZFS we run in a parallel workspace.

[Beads]: https://github.com/steveyegge/beads
[git-bug]: https://github.com/git-bug/git-bug
[jj]: https://github.com/jj-vcs/jj

We didn't sit down and design any of this on the first try. Five
questions had to be answered before we could even start writing
Rust: should we link `jj-lib` or shell out to the CLI? What does
the smallest reasonable Rust example using jj look like? Is the
jj operation log enough for `jjf`'s audit needs? Where exactly do
bugs live in a jj repo? What does jj actually do when two clones
edit the same bug? Each of those answers shapes the others. None
of them are guessable.

## Five subagents, in serial, on a fresh repo

The play: file each question as an issue in a fresh `jjforge`
repo, using `git-bug` as the planning tracker. Then dispatch a
subagent on each issue in dependency order, in serial. Each agent
gets the closing comments of the prior agents as context, so by
the time the fourth agent reads "should we link `jj-lib`?" that
question is already settled.

We're using `git-bug` here as a deliberate exercise in
eating-our-own-style-of-dogfood: planning a jj-native issue
tracker inside a git-based issue tracker. If git-bug's ergonomics
hurt during planning, that tells us what jjforge should improve
on, in detail, from real friction.

The dispatch flow is enforced by a small skill the dispatcher
loads automatically. It tells each subagent that the issue's
closing comment is the deliverable, what shape the comment must
take (Findings, Recommendation, Confidence, Open follow-ups),
and how to mark the issue closed. That skill is its own post.

## Shell out to `jj`, don't link `jj-lib`

The first question was the most expensive to get wrong. jj
exposes its functionality two ways: a library (`jj-lib`) you can
link into a Rust binary, and a CLI (`jj`) you can call as a
subprocess. The library is faster and more direct. The CLI is
slower and constrains you to whatever the CLI surfaces. The
question is which cost you'd rather pay.

The first subagent went looking for evidence. Upstream's own
architecture docs explicitly disclaim API stability: *"not much
has gone into … which symbols are exposed in the API."* The
roadmap names an "RPC API" item for non-Rust embedders, but no
timeline. And `gg`, the most-developed [Tauri GUI for jj][gg]
(832 stars when I checked, with 30 releases over the project's
lifetime and the latest at v0.39.1), is on a release schedule
roughly synchronized to jj's. Any project that links `jj-lib`
and writes to the repo signs up for that same schedule.

[gg]: https://github.com/gulbanana/gg

The CLI side measured better than we feared: ~14.6 ms per `jj`
invocation in a 1000-iteration benchmark we ran in
`experiments/jj-cli-overhead/`. For an agent loop, that's
invisible. The dependency-graph comparison was even more
lopsided: shelling out, the prototype pulls 12 transitive crates.
Linking, it pulls 193.

So we shell out. We revisit when jj ships 1.0 or the RPC API
lands, whichever comes first. Until then, the CLI is jj's stable
contract: they document it, they note breaking changes in the
changelog, they treat it as a public interface. The library is
internal.

## Bugs as commits on a dedicated `bugs` bookmark

The next question was where, exactly, the bug data lives. Three
candidates were on the table.

The first: bugs live as markdown files in an `issues/` directory
on `main`, alongside the code. This is the zfs-workspace
convention. It works for a solo-dev port: issues and code travel
together, the agent reads them with `cat`, the markdown is the
data.

The second: bugs live on a dedicated bookmark (jj's name for a
named ref, roughly analogous to a git branch you might call
`issues@`). Bug files live there; the working copy on `main`
doesn't see them.

The third: bugs attach to jj operations as side-channel data, as
custom metadata on the operation log. (The operation log is jj's
per-repo record of every action that mutated repo state,
distinct from the commit history.)

The third one died first. jj's operation log lives in
`.jj/repo/op_store/` and is **local-only**: it isn't carried by
`jj git push` or `jj fetch`. We confirmed this empirically in
`experiments/op-log/`, where one clone made 10 operations,
fetched into a second clone, and the second clone saw 3.
Whatever bug data we attach to operations stays on the machine
that wrote it. That's the wrong durability story for a tool
whose point is to keep project state in the repo.

The first option lost on blast radius. If bugs live in
`issues/*.md` on `main`, then an agent editing a bug and an agent
editing the code can both produce concurrent commits on `main`,
and the merge of those commits is the merge of *both*: bug
changes get tangled with code changes whenever two clones write
in parallel. That's especially painful for the case we care
about, a project-agent firing off subagents that each work on
different issues, each committing to their own jj working copy.
The fewer things share a merge surface, the easier the
distributed story is.

So the bookmark option wins. Bugs live as commits on a
`bugs` bookmark; the working copy on `main` is unaffected. When
you `jj git push`, both bookmarks go to the remote.

## `Jjf-Op:` trailers carry the audit log

The next question was where the per-bug history lives. git-bug
solves this by storing every change as a typed operation
(`SetTitleOp`, `AddCommentOp`, `SetStatusOp`, …) replayed in
deterministic order. We need something equivalent: an agent
looking at a bug should be able to ask "how did this bug get to
its current state" and get a structured answer.

The obvious option was `jj op log`. It exists, it's there for
free, and on a Rust-shaped repo it's already useful. So we tried
it as the audit surface. It failed three different ways. First,
the granularity is wrong: `jj op` descriptions are jj-flavored
(*"snapshot working copy"*, *"new empty commit"*), and the
jjforge-shaped intent ("alice closed bug 42 with reason X") lands
on the commit description, not the op. Second, the op log is
local-only, the same problem that killed the side-channel storage
idea. Third, you can't filter ops by path: `jj op log <path>`
errors out. You can ask "what happened in the repo" but not "what
happened to this one file."

The answer turned out to be embedded trailers in the commit
description, plus `jj log <path>` (not `jj op log`) to read them
back. Each commit on the `bugs` bookmark carries one or more
trailer lines like:

```text
Jjf-Op: set-status {"from": "open", "to": "closed", "reason": "..."}
```

Trailers survive `jj describe`. They round-trip through `jj git
push`. They're machine-readable with `jj log <path> -T 'json(self)'`,
which emits the commit metadata as structured JSON we can parse
for the trailers. The shell-out prototype in
`experiments/jj-shellout-hello/` proves the round trip works end
to end.

## The merge driver is the surprise

The last question was the one we expected to confirm something
nice. When two clones edit the same bug while offline, does jj's
automatic conflict resolution do the right thing? The hope was
yes. jj's merge model is better than git's, and "let jj handle
it" would have been the cheapest possible design. It didn't pan
out that way.

The last subagent built a two-clone test harness in
`experiments/distributed-edit/` and ran every scenario the issue
called for, plus a few. The result: jj's content-merge only
succeeds cleanly when both clones write byte-identical bytes
(idempotent edits, both closing the bug with the same body), or
when the two edits are separated by unchanged padding lines. As
soon as two agents touch nearby lines in the same bug file (title
vs status, status vs a new comment) jj produces content conflicts.

The bug-file format we picked aggravates this. Each bug is a
small JSON file; status, title, assignee, and comments all sit
within a few lines of each other. The natural single-line-per-key
layout puts two fields exactly one line apart. That's well inside
the distance where jj's merger says *"both of you changed this
region, I'm out."*

The good news is jj's conflict markers are recoverable. They use
a regular grammar (`<<<<<<<`, `+++++++`, `%%%%%%%`, `>>>>>>>`),
and the experiment showed that a ~30-line script can parse them,
pick a winner per field by policy (last-write-wins for scalars,
set-union for arrays, append-by-timestamp for comments), and
write resolved bytes back. No human intervention required.

That recovery is what we now need to write. We filed a follow-up
issue, `e2e473b`, for the **merge driver**: the layer that runs
after `jj fetch`, detects whether the `bugs` bookmark has a
content conflict, applies the per-field policy, and pushes the
resolution. Without it, two agents on two machines can't both
work the same project for very long.

## What's next

The four research issues that close cleanly closed with
`Confidence: high`. Storage shape stays open as a watcher: the
decision is recorded, and the issue closes when the implementation
adopts it.

Today's outcome: we know what we're building, we know which parts
won't be cheap, and we have one issue's worth of work (the merge
driver) as the next concrete piece of code. We don't have `jjf`
yet beyond a shell shim that proxies to `git-bug`. We don't have
a website, a PWA, or a voice input. We have the shape, and a
backlog with one item on it.

That's a fine place to be on the first day.

## Links

- Project repo: `~/p/jjforge` (local; not yet hosted).
- Research issue files (in the project's `git-bug`):
  - 2130de1 — jj-lib API stability for embedded use.
  - a60bb95 — does jj op log give us what we need for jjf ops.
  - 380c4e2 — smallest reasonable Rust example using jj-lib.
  - dcd4b57 — where exactly do bug records live in the jj repo.
  - 8d3e045 — distributed-edit behavior on the issues bookmark.
  - e2e473b — implement jjf merge driver for the bugs bookmark.
- Commits this post documents:
  - `6739b37a29787f60d376993dece1fc3cd691cae9` — initial commit.
  - `d95f08342e873dafb2484c248c9ae6ede923aaf8` — `jjf` shim.
  - `4ac9c784ec7f54e67aa339e3df67cfb81643a0e7` — experiments from
    2130de1.
  - `dcd88ffd01dfda1479903b2a74ba805b7026a9ca` — experiments from
    a60bb95.
  - `420be9064060b4ea29049dcbb241d0458cc3de02` — experiments from
    380c4e2.
  - `d034be00c19852e9def219d4a420a9c40f7d356a` — experiments from
    dcd4b57.
  - `8caa280925bbe586173ba812f95900f0cf35486b` — experiments from
    8d3e045.
- Inspirations: [git-bug], [Beads], [jj]. The zfs-workspace
  markdown-in-repo convention isn't a hosted project; it's a
  local pattern we lift from the same machine.
