# git-issues — operator actions (run manually)

The jj-divorce + rename is code-complete. The binary is `iss`, the
crates are `iss`/`iss-storage`/`iss-merge`, and the tool no longer
shells out to `jj` anywhere. These remaining steps touch your
filesystem / Forgejo and are NOT automated — the code works
regardless of the working-directory name.

1. **Rename the working directory** (optional, cosmetic):
   ```
   mv ~/p/jjforge ~/p/git-issues
   ```
   Nothing in the code depends on the directory name.

2. **Rename the Forgejo repo** `jjforge` → `git-issues` in the web UI,
   then repoint the remote:
   ```
   cd ~/p/git-issues
   git remote set-url origin git@github.com:myers/git-issues.git
   ```

3. **Install the renamed shim.** `bin/iss` replaces `bin/jjf`. If you
   had `bin/jjf` on your PATH (e.g. a symlink in `~/bin`), repoint it
   to `bin/iss`.

## Deliberately NOT changed (vestigial wire tokens)

The on-disk wire format keeps the old spelling on purpose, frozen for
backwards compatibility with every existing host repo:

- `refs/jjf/*` refs (issue records, the format-version sentinel) and
  the `refs/remotes/<remote>/jjf/*` remote-tracking namespace.
- `Jjf-Op:` / `Jjf-At:` commit trailers and the `jjf_at` JSON field.
- The `issues` bookmark name and `ISSUES_BOOKMARK` / `*_REVSET`
  constants.
- `ISSUES_SEED_DESCRIPTION` (`"jjf: seed issues bookmark"`, spec §1.1
  pinned).
- The `Error::NotAJjRepo` variant / `not_a_jj_repo` error kind (the
  git-repo-absent error — the name is kept for now).

A future wire-format rename (`refs/jjf/*` → `refs/iss/*`, `Jjf-*` →
`Iss-*`) would be a breaking change requiring a `vN → vN+1` migrator
across every live host repo. It is intentionally **out of scope** for
this change and not currently planned.

## Storage-format note

There is **no auto-migration**. A repo that is not in the v3 ref
layout (no `refs/jjf/meta/format-version` sentinel) is refused at
`Storage::open` with a typed `unsupported_legacy_format` error rather
than silently read or migrated. All live host repos are already v3.
If you encounter a pre-v3 repo, re-create its issues on a current
`iss`, or restore from a v3 backup.
