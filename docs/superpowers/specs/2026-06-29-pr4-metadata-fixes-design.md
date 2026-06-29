# PR #4 fix-and-merge: per-issue metadata facility

**Status:** spec
**Author session:** 2026-06-29
**Triggering review:** `xhigh` code-review of forgejo PR #4
(`feat/issue-metadata`, commit `76c756c4` by peer agent `claude`).
14 verified findings; this spec covers all of them.

## Goal

Land per-issue string→string metadata in jjforge with every
verified finding from the PR #4 review addressed, then merge
PR #4 on `chaos-inc/jjforge`. End state: a `BTreeMap<String,
String>` field on every issue, exposed via `jjf metadata
set|unset`, `--meta key=value` filter on every list-shaped
verb, `--meta` flag on `jjf new`, plain-text rendering in `jjf
show`, and full spec-doc coverage.

## Scope (cohort decisions)

- **Fix scope:** all 14 verified findings (scope C from
  brainstorming).
- **Merge target:** force-push to `feat/issue-metadata` on
  `forgejo`, merge PR #4 normally. Peer agent keeps authorship
  credit.
- **Record-version policy:** do NOT bump v2→v3. Treat metadata
  as an "additive-tolerated" field per the policy this spec
  codifies in commit 5. Empty-by-default + `#[serde(default)]`
  is the convention for every additive field added so far
  (priority v2.8, slug v2.1, type v2.1) and is now documented.

## Out of scope

- Removing or replacing the `jjf-merge` v1 crate (only the
  policy is patched here; full removal is a separate ticket).
- Reviving `docs/storage-format.md` if it doesn't exist on
  disk (file a separate ticket; this spec patches references
  in-place via TODO comments where needed).
- Structured (JSON-value) metadata. The flat-string facility
  ships as-is; if a consumer needs structured values, file a
  separate ticket for a `metadata_json` facility or migration.
- A `jjf doctor` warning for mixed-version repos. The
  additive-tolerated policy means the warning isn't load-
  bearing for this PR.

## Commit shape (5 themed commits on feat/issue-metadata)

The project's "commit when a coherent unit of work is done"
rule maps cleanly to these five units. Each is independently
reviewable; any one can be dropped or revised without
unraveling the others.

### Commit 1: storage-layer validation hardening

**Files:** `crates/jjf-storage/src/lib.rs`,
`crates/jjf-storage/src/op.rs`.

**Changes:**

- New `validate_metadata_key(s: &str) -> Result<(), Error>`:
  rejects empty, leading/trailing whitespace (`\s`-class
  including `\t`), control characters, and `=` (which would
  break `--meta key=value` filter round-trip).
- New `validate_metadata_value(s: &str) -> Result<(), Error>`:
  retains the existing newline/CR rejection; adds a 256 KiB
  length cap (well above realistic metadata, well under the
  64 KiB body cap so neither field becomes a body-bypass
  vector).
- `Storage::set_metadata` and `Storage::unset_metadata` call
  the new validators at the storage boundary (NOT only at the
  CLI). Direct API callers and future verbs are guarded.
- `Storage::set_metadata` idempotence: short-circuit before
  the mutate closure if `rec.metadata.get(key) == Some(value)`
  already. Return `MutateOutcome::NoOp`. No commit lands.
- `Storage::unset_metadata` idempotence: short-circuit if the
  key is already absent.
- `op.rs::Op::to_wire`: the existing `debug_assert!` newline
  checks stay as defense-in-depth. Now guaranteed-true given
  the storage-layer validators.

**Tests (in `crates/jjf-storage/tests/integration.rs` or a
new unit-tests block):**

- `set_metadata_rejects_empty_key`
- `set_metadata_rejects_whitespace_in_key` (`\t`, ` `, leading
  and trailing each)
- `set_metadata_rejects_equals_in_key`
- `set_metadata_rejects_oversize_value` (257 KiB → error)
- `set_metadata_idempotent_same_value_no_commit` (count refs;
  second set lands zero new commits)
- `unset_metadata_idempotent_absent_key_no_commit`

**Resolves findings:** #1, #3, #4 (storage-layer guard for the
trailer drop case), #8 (idempotence).

### Commit 2: trailer parser hardening + concurrent-write LWW test

**Files:** `crates/jjf-storage/src/trailer.rs`,
`crates/jjf-storage/tests/integration.rs`.

**Changes:**

- `trailer.rs::stanza_to_op` for `set-metadata`: if
  `Jjf-Metadata-Value` is missing, return a typed
  `TrailerError::MissingField { op: "set-metadata", field:
  "Jjf-Metadata-Value" }` instead of letting `?` drop the op
  silently. Same for `unset-metadata` missing
  `Jjf-Metadata-Key`.
- Verify (during implementation) what the existing parse-
  error path does — refuses the snapshot rebuild or warn-and-
  skips. Mirror that pattern; do not invent a new error
  policy.
- New integration test
  `metadata_lww_under_concurrent_heads`:
  1. Set up two divergent `jj op log` heads, each carrying a
     `SetMetadata` op on the same key with different values.
  2. Merge the heads.
  3. Read the issue. Assert the LWW winner is the op with the
     later `Jjf-At` (or the tie-break id if `Jjf-At` ties).
  4. Assert the loser's value is GONE from
     `Issue.metadata.get(key)` (not merged, not concatenated).

**Tests:** the parser-drop case gets a unit test
(`trailer_set_metadata_missing_value_returns_error`); the
concurrent-write case is the integration test above.

**Resolves findings:** #2 (silent op drop), #11 (LWW test
coverage gap).

### Commit 3: CLI symmetry — --meta on ready/search/stale + jjf new --meta + plain-text show

**Files:** `crates/jjf-storage/src/lib.rs` (ReadyFilter,
`Storage::search`, `Storage::stale`),
`crates/jjf-storage/src/record.rs` (IssueDraft),
`crates/jjf/src/main.rs` (CLI surface + plain-text render +
predicate refactor).

**Changes:**

- **`ReadyFilter`** gains `meta: Vec<(String, String)>`;
  `Storage::list_ready` filters issues whose metadata matches
  AND-semantics on every (k, v) pair.
- **`Storage::search`** scans metadata values when an
  `--include-metadata` flag is set on the search call.
  Default OFF — mirrors the existing `--include-comments`
  opt-in to preserve snapshot-cache perf parity. The flag
  reaches the storage layer via the existing search API
  (extend its parameter struct).
- **`Storage::stale`** gains the same `meta` filter as
  ReadyFilter.
- **`IssueDraft`** gains `metadata: BTreeMap<String, String>`.
  `Storage::create_issue` emits `SetMetadata` ops alongside
  the create op, atomically in the same commit. The issue
  arrives on the bookmark with metadata already populated;
  no second commit needed.
- **CLI Clap surface:**
  - New `--meta key=value` (repeatable) on: `Commands::New`,
    `Commands::Ls`, `Commands::Ready`, `Commands::Search`,
    `Commands::Stale`.
  - `value_parser`: `parse_meta_kv(s: &str) -> Result<(String,
    String), String>` — `s.split_once('=').map(|(k,v)| (k.into(),
    v.into())).ok_or_else(|| "expected key=value (no '=' is an
    error, not an empty-value match)".into())`. Bare keys fail
    at clap parse time (exit 2 via clap, error code surfaced
    as `metadata_filter_malformed` in the `--json` envelope).
  - The `meta: Vec<String>` field type on each command becomes
    `meta: Vec<(String, String)>`.
- **`metadata_matches` predicate** in `main.rs` becomes
  `wanted.iter().all(|(k, v)| issue.metadata.get(k) ==
  Some(v))`. The inline `split_once` is gone (it was parsing
  per-issue per-filter, an O(N×M) drop into O(1) after the
  argv-time parse).
- **`jjf show` plain-text renderer:** when
  `issue.metadata.is_empty() == false`, emit:
  ```
  metadata:
    <key1>=<value1>
    <key2>=<value2>
  ```
  Keys sorted (BTreeMap iteration order is already sorted —
  free). Place between `labels:` and `dependencies:` to
  mirror the JSON field ordering.

**Tests:**

- `jjf_new_meta_seeds_metadata_atomically`: create with
  `--meta k=v`, verify one commit, verify metadata present
  before any post-create op.
- `jjf_ready_filters_by_meta`: two issues, one with metadata,
  `jjf ready --meta k=v` returns only the matching one.
- `jjf_search_metadata_off_by_default`: search value substring
  doesn't match without `--include-metadata`.
- `jjf_search_metadata_with_flag`: same query with
  `--include-metadata` returns the match.
- `jjf_show_plain_renders_metadata`: snapshot test on the
  plain-text rendering.
- `meta_value_parser_rejects_bare_key`: clap-level rejection
  on `--meta foo`.

**Resolves findings:** #6 (docstring lie — replaced with hard
exit-2 rejection at parse time), #7 (`--meta` on
ready/search/stale), #9 (cli-json contract — companion to
commit 5's docs), #12 (IssueDraft.metadata + jjf new --meta),
#13 (plain-text show), the `metadata_matches` predicate
cleanup from the reuse review.

### Commit 4: cache schema bump + jjf-merge policy patch

**Files:** `crates/jjf-storage/src/cache.rs`,
`crates/jjf-merge/src/merge.rs`.

**Changes:**

- `cache.rs::CACHE_SCHEMA_VERSION`: `1 → 2`. One-time cache
  rebuild on first read with the new binary. Closes the
  upgrade-then-read-stale window where `jjf ls --meta` would
  return empty on a pre-PR cache.
- `jjf-merge/src/merge.rs::MergePolicy::default()`: add
  `"metadata"` to the per-key LWW family. Use the same merge
  shape the labels field uses (or — if labels uses a different
  semantic — extend the policy enum to express
  "per-key-LWW-map" as a distinct variant from "set" /
  "scalar"). The reducer code in `merge_ops.rs` is the source
  of truth for the right merge; this commit only patches the
  v1 fallback driver so it doesn't silently corrupt metadata
  if anyone ever wires it back in.

**Tests:**

- `cache_schema_bump_forces_rebuild`: write a v1 cache file
  with no metadata in the cached Issues, point new binary at
  it, assert a rebuild happens on first read.
- `jjf_merge_metadata_per_key_lww`: feed the v1 driver two
  records each carrying half of the same metadata BTreeMap;
  assert the merged record contains the union with the LWW
  winner for any shared key.

**Resolves findings:** #10 (cache schema bump), #14
(jjf-merge dead-code drift).

### Commit 5: documentation — cli-json.md, storage-format.md handling, CLAUDE.md additive policy

**Files:** `docs/cli-json.md`, `docs/storage-format.md` (or a
new ticket if it doesn't exist), `CLAUDE.md`.

**Changes:**

- **`docs/cli-json.md`:**
  - New section for `jjf metadata set` and `jjf metadata
    unset`: envelope shape `{"ok": true, "id": "...", "key":
    "...", "value": "..." | absent, "action": "set" | "unset"}`.
    Document the asymmetry (value absent on unset) explicitly.
  - Update `jjf ls`, `jjf ready`, `jjf search`, `jjf stale`,
    `jjf new`: add `--meta key=value` to the flag reference;
    note bare-key rejection (exit 2,
    `metadata_filter_malformed`).
  - Update `jjf show --json` and `jjf ls --json` envelope
    docs: add the `metadata` object (empty map `{}` when no
    keys; field always present in record).
- **`docs/storage-format.md`:**
  - If it exists on disk, add §3 (record schema) entry for
    `metadata: BTreeMap<String, String>` and §5 (trailer
    schema) entries for `Jjf-Metadata-Key`,
    `Jjf-Metadata-Value`, op kinds `set-metadata` /
    `unset-metadata`.
  - If it does NOT exist (the review flagged it as a phantom
    reference): file a separate ticket
    `storage-format-doc-revival` under the same epic; add a
    short TODO comment in `crates/jjf-storage/src/record.rs`
    near the `metadata` field pointing at that ticket. Do not
    create a half-baked file in this PR.
- **`CLAUDE.md`** — append to the "Multiple host repos —
  auto-migration matters now" section a new subsection
  "Additive field policy":

  > A new field that's empty-by-default and read-tolerant
  > via `#[serde(default)]` does NOT bump the record version.
  > Every additive field shipped to date (priority v2.8, slug
  > v2.1, type v2.1, metadata 2026-06-29) follows this rule.
  > Reserve version bumps for breaking changes — removed
  > fields, semantic changes to existing fields, ref-namespace
  > moves. The asymmetric-read-tolerance trade-off (older
  > binaries silently drop the new field on write-back) is
  > the explicit cost.

**Tests:** none — pure docs. The PR description on Forgejo
gets updated to link this spec.

**Resolves findings:** #5 (version-bump policy documented),
#9 (cli-json.md update), #15 (CLAUDE.md violation closed by
the docs work).

## Acceptance for the whole cohort

Before force-pushing `feat/issue-metadata` and merging PR #4:

- `cargo nextest run --workspace` green (existing 638 + new
  tests above).
- `cargo clippy --workspace --all-targets` shows no NEW
  warnings vs `main` baseline.
- `cargo build --release` green.
- Manual smoke from `~/p/jjforge`:
  - `jjf new --meta gc.routed_to=worker-1 --slug trial -t 'metadata trial' -F /dev/null` succeeds; `jjf show trial` plain-text shows the metadata block.
  - `jjf ready --meta gc.routed_to=worker-1` returns the trial issue.
  - `jjf metadata set trial notes "$(python3 -c 'print("a"*300000)')"` rejects with `value_too_large` error.
  - `jjf ls --meta foo` (bare key) exits 2 with `metadata_filter_malformed`.
  - `jjf abandon trial` (cleanup).
- PR #4 description updated to link
  `docs/superpowers/specs/2026-06-29-pr4-metadata-fixes-design.md`.

## Risks and open seams

- **TrailerError parse-error policy:** the implementation
  needs to read existing trailer-parse-failure handling in
  `merge_ops.rs` and `read.rs` and mirror it. If existing
  policy is warn-and-skip-the-op, the metadata fix surfaces
  the missing-field case but lets the projection continue.
  If existing policy is refuse-the-snapshot-rebuild, this is
  the right behavior but may force a manual repair for a
  hand-edited bad commit. The implementation plan will read
  the existing pattern and pick the consistent one.
- **`--include-metadata` flag bikeshed:** the spec picks
  opt-in (mirrors `--include-comments`). If reviewers prefer
  always-on (simpler UX, metadata is short and cheap to
  scan), the flag goes away and `Storage::search` always
  scans metadata. Either is defensible; the flag is the
  conservative default.
- **`MergePolicy` enum extension:** the v1 driver may not
  cleanly express "per-key LWW map" without a new policy
  variant. If labels uses an `array_fields` shape that
  doesn't fit metadata's map semantics, the patch needs a
  new variant — adding ~30 lines to a dead-code crate. If
  the cost is high, switch to a TODO comment and leave the
  driver in its current state; the planner ticket
  `cc2fa96` for `jjf-merge` removal becomes the right
  follow-up.
- **`docs/storage-format.md` phantom:** if the file doesn't
  exist, commit 5 files a separate ticket and adds a TODO
  in source. If reviewers want the file revived in this PR,
  that's a separate effort and the spec gets a sub-ticket.
- **Force-push hazard:** the PR's branch on Forgejo will be
  rewritten. Verify with the PR's author (peer agent) that
  no other in-flight work depends on the current SHA. The
  spec assumes nobody is rebasing onto `76c756c4`.
