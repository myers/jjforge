# storage-shape

Empirical probe for issue `dcd4b57` — *where exactly do bug records
live in the jj repo?*

The issue lists three candidate shapes:

- **A** — Bookmark-branch. Bugs are files on a dedicated `bugs`
  bookmark. Each mutation is a commit on that bookmark.
- **B** — Side-channel jj operation type. Custom metadata attached
  directly to jj operations.
- **C** — Files on `main`, like `~/p/zfs-workspace/issues/`. jj is
  just the VCS that happens to be there.

Each shape has a shell script that builds a throwaway bare git
remote, clones it twice (`alice`, `bob`), exercises the five
questions from the issue body, and writes a transcript to
`runs/shape-<x>.transcript.txt`.

## Run

```sh
bash test-shape-a-bookmark.sh
bash test-shape-b-side-channel.sh
bash test-shape-c-files-on-main.sh
```

Each script is hermetic: it builds its scratch repos under
`.scratch/` and never leaves a `.git/` or `.jj/` behind in the
source tree once `.scratch/` is cleaned (the `.gitignore` excludes
`.scratch/`).

## Summary of findings

Posted as a comment on `dcd4b57`. Verdict: **A (bookmark-branch).**

- Shapes **A** and **C** have identical distributed-edit behavior:
  bookmark conflict → push refused → `jj new bookmarks(<name>)` to
  merge → jj-style content conflict markers in the bug file → human
  or agent picks a side → `jj bookmark set` → push.
- Shape **B** is structurally infeasible today: there is no public
  way to write custom data into jj operations, AND the op store is
  local-only (does not survive clone). Confirmed empirically: alice
  ran 5 mutations and ended with 10 ops; bob's fresh clone had 6
  ops, none of them alice's.
- The deciding difference between A and C is **blast radius**: in
  C, every bug-edit race conflicts the `main` bookmark, blocking
  code merges to `main` until the bug edit is resolved. In A, the
  same race conflicts only the `bugs` bookmark.

The `8d3e045` distributed-edit research only needs to test the A
shape; the B and C answers are already established (B: not
buildable; C: same shape as A but on the wrong bookmark).
