# Design: jjforge → git-issues, and total jj removal

Date: 2026-06-30
Status: approved design, pre-implementation

## Summary

Two independent but co-shipped changes to the project currently named
**jjforge** (CLI `jjf`):

1. **Workstream J — total jj divorce.** Remove every dependency on the
   `jj` CLI. End state: the binary runs in *any* git repository with
   **jj not installed at all**. The v3 data layer is already pure git
   plumbing (`git hash-object` / `mktree` / `commit-tree` / `update-ref`
   / `cat-file` / `for-each-ref`); jj survives only in v2-legacy paths,
   the v2→v3 migrator, two convenience wrappers, and preflight probes.
   All of those are removed or rerouted to plain `git`.

2. **Workstream R — rename.** `jjforge` → `git-issues`; command
   `jjf` → `iss`; crates `jjf-storage`/`jjf-merge` → `iss-storage`/
   `iss-merge`. **Human surface only.** The on-disk wire format —
   the `refs/jjf/*` ref namespace and the `Jjf-*` commit-message
   trailer keys — is left UNCHANGED as vestigial wire tokens. Renaming
   the wire format is deferred to a future `vN → vN+1` storage
   migration (tracked separately; see "Deferred" below).

The two workstreams are separable (either could ship alone) but land
together because both touch `crates/jjf/src/main.rs` heavily. **J is
sequenced before R** so dead code is deleted before it would otherwise
be renamed.

## Naming decisions (final)

| Layer | Old | New |
|-------|-----|-----|
| Project name | jjforge | git-issues |
| Binary / command | `jjf` | `iss` |
| Main crate | `jjf` | `iss` |
| Storage crate | `jjf-storage` | `iss-storage` |
| Merge crate | `jjf-merge` | `iss-merge` |
| Actor env var | `JJF_ACTOR` | `ISS_ACTOR` |
| Cache file | `.jj/jjforge-cache.json` | `.git/iss-cache.json` |
| Shim | `bin/jjf` | `bin/iss` |
| **Ref namespace** | `refs/jjf/*` | **unchanged (vestigial)** |
| **Trailer keys** | `Jjf-Op:` / `Jjf-Issue:` / `Jjf-At:` / … | **unchanged (vestigial)** |

Verb-selection rationale: `iss` reads plainly as "issues," avoids the
`git`-anagram fingertwist hazard (ruled out `gti`, `gbd`), avoids the
geospatial-domain overload of `gis`, and avoids shadowing beads' own
`bd`. Name `git-issues` chosen over `git-beads` for being
self-documenting (no lineage knowledge required).

## Why the rename does not touch live data

The project dogfoods its own planner on `refs/jjf/issues/*` in this
very repo, and `~/p/asterinas-workspace` is a second live v3 host.
Because Workstream R keeps the `refs/jjf/*` namespace (option A),
`iss show roadmap` reads the **exact same refs** that `jjf show roadmap`
read. No data migration, no cross-repo broadcast, no version bump. The
rename is purely a human-surface change.

---

## Workstream J — total jj divorce

Delete-then-reroute, in dependency order. After J, no code path
constructs `JjRepo` or spawns a `jj` subprocess.

### J1. Delete v2-legacy and migration code

These paths exist only to read, sync, or migrate v2-shape repos. All
live host repos are v3, so deleting them regresses nothing live. The
accepted cost: a v2-shape repo becomes **unreadable and
non-migratable** by `iss`. The migrator stays recoverable from git
history if a stray v2 repo ever surfaces.

Remove:

- `run_push_v2_legacy` and the v2 branch of `run_pull` /
  `probe_pull_mode` in `crates/jjf/src/main.rs` (the `jj git push
  --bookmark issues`, `jj bookmark set issues`, `jj new root()` dance
  at ~4662, ~4893, ~5033–5140). The `is_v3()` gate in `run_push`
  collapses to the v3 path unconditionally.
- `crates/jjf-storage/src/migrate_v2_v3.rs` — the entire module (the
  v2→v3 migrator and all its `jj bookmark *` calls).
- The v2 cache probes in `crates/jjf-storage/src/cache.rs`:
  `probe_head_commit` and the `jj file show` path (~283–296, ~825+).
  These query `ISSUES_BOOKMARK_REVSET`; v3 uses the separate ref-set
  rebuild path (`rebuild_v3` / `load_or_rebuild`, ~433) which stays.
- The v1/v2 preflight probes in `crates/jjf/src/preflight.rs`
  (~45, ~89, ~109) that shell out to `jj`.

### J2. Reroute convenience wrappers to plain git

- `remote add` / `remote ls` / `remote rm` (`main.rs` ~3220, ~3343,
  ~3407): `jj git remote add|list|remove` → `git remote add|<list>|
  remove`. Adjust output parsing to git's format (`git remote -v`
  vs jj's listing).
- `jj config get <key>` (`main.rs` ~3854): determine the key at
  implementation time (likely `user.name` / `user.email`); replace
  with `git config <key>` or drop if no longer needed.

### J3. Delete `JjRepo`

After J1+J2, `JjRepo::open` has no callers.

- Delete `crates/jjf-storage/src/jj.rs`.
- Remove `pub use jj::JjError` and `use jj::JjRepo` from `lib.rs`.
- Fold any still-referenced error variants from `JjError` into the
  git error type (`GitError` in `git.rs`).
- Repoint or delete the three `JjRepo::open` sites in `lib.rs`
  (~1565, ~1621, ~1673) — they go away with the v2 paths or repoint
  at `GitRepo`.

### J4. Strip self-host guard remnants and env opt-out

- `JJF_ALLOW_SELF_HOST` env var and the `refuse_self_hosted_write`
  preflight — these guarded the v2 HEAD-drift bug, which cannot occur
  in v3. Remove the env var and the preflight.
- The v2-opt-out env var that gated the `is_v3()` fallback — remove;
  the fallback target (v2 path) is gone.

### J5. Verify the divorce

New integration test: full `iss` lifecycle (`init` → `new` → `show`
→ `ls` → `push`/`pull` round-trip) in a **plain `git init` repo with
jj absent**. The test either runs with `PATH` scrubbed of `jj`, or
asserts no `jj` subprocess is spawned. This is the acceptance
criterion for "jj removed."

---

## Workstream R — rename to git-issues / `iss`

Mechanical and wide; layered so each layer is independently testable.

### R1. Cargo / crates

- Rename binary+crate `jjf` → `iss`, `jjf-storage` → `iss-storage`,
  `jjf-merge` → `iss-merge`. Update `[[bin]] name = "iss"`, all
  `[dependencies]` cross-refs, and every `use jjf_storage::` /
  `use jjf_merge::` → `iss_storage::` / `iss_merge::`.
- Directory renames: `crates/jjf/` → `crates/iss/`,
  `crates/jjf-storage/` → `crates/iss-storage/`,
  `crates/jjf-merge/` → `crates/iss-merge/` (git tracks the moves).

### R2. Identifiers in code

- Env var `JJF_ACTOR` → `ISS_ACTOR` (the `--actor` flag and
  `jj user.name` precedence chain stay; only the env name changes).
- Cache file path `.jj/jjforge-cache.json` → `.git/iss-cache.json`
  (a `.jj/` dir may not exist post-divorce; `.git/` always does).
- User-facing strings: help text, error messages, `--version`
  banner, the invocation name in usage output → `iss` / `git-issues`.
- **Explicitly NOT changed (option A):** `refs/jjf/*` ref literals,
  `Jjf-*` trailer key constants, and any `ISSUES_BOOKMARK`-derived
  constants that survive J. These remain as wire-format tokens.

### R3. Docs and project surface

- `README.md`, `CLAUDE.md`, all of `docs/`, `blog/` references, and
  the shim `bin/jjf` → `bin/iss`.
- Fix the dangling `docs/storage-format.md` reference in `CLAUDE.md`
  — that file does not exist; the storage spec lives in
  `docs/architecture.md` + `docs/storage-out-of-tree.md`. Repoint
  the references.
- Rename the skill `subagent-working-a-jjforge-issue` and update its
  trigger keywords (`jjf` / `jjforge` → `iss` / `git-issues`).

### R4. Operator actions (listed, NOT auto-executed)

The spec records these commands; the operator runs them when ready.
The code works regardless of the on-disk directory name.

- `mv ~/p/jjforge ~/p/git-issues`
- Rename the Forgejo repo `git@github.com:myers/jjforge.git` →
  `git@github.com:myers/git-issues.git` and update the `origin`
  remote URL.

---

## Testing and sequencing

- **Order:** J (delete + divorce) fully, then R (rename).
- **Gate after J:** `cargo nextest run --workspace` green (fall back
  to `cargo test --workspace` if nextest absent).
- **Gate after R:** `cargo nextest run --workspace` green.
- **J acceptance:** the J5 jj-absent lifecycle test passes.
- **R verification sweep:** `grep -rn 'jjf\|jjforge\|JjRepo\|JJF_'
  crates/ --include='*.rs'` returns **only** the intentional
  `refs/jjf/*` and `Jjf-*` wire tokens — nothing else.

## Deferred (explicitly out of scope)

- **Wire-format rename** (`refs/jjf/*` → `refs/iss/*`, `Jjf-*` →
  `Iss-*`). This is a breaking on-disk change requiring a `vN → vN+1`
  open-time migrator and a cross-repo broadcast to every live host
  repo. Reserved for a future migration ticket per the
  migration-design rules in `CLAUDE.md`. The vestigial `jjf` tokens
  are the accepted cost of keeping this change migration-free.
- **Reading v2-shape repos.** Removed with J1; not restored.

## Risks

- **R1 (low):** J1 deletes the only v2→v3 migrator; a stray
  un-migrated v2 repo becomes unreadable by `iss`. Mitigated: both
  live hosts are v3; migrator recoverable from git history.
- **R2 (low):** the rename is wide (~900+ files reference `jjf`).
  Mitigated by the R-verification grep sweep, which makes any missed
  rename (outside the allowed wire tokens) visible.
- **R3 (low):** the self-hosted planner runs on `refs/jjf/*` in this
  repo; a mistaken wire-format edit would orphan the live roadmap.
  Mitigated: option A forbids touching `refs/jjf/*` / `Jjf-*`, and the
  grep sweep confirms those tokens are the *only* surviving `jjf`
  references.
