# sync-remote investigation

Scratch space for the `sync-remote-setup` ticket (`07780aa`).
Closing the question: **does the `bugs` bookmark sync via standard
`jj git push` / `jj git fetch` with default config, or do we need
to land a refspec/config wrinkle in this ticket?**

Short answer: **default config is enough.** This ticket lands the
`jjf remote add|ls|rm` CLI verb only; no `git.fetch` / refspec
knobs need writing.

## jj surface (jj 0.40.0)

```
jj git remote add <REMOTE> <URL>    # extra flags: --fetch-tags, --push-url
jj git remote list                  # plain text: "<name> <url>" per line, SPACE-separated
jj git remote remove <REMOTE>       # also "forgets its bookmarks" per --help
jj git remote rename <OLD> <NEW>
jj git remote set-url <REMOTE> <URL>
```

Error matrix observed in `.scratch/invest/`:

| operation                       | stderr                                          | exit |
|---------------------------------|-------------------------------------------------|------|
| `add` already-existing name     | `Error: Git remote named 'origin' already exists` | 1 |
| `remove` absent name            | `Error: No git remote named 'nope'`             | 1 |
| `remove` existing               | (none)                                          | 0 |

The `list` output uses SPACE as the column separator, not tab.
`jjf remote ls` re-renders to tab-separated so the output matches
the `<id>\t<status>\t...` convention every other `ls`-style verb
in jjforge uses. We parse jj's output, never pass it through.

## Two-clone sync verified

Walk under `.scratch/clone-test/` (gitignored):

1. `git init --bare bare` — naked git repo as the "server."
2. `jj git clone bare alice` — Alice's clone; jjforge `jjf init`
   creates the `bugs` bookmark.
3. `jjf new -t "from alice" -F -` — Alice writes a bug.
4. `jj git push --bookmark bugs --remote origin` — works, no
   `git.fetch` config, no per-bookmark refspec, nothing.
5. `jj git clone bare bob` — Bob's clone.
6. `jj bookmark track bugs --remote=origin` — Bob has to track
   the remote bookmark to materialize a local `bugs`.
7. `jjf ls` in Bob — Alice's bug appears.

The `bookmark track` step in (6) is the only sync-time wrinkle
and it belongs to `sync-pull` (the next ticket), not here.

## What the ticket lands

Just the CLI verb:
- `jjf remote add <name> <url>` — wraps `jj git remote add`.
- `jjf remote ls` — wraps `jj git remote list`, re-renders as
  tab-separated `<name>\t<url>`.
- `jjf remote rm <name>` — wraps `jj git remote remove`.

Preflight: jj-repo check only (no `bugs` bookmark required —
adding a remote is meaningful before `jjf init`).

Error mapping:
- `not_a_jj_repo` (cwd isn't a jj repo) → exit 2.
- `remote_already_exists` (stderr from jj contains the canonical
  "already exists" phrase) → exit 2.
- `remote_not_found` (stderr from jj contains "No git remote
  named") → exit 2.

## Out of scope (deferred to `sync-push-pull`)

- Bookmark tracking (`jj bookmark track ... --remote ...`).
- `git.fetch` / `git.push` defaults — they already cover bookmarks.
- Per-bookmark refspec knobs (only matters if a future user wants
  to sync `bugs` to one remote and code to another).
- Auth (whatever git/jj do already).

## Follow-up: post-fetch divergence surface (for `sync-push-pull`)

Walked this in `.scratch/post-fetch/` via
`investigate-post-fetch.sh` + `investigate-merge.sh` against jj 0.40.

Headline: **jj surfaces divergence as a "conflicted" bookmark, not
as conflict markers in the file.** After `jj git fetch` on a clone
that made a local edit, when the remote moved the bookmark to a
different commit:

```
bugs (conflicted):
  - <base>           ← common ancestor
  + <local head>     ← @git
  + <remote head>    ← @origin
```

The bugs file in the working copy is NOT auto-materialized; `jjf
show <id>` fails with `Revset 'bookmarks(bugs)' resolved to more
than one revision`. The hint jj prints is to
`jj bookmark set <name> -r <rev>`, which would force-pick one side
and silently drop the other — exactly the wrong default for a bug
tracker.

To merge, we need to:

1. Detect the conflicted-bookmark state. Cheapest probe is to ask
   for `heads(bookmarks(bugs))` and check the count — if >1, the
   bookmark is divergent.
2. `jj new <head_a> <head_b> -m "<msg with merge trailers>"`. jj
   materializes the merge commit's working-copy files with its
   standard textual conflict markers (`<<<<<<<` / `+++++++` /
   `%%%%%%%` / `\\\\\\\` / `>>>>>>>`) — exactly the shape
   `jjf-merge::resolve` parses.
3. For each conflicted `bugs/*.json` file: read it, run
   `jjf_merge::resolve(&text, &MergeOptions::default())`, write
   resolved bytes back.
4. `jj bookmark set bugs -r @ --allow-backwards`. The merge commit
   becomes the new bookmark tip.
5. `jj new root()` to step `@` off the bookmark (standard 4-CLI
   dance suffix).

The merge commit's description carries one `Jjf-Op: merge` trailer
per resolved bug, per `docs/storage-format.md` §5.2.

A clean fetch (the local clone hadn't edited locally) leaves the
bookmark non-conflicted; the file in the working copy is
auto-materialized to the remote's value. No merge driver pass
needed. We detect this case the same way: `heads(bookmarks(bugs))`
returns one revision.

Comments file (`bugs/<id>.comments.jsonl`): if conflicted on
fetch, that's outside the v1 merge driver's scope (`jjf-merge` is
JSON-record only). We treat a conflicted `.jsonl` as `Unmergeable`
and exit 1 — the `sync-conflict-fallback` ticket owns the better
escape hatch.
