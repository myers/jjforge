# Drop `epic:<slug>` label convention; replace with `--parent` filter

## Goal

Make parent-child dep edges the single mechanism for "this issue
belongs to that epic." Drop the `epic:<slug>` label convention.
Remove the bare `epic` label too (redundant with `type: epic`).

## Why

Today, child-of-epic membership is encoded two ways:

- **Label:** every child carries `epic:<slug>`. `jjf ls --label
  epic:foo` and `jjf ready --label epic:foo` filter on it.
- **Parent-child dep edge:** the v2.4 mechanism. `jjf ready`'s
  cascade honors it (parent's blocked state propagates to
  children).

Audit of the live planner (2026-06-28):

- 30+ open and closed issues carry `epic:<slug>` labels.
- **Zero** of the 13 children under `epic:agent-ergonomics` use
  a parent-child edge to the epic. The label is doing 100% of
  the actual work; the dep edge is doing 0%.

The label convention is structural by accident: nothing in the
storage layer or CLI knows about `epic:<prefix>` — it's a string
prefix the operator agrees to follow. The same convention is
also redundant on the epic itself (the epic carries both `epic`
and `epic:<slug>` on top of `type: epic` + `slug`).

Collapsing label + edge into "edge only" reduces the mechanism
count, eliminates the two-step footgun ("file the label, forget
the edge → cascade doesn't reach the child"), and reuses
machinery (`DepKind::ParentChild`) that already works and is
tested.

## Non-goals

- No new edge kinds.
- No schema bump. The `Issue` record's `dependencies` field
  already carries parent-child edges.
- No transitive walk (`--ancestor-of`); single-hop is enough
  for the project's current shape (no nested epics).
- No back-compat shim for `--label epic:foo` invocations.
  Operationally unnecessary because the migration runs the
  same day the CLI lands.
- No changes to `jjf dep tree`. It already walks parent-child
  in the child direction from any root.

## CLI changes

One new flag: `--parent <handle>` on three verbs.

### `jjf ls --parent <handle>`

Filter the listing to issues that carry a `parent-child` edge
whose target equals the resolved `<handle>`. `<handle>` accepts
the v2.1 id-or-slug resolver. Unknown handle exits 2 with
`slug_not_found`.

```bash
jjf ls --parent agent-ergonomics                  # children, any status (default --status open)
jjf ls --parent agent-ergonomics --status all     # plus closed/abandoned
jjf ls --parent agent-ergonomics --type bug       # bug-typed children only
```

AND-composed with all existing `ls` filters (`--label`,
`--type`, `--slug`, `--status`).

### `jjf ready --parent <handle>`

Same filter, applied to the ready set. Composes with
`--label` / `--type` / `--limit` as today.

```bash
jjf ready --parent agent-ergonomics --limit 1     # next unblocked under this epic
```

### `jjf search --parent <handle>`

Same filter, intersected with the search query.

### Flag is not repeatable

`--parent` takes one handle. Multi-parent membership exists in
the data model (a child can carry parent-child edges to several
issues) but the typical query is "things under THIS epic," not
"things under any of these." If multi-parent OR-filtering
becomes a real need, file a follow-up.

## Storage impact

**Zero schema changes.** The migration uses existing verbs
(`jjf dep add --kind parent-child`, `jjf label rm`) and lands as
ordinary planner commits on the affected `refs/jjf/issues/<id>`
refs.

## Implementation

### Filter wiring

1. Locate the predicate struct in
   [`crates/jjf-storage/src/lib.rs`](../../../crates/jjf-storage/src/lib.rs)
   that backs `list_issues` and `list_ready` (`ReadyFilter` is
   one of them; verify the symmetry with `ls`'s filter at
   implementation time).
2. Add a `parent: Option<IssueId>` field. Default is `None`.
3. In the predicate body, `Some(pid)` adds the test
   `issue.dependencies.iter().any(|d| d.target == pid && d.kind == DepKind::ParentChild)`.
4. AND-composed with the existing label / type / status / slug
   tests.

### CLI plumbing

5. Add `--parent <handle>` to the clap arg structs for
   [`Commands::Ls`](../../../crates/jjf/src/main.rs),
   `Commands::Ready`, `Commands::Search`. String-typed at the
   clap layer (it accepts id-or-slug).
6. In each `run_*` function, resolve the handle to an `IssueId`
   via the existing v2.1 resolver. Unknown → `CliError::SlugNotFound`
   (exit 2).
7. Pass the resolved id into the storage-layer filter struct.

### JSON envelope

Unchanged. `--parent` is filter-only; output shapes stay
identical.

## Tests

- [`crates/jjf/tests/ls.rs`](../../../crates/jjf/tests/ls.rs)
  - `ls_parent_flag_filters_to_parent_child_children`
  - `ls_parent_unknown_handle_exits_two`
  - `ls_parent_composes_with_type_and_status`
- [`crates/jjf/tests/ready.rs`](../../../crates/jjf/tests/ready.rs)
  - `ready_parent_flag_filters_to_parent_child_children`
  - `ready_parent_composes_with_type_and_limit`
- [`crates/jjf/tests/search.rs`](../../../crates/jjf/tests/search.rs)
  - `search_parent_flag_intersects_with_query`
- [`crates/jjf-storage/tests/integration.rs`](../../../crates/jjf-storage/tests/integration.rs)
  - One storage-layer test pinning the filter shape independent
    of the CLI.

## Docs surface

### [`docs/quickstart.md`](../../quickstart.md)

Section 6 ("Scale up: epics with deps") currently teaches
`-l epic:backend` + a parent-child edge as a pair. Rewrite as
the parent-child edge alone. Update `jjf ready --label
epic:backend` examples to `jjf ready --parent backend`.

### [`docs/cli-json.md`](../../cli-json.md)

Add `--parent <handle>` to the flags tables for `ls`, `ready`,
`search`. No new error kinds (`slug_not_found` already covers
the unknown-handle case).

### [`skills/using-jjforge/SKILL.md`](../../../skills/using-jjforge/SKILL.md)

Swap `jjf ls --label epic:foo` examples in Common queries to
`jjf ls --parent foo`. Add a Common mistakes row:

| Used `-l epic:foo` to associate a child with an epic | Use `jjf dep add --kind parent-child <child> <epic>`, or `--parent <epic>` on `jjf new`. The `epic:<slug>` label convention was retired. |

### [`skills/subagent-working-a-jjforge-issue/SKILL.md`](../../../skills/subagent-working-a-jjforge-issue/SKILL.md)

No mention of `epic:` labels. No change.

### `CLAUDE.md`

The "Label scheme" section currently documents `epic` and
`epic:<slug>` as conventions. Rewrite as: "Epics are typed
`epic` (the `--type` field). Children attach via parent-child
dep edges (`jjf dep add --kind parent-child <child> <epic>`).
There is no `epic:*` label convention."

The "Queries" section's `jjf ls --label epic:mvp-storage` /
`jjf ready --label epic:host-asterinas` examples become
`jjf ls --parent mvp-storage` / `jjf ready --parent host-asterinas`.

## Migration

The migration is an **operational one-shot, not committed**.
Lives at `experiments/drop-epic-labels/run.sh` (gitignored per
the existing `experiments/**/.scratch/` rule — add the
`drop-epic-labels` subtree to .gitignore if it isn't covered).

Pseudocode:

```bash
# 1. Build slug→id map for every epic.
epics_json=$(jjf ls --type epic --status all --json)

# 2. For every issue carrying any epic:<slug> label:
jjf ls --status all --json \
    | jq -r '.[] | select(.labels[]? | startswith("epic:")) | .id' \
    | while read child_id; do
        for label in $(jjf show $child_id --json | jq -r '.labels[]' | grep '^epic:'); do
            slug=${label#epic:}
            epic_id=$(echo "$epics_json" | jq -r ".[] | select(.slug==\"$slug\") | .id")
            if [ -n "$epic_id" ]; then
                jjf dep add --kind parent-child $child_id $epic_id
                jjf label rm $child_id $label
            else
                echo "WARN: no epic with slug $slug; left label on $child_id" >&2
            fi
        done
    done

# 3. For every epic, drop the bare `epic` label.
echo "$epics_json" | jq -r '.[] | .id' | while read epic_id; do
    jjf label rm $epic_id epic 2>/dev/null || true   # might not have it
done
```

After the script runs cleanly: `jjf push origin` lands the
mutations to the remote.

### Migration verification

Post-script sanity checks:

```bash
# No epic:* labels should remain anywhere.
jjf ls --status all --json | jq -r '.[] | .labels[]?' | grep -c '^epic:' # expect 0

# Every former-child issue should now have a parent-child edge.
# Spot-check a known epic — count of children should equal what
# `--label epic:<slug>` returned before migration.
jjf ls --parent agent-ergonomics --status all --json | jq length
```

## Order of operations

1. **Land CLI changes.** Add `--parent` to `ls` / `ready` /
   `search`; tests pass; commit; push.
2. **Run migration locally.** Execute the script, verify output.
   `jjf push origin` to land the migrated planner refs.
3. **Land doc updates.** quickstart, cli-json, using-jjforge,
   CLAUDE.md. Commit; push.

Two git commits + one planner-data push, no force-pushes, no
schema bumps.

## Risks

- **Stale CLAUDE.md / external doc references to `--label
  epic:foo`.** Anything not under the README-linked docs scope
  (handoffs, design docs, archived planning) won't be migrated.
  These become wrong-but-not-broken: the invocation still runs,
  it just returns nothing for issues that already lost the
  label. Acceptable — those docs are historical.
- **Multi-epic children.** If any issue currently carries two
  `epic:*` labels (it can — labels are a set), the migration
  script adds two parent-child edges. That's semantically
  correct but worth noting.
- **Slug renames.** If an epic's slug ever changed without the
  child labels being updated, the migration logs `WARN` and
  leaves the stale label on the child. Operator handles
  individually.
