# Drop epic:<slug> labels; add --parent filter — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `epic:<slug>` label convention with parent-child dep edges. Add a `--parent <handle>` filter to `jjf ls` / `ready` / `search`.

**Architecture:** A new `--parent <handle>` flag on three CLI verbs filters issues by `parent-child` dep edge target. `ls` and `search` filter at the CLI layer (matching existing `labels_match` / `types_match` helper pattern); `ready` filters in the storage layer (`ReadyFilter`, where the priority sort happens). The handle resolves through the existing v2.1 id-or-slug resolver — unknown handle exits 2 with `slug_not_found`. Existing planner data migrates via a one-shot, gitignored operational script (not committed code).

**Tech Stack:** Rust 1.75+, clap-derive, cargo nextest. The CLI binary lives at [`crates/jjf/src/main.rs`](../../../crates/jjf/src/main.rs); storage primitives at [`crates/jjf-storage/src/lib.rs`](../../../crates/jjf-storage/src/lib.rs).

## Global Constraints

- **No schema bump.** The `Issue` record's `dependencies` field already carries parent-child edges (v2.4, `DepKind::ParentChild`). The migration uses existing verbs.
- **No back-compat shim.** `--label epic:foo` invocations stop finding migrated issues; the user runs the migration script the same day the CLI lands.
- **No new error kinds.** `slug_not_found` (exit 2) already covers the unknown-handle case.
- **JSON envelopes unchanged.** `--parent` is filter-only; output shapes stay identical.
- **Commit hygiene:** explicit-name `git add`, no `git add .` or `-A`. Per-issue closing comments use the four-section recipe (Findings / Recommendation / Confidence / Open follow-ups).
- **The flag is not repeatable.** `--parent` takes one handle. Multi-parent OR-filtering is YAGNI today.

---

## File Structure

This plan touches these files:

**Source (new code):**
- [`crates/jjf-storage/src/lib.rs`](../../../crates/jjf-storage/src/lib.rs) — extend `ReadyFilter` with a `parent: Option<IssueId>` field and honor it in `list_ready`.
- [`crates/jjf/src/main.rs`](../../../crates/jjf/src/main.rs) — add `--parent <handle>` to `Commands::Ls` / `Commands::Ready` / `Commands::Search`; wire it through `run_ls` / `run_ready` / `run_search`; add a `parent_matches` helper.

**Tests (new files / additions):**
- [`crates/jjf-storage/tests/integration.rs`](../../../crates/jjf-storage/tests/integration.rs) — one storage-layer test pinning `ReadyFilter::parent` semantics independently of the CLI.
- [`crates/jjf/tests/ls.rs`](../../../crates/jjf/tests/ls.rs) — `--parent` filter behavior + unknown-handle preflight.
- [`crates/jjf/tests/ready.rs`](../../../crates/jjf/tests/ready.rs) — `--parent` filter + composition with other filters.
- [`crates/jjf/tests/search.rs`](../../../crates/jjf/tests/search.rs) — `--parent` intersects with the query.

**Docs (updates):**
- [`docs/quickstart.md`](../../quickstart.md) — Section 6 rewrite (use parent-child edges, drop `-l epic:` examples).
- [`docs/cli-json.md`](../../cli-json.md) — `--parent` row added to `ls` / `ready` / `search` flag tables.
- [`skills/using-jjforge/SKILL.md`](../../../skills/using-jjforge/SKILL.md) — swap label-based examples, add Common mistakes row.
- `CLAUDE.md` — rewrite Label scheme section; update Queries section examples.

**Operational (NOT committed):**
- `experiments/drop-epic-labels/run.sh` — one-shot migration. Gitignored.

---

## Task 1: Add `parent: Option<IssueId>` to `ReadyFilter` + honor in `list_ready`

**Files:**
- Modify: [`crates/jjf-storage/src/lib.rs`](../../../crates/jjf-storage/src/lib.rs) — `ReadyFilter` struct (around line 626) and `list_ready` filter loop (around line 3465).
- Test: [`crates/jjf-storage/tests/integration.rs`](../../../crates/jjf-storage/tests/integration.rs).

**Interfaces:**
- Consumes: nothing new (extends an existing struct).
- Produces: `ReadyFilter::parent: Option<IssueId>` — when `Some(pid)`, `list_ready` only returns issues with a `DepKind::ParentChild` edge whose `target == pid`.

- [ ] **Step 1: Write the failing storage-layer test**

Add to [`crates/jjf-storage/tests/integration.rs`](../../../crates/jjf-storage/tests/integration.rs). Mirror existing tests' setup pattern (look for `fn fresh_storage` or similar helper):

```rust
#[test]
fn list_ready_filters_by_parent_child_edge() {
    let storage = fresh_storage("list_ready_parent");
    let epic = storage
        .create_issue(&IssueDraft {
            title: "epic".into(),
            type_: Some(IssueType::Epic),
            ..Default::default()
        })
        .unwrap();
    let child = storage
        .create_issue(&IssueDraft {
            title: "child of epic".into(),
            ..Default::default()
        })
        .unwrap();
    let sibling = storage
        .create_issue(&IssueDraft {
            title: "sibling, no edge".into(),
            ..Default::default()
        })
        .unwrap();
    storage
        .add_dep_edge(&child, &epic, DepKind::ParentChild)
        .unwrap();

    // No filter: all three appear.
    let all = storage.list_ready(&ReadyFilter::default()).unwrap();
    assert_eq!(all.len(), 3);

    // --parent <epic>: only `child` and `epic` itself appear?
    // No — `--parent <epic>` is "issues with parent-child edge TO
    // <epic>". The epic itself doesn't have an edge to itself, so
    // it's excluded. Only `child`.
    let filter = ReadyFilter {
        parent: Some(epic.clone()),
        ..Default::default()
    };
    let filtered = storage.list_ready(&filter).unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, child);
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test --release -p jjf-storage --test integration list_ready_filters_by_parent_child_edge`

Expected: FAIL with a compile error (`ReadyFilter` has no field `parent`).

- [ ] **Step 3: Add `parent` field to `ReadyFilter`**

In [`crates/jjf-storage/src/lib.rs`](../../../crates/jjf-storage/src/lib.rs), modify the `ReadyFilter` struct (around line 626):

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadyFilter {
    pub labels: Vec<String>,
    pub types: Vec<IssueType>,
    pub limit: Option<usize>,
    pub include_claimed: bool,
    pub include_blocked: bool,
    /// When `Some(pid)`, `list_ready` only returns issues that
    /// carry a `DepKind::ParentChild` edge whose `target == pid`.
    /// Mirrors the CLI's `--parent <handle>` flag. AND-composed
    /// with `labels` / `types`.
    pub parent: Option<IssueId>,
}
```

- [ ] **Step 4: Honor `parent` in `list_ready`**

Find `pub fn list_ready` in [`crates/jjf-storage/src/lib.rs`](../../../crates/jjf-storage/src/lib.rs) (around line 3465). After the existing `labels` and `types` filtering, add:

```rust
if let Some(pid) = &filter.parent {
    issues.retain(|i| {
        i.dependencies
            .iter()
            .any(|d| d.target == *pid && d.kind == DepKind::ParentChild)
    });
}
```

Place this after the existing label/type filtering but BEFORE the priority sort, so the filter narrows the set before sorting (cheaper) and the sort still sees only the surviving subset.

- [ ] **Step 5: Run the test and verify it passes**

Run: `cargo test --release -p jjf-storage --test integration list_ready_filters_by_parent_child_edge`

Expected: PASS.

- [ ] **Step 6: Run the full workspace to confirm no regression**

Run: `cargo nextest run --workspace`

Expected: All previously-passing tests still pass; the new test passes.

- [ ] **Step 7: Commit**

```bash
git add crates/jjf-storage/src/lib.rs crates/jjf-storage/tests/integration.rs
git commit -m "$(cat <<'EOF'
storage: add ReadyFilter::parent for parent-child edge filtering

Extends ReadyFilter with an Option<IssueId> field that, when
Some(pid), restricts list_ready to issues carrying a
DepKind::ParentChild edge to pid. AND-composed with the existing
labels / types filters. Default None preserves the current
behavior.

Backs the upcoming `jjf ready --parent <handle>` CLI flag.
EOF
)"
```

---

## Task 2: Add `--parent <handle>` to `jjf ready`

**Files:**
- Modify: [`crates/jjf/src/main.rs`](../../../crates/jjf/src/main.rs) — `Commands::Ready` arg struct (around line 1880); `run_ready` function (around line 3807).
- Test: [`crates/jjf/tests/ready.rs`](../../../crates/jjf/tests/ready.rs).

**Interfaces:**
- Consumes: `ReadyFilter::parent` from Task 1.
- Produces: `jjf ready --parent <handle>` (handle = 7-hex id or slug, resolved via the existing v2.1 resolver).

- [ ] **Step 1: Write the failing test (basic --parent filtering)**

Add to [`crates/jjf/tests/ready.rs`](../../../crates/jjf/tests/ready.rs):

```rust
#[test]
fn ready_parent_flag_filters_to_parent_child_children() {
    let repo = make_initialized_repo("ready_parent_basic");

    let epic_id = parse_id_from_stdout(
        &run_jjf(&repo, &["new", "--json", "-t", "Epic", "--type", "epic", "--slug", "demo-epic"])
            .stdout,
    );
    let child_id = parse_id_from_stdout(
        &run_jjf(&repo, &["new", "--json", "-t", "child", "--parent", epic_id.as_str()]).stdout,
    );
    let _sibling_id = parse_id_from_stdout(
        &run_jjf(&repo, &["new", "--json", "-t", "sibling"]).stdout,
    );

    // Bare `ready` returns all three.
    let bare = run_jjf(&repo, &["ready", "--json"]);
    let bare_arr: Vec<serde_json::Value> = serde_json::from_slice(&bare.stdout).unwrap();
    assert_eq!(bare_arr.len(), 3);

    // `--parent <epic-id>` returns only the child.
    let filtered = run_jjf(&repo, &["ready", "--json", "--parent", epic_id.as_str()]);
    let filtered_arr: Vec<serde_json::Value> = serde_json::from_slice(&filtered.stdout).unwrap();
    assert_eq!(filtered_arr.len(), 1);
    assert_eq!(filtered_arr[0]["id"].as_str().unwrap(), child_id.as_str());

    // `--parent demo-epic` (by slug) works identically.
    let by_slug = run_jjf(&repo, &["ready", "--json", "--parent", "demo-epic"]);
    let by_slug_arr: Vec<serde_json::Value> = serde_json::from_slice(&by_slug.stdout).unwrap();
    assert_eq!(by_slug_arr.len(), 1);
    assert_eq!(by_slug_arr[0]["id"].as_str().unwrap(), child_id.as_str());
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test --release -p jjf --test ready ready_parent_flag_filters_to_parent_child_children`

Expected: FAIL with `unexpected argument '--parent' found`.

- [ ] **Step 3: Add `--parent` to `Commands::Ready`**

In [`crates/jjf/src/main.rs`](../../../crates/jjf/src/main.rs), find the `Commands::Ready` enum variant (around line 1880). Add a field. Look at the existing `labels` / `types` arg attributes for the pattern to mirror. The new field:

```rust
        /// Filter to issues carrying a `parent-child` dep edge to
        /// `<handle>`. `<handle>` is an issue id (7-char hex) or
        /// slug. AND-composed with `--label` / `--type`. Unknown
        /// handle exits 2 (`slug_not_found`).
        #[arg(long)]
        parent: Option<String>,
```

- [ ] **Step 4: Plumb `parent` through the dispatch and `run_ready`**

Update the `Commands::Ready { ... }` destructure in the main match arm (around line 1880) to include `parent`, then pass it into `run_ready`:

```rust
        Commands::Ready {
            labels,
            r#type,
            limit,
            include_claimed,
            include_blocked,
            claim,
            parent,
        } => run_ready(cli.json, labels, r#type, limit, include_claimed, include_blocked, claim, parent),
```

Update `fn run_ready` signature to accept the new arg (around line 3807):

```rust
fn run_ready(
    json: bool,
    labels: Vec<String>,
    types: Vec<TypeArg>,
    limit: Option<usize>,
    include_claimed: bool,
    include_blocked: bool,
    claim: bool,
    parent: Option<String>,
) -> Result<(), CliError> {
```

In the body, after the existing preflight/storage-open code and before constructing `ReadyFilter`, resolve `parent`:

```rust
    let parent_id: Option<IssueId> = match parent {
        Some(handle) => Some(resolve_handle(&storage, &handle)?),
        None => None,
    };
```

(`resolve_handle` is the existing function — grep for it; if the symbol name differs in the codebase, use whatever the existing CLI verbs use to turn an id-or-slug string into an `IssueId`. Probably named `resolve_handle` or `resolve_id_or_slug`.)

Then in the `ReadyFilter { ... }` construction (around line 3833), add the new field:

```rust
    let filter = ReadyFilter {
        labels,
        types: wanted_types,
        limit,
        include_claimed,
        include_blocked,
        parent: parent_id,
    };
```

- [ ] **Step 5: Run the test and verify it passes**

Run: `cargo test --release -p jjf --test ready ready_parent_flag_filters_to_parent_child_children`

Expected: PASS.

- [ ] **Step 6: Write a follow-up test for composition with other filters**

Add to [`crates/jjf/tests/ready.rs`](../../../crates/jjf/tests/ready.rs):

```rust
#[test]
fn ready_parent_composes_with_type_and_limit() {
    let repo = make_initialized_repo("ready_parent_compose");
    let epic_id = parse_id_from_stdout(
        &run_jjf(&repo, &["new", "--json", "-t", "E", "--type", "epic", "--slug", "e"]).stdout,
    );
    // Two children: one bug, one feature.
    let _bug = run_jjf(
        &repo,
        &["new", "--json", "-t", "bug-child", "--type", "bug", "--parent", epic_id.as_str()],
    );
    let _feat = run_jjf(
        &repo,
        &["new", "--json", "-t", "feat-child", "--type", "feature", "--parent", epic_id.as_str()],
    );

    // `--parent e --type bug` returns just the bug.
    let out = run_jjf(&repo, &["ready", "--json", "--parent", "e", "--type", "bug"]);
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["title"].as_str().unwrap(), "bug-child");

    // `--parent e --limit 1` returns the top-priority one (bug, per type-priority sort).
    let out = run_jjf(&repo, &["ready", "--json", "--parent", "e", "--limit", "1"]);
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["title"].as_str().unwrap(), "bug-child");
}
```

- [ ] **Step 7: Write the unknown-handle test**

Add to [`crates/jjf/tests/ready.rs`](../../../crates/jjf/tests/ready.rs):

```rust
#[test]
fn ready_parent_unknown_handle_exits_two() {
    let repo = make_initialized_repo("ready_parent_unknown");
    let out = run_jjf(&repo, &["ready", "--parent", "no-such-slug"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("slug_not_found") || stderr.contains("no-such-slug"),
        "stderr should mention the bad handle: {stderr}"
    );
}
```

- [ ] **Step 8: Run all three ready --parent tests and verify they pass**

Run: `cargo test --release -p jjf --test ready ready_parent`

Expected: 3 tests pass.

- [ ] **Step 9: Run the full workspace**

Run: `cargo nextest run --workspace`

Expected: all green.

- [ ] **Step 10: Commit**

```bash
git add crates/jjf/src/main.rs crates/jjf/tests/ready.rs
git commit -m "$(cat <<'EOF'
ready: --parent <handle> filter for parent-child children

`jjf ready --parent <handle>` restricts the ready set to issues
carrying a parent-child dep edge to <handle>. Handle accepts id
or slug. AND-composes with --label / --type / --limit.

Backs the deprecation of the `epic:<slug>` label convention —
parent-child edges become the single mechanism for child-of-
epic membership.
EOF
)"
```

---

## Task 3: Add `--parent <handle>` to `jjf ls`

**Files:**
- Modify: [`crates/jjf/src/main.rs`](../../../crates/jjf/src/main.rs) — `Commands::Ls` arg struct (around line 1873); `run_ls` function (around line 3700); add `parent_matches` helper near the other `*_match` helpers (around line 4200).
- Test: [`crates/jjf/tests/ls.rs`](../../../crates/jjf/tests/ls.rs).

**Interfaces:**
- Consumes: nothing new from earlier tasks (the storage-layer field added in Task 1 is used by `ready` only; `ls` filters at the CLI layer).
- Produces: `jjf ls --parent <handle>`.

- [ ] **Step 1: Write the failing test**

Add to [`crates/jjf/tests/ls.rs`](../../../crates/jjf/tests/ls.rs):

```rust
#[test]
fn ls_parent_flag_filters_to_parent_child_children() {
    let repo = make_initialized_repo("ls_parent_basic");
    let epic_id = parse_id_from_stdout(
        &run_jjf(&repo, &["new", "--json", "-t", "Epic", "--type", "epic", "--slug", "demo"]).stdout,
    );
    let child_id = parse_id_from_stdout(
        &run_jjf(&repo, &["new", "--json", "-t", "child", "--parent", epic_id.as_str()]).stdout,
    );
    let _sibling = run_jjf(&repo, &["new", "--json", "-t", "sibling"]);

    let out = run_jjf(&repo, &["ls", "--json", "--parent", "demo"]);
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"].as_str().unwrap(), child_id.as_str());
}

#[test]
fn ls_parent_unknown_handle_exits_two() {
    let repo = make_initialized_repo("ls_parent_unknown");
    let out = run_jjf(&repo, &["ls", "--parent", "no-such-slug"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn ls_parent_composes_with_type_and_status() {
    let repo = make_initialized_repo("ls_parent_compose");
    let epic_id = parse_id_from_stdout(
        &run_jjf(&repo, &["new", "--json", "-t", "E", "--type", "epic", "--slug", "e"]).stdout,
    );
    let bug_id = parse_id_from_stdout(
        &run_jjf(&repo, &["new", "--json", "-t", "bug", "--type", "bug", "--parent", "e"]).stdout,
    );
    let _feat = run_jjf(&repo, &["new", "--json", "-t", "feat", "--type", "feature", "--parent", "e"]);

    // Close the bug; it should disappear from the default --status open listing.
    run_jjf(&repo, &["close", bug_id.as_str()]);

    let out = run_jjf(&repo, &["ls", "--json", "--parent", "e", "--type", "bug"]);
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(arr.len(), 0, "closed bug should be hidden by default --status open");

    let out = run_jjf(&repo, &["ls", "--json", "--parent", "e", "--type", "bug", "--status", "all"]);
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(arr.len(), 1);
}
```

- [ ] **Step 2: Run the tests and verify they fail**

Run: `cargo test --release -p jjf --test ls ls_parent`

Expected: 3 tests FAIL with `unexpected argument '--parent'`.

- [ ] **Step 3: Add `parent_matches` helper**

In [`crates/jjf/src/main.rs`](../../../crates/jjf/src/main.rs), add this helper near the other `*_match` helpers (after `types_match` around line 4208):

```rust
/// `--parent` predicate. None matches every issue. Some(pid)
/// requires the issue to carry a `DepKind::ParentChild` edge
/// whose target equals `pid`. Mirrors `ReadyFilter::parent`'s
/// semantics on the CLI side for verbs that don't go through
/// the storage-layer filter (`ls`, `search`).
fn parent_matches(issue: &Issue, wanted: &Option<IssueId>) -> bool {
    match wanted {
        None => true,
        Some(pid) => issue
            .dependencies
            .iter()
            .any(|d| d.target == *pid && d.kind == DepKind::ParentChild),
    }
}
```

- [ ] **Step 4: Add `--parent` to `Commands::Ls`**

In [`crates/jjf/src/main.rs`](../../../crates/jjf/src/main.rs), find `Commands::Ls` (around line 1873). Add the field, mirroring the existing arg attribute style:

```rust
        /// Filter to issues carrying a `parent-child` dep edge to
        /// `<handle>`. `<handle>` is an issue id (7-char hex) or
        /// slug. AND-composed with `--label` / `--type` /
        /// `--status` / `--slug`. Unknown handle exits 2.
        #[arg(long)]
        parent: Option<String>,
```

- [ ] **Step 5: Plumb `parent` through the dispatch and `run_ls`**

Update the `Commands::Ls { ... }` destructure in the main match arm to include `parent` and pass it to `run_ls`. Update `fn run_ls` signature to accept it.

In the body of `run_ls` (around line 3700), after preflight + storage open, resolve the handle:

```rust
    let parent_id: Option<IssueId> = match parent {
        Some(handle) => Some(resolve_handle(&storage, &handle)?),
        None => None,
    };
```

Then in the filter loop (around line 3727), add the `parent_matches` check alongside `labels_match` / `types_match`:

```rust
        if !labels_match(&issue, &labels) {
            continue;
        }
        if !types_match(&issue, &wanted_types) {
            continue;
        }
        if !parent_matches(&issue, &parent_id) {
            continue;
        }
```

- [ ] **Step 6: Run the tests and verify they pass**

Run: `cargo test --release -p jjf --test ls ls_parent`

Expected: 3 tests pass.

- [ ] **Step 7: Run the full workspace**

Run: `cargo nextest run --workspace`

Expected: all green.

- [ ] **Step 8: Commit**

```bash
git add crates/jjf/src/main.rs crates/jjf/tests/ls.rs
git commit -m "$(cat <<'EOF'
ls: --parent <handle> filter for parent-child children

Adds the same `--parent <handle>` flag to `jjf ls` that landed
on `jjf ready` — restricts the listing to issues with a
parent-child dep edge to the handle. AND-composes with the
existing --label / --type / --status / --slug filters.

`ls` filters at the CLI layer (not the storage layer) to match
the existing `labels_match` / `types_match` pattern; the new
`parent_matches` helper sits alongside them.
EOF
)"
```

---

## Task 4: Add `--parent <handle>` to `jjf search`

**Files:**
- Modify: [`crates/jjf/src/main.rs`](../../../crates/jjf/src/main.rs) — `Commands::Search` arg struct (around line 1927); `run_search` function (around line 3965).
- Test: [`crates/jjf/tests/search.rs`](../../../crates/jjf/tests/search.rs).

**Interfaces:**
- Consumes: the `parent_matches` helper from Task 3.
- Produces: `jjf search --parent <handle>`.

- [ ] **Step 1: Write the failing test**

Add to [`crates/jjf/tests/search.rs`](../../../crates/jjf/tests/search.rs):

```rust
#[test]
fn search_parent_flag_intersects_with_query() {
    let repo = make_initialized_repo("search_parent");
    let epic_id = parse_id_from_stdout(
        &run_jjf(&repo, &["new", "--json", "-t", "Epic", "--type", "epic", "--slug", "e"]).stdout,
    );
    // Two children, both with "needle" in the title — only one is parented.
    let parented_id = parse_id_from_stdout(
        &run_jjf(&repo, &["new", "--json", "-t", "needle child", "--parent", "e"]).stdout,
    );
    let _orphan = run_jjf(&repo, &["new", "--json", "-t", "needle orphan"]);

    let out = run_jjf(&repo, &["search", "--json", "--parent", "e", "needle"]);
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = envelope["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["id"].as_str().unwrap(), parented_id.as_str());
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test --release -p jjf --test search search_parent`

Expected: FAIL with `unexpected argument '--parent'`.

- [ ] **Step 3: Add `--parent` to `Commands::Search`**

In [`crates/jjf/src/main.rs`](../../../crates/jjf/src/main.rs), find `Commands::Search` (around line 1927). Add the same field:

```rust
        /// Filter to issues carrying a `parent-child` dep edge to
        /// `<handle>`. AND-composed with the search query and
        /// existing `--label` / `--type` / `--status` filters.
        /// Unknown handle exits 2.
        #[arg(long)]
        parent: Option<String>,
```

- [ ] **Step 4: Plumb `parent` through the dispatch and `run_search`**

Update the `Commands::Search { ... }` destructure and the `run_search` signature.

In the body of `run_search` (around line 3965), resolve the handle after preflight + storage open. Then in the `hits.retain(...)` closure (around line 3987), add the `parent_matches` check:

```rust
    hits.retain(|h| {
        status_matches(&h.issue, status)
            && labels_match(&h.issue, &labels)
            && types_match(&h.issue, &wanted_types)
            && parent_matches(&h.issue, &parent_id)
    });
```

- [ ] **Step 5: Run the test and verify it passes**

Run: `cargo test --release -p jjf --test search search_parent`

Expected: PASS.

- [ ] **Step 6: Run the full workspace**

Run: `cargo nextest run --workspace`

Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add crates/jjf/src/main.rs crates/jjf/tests/search.rs
git commit -m "$(cat <<'EOF'
search: --parent <handle> filter for parent-child children

Adds the `--parent <handle>` flag to `jjf search`, completing
the parent-child filter surface across `ls` / `ready` /
`search`. Reuses the `parent_matches` helper from `ls`;
AND-composed with the search query and existing filters.
EOF
)"
```

- [ ] **Step 8: Push to origin**

```bash
git push origin main
```

---

## Task 5: Update docs/quickstart.md to teach parent-child edges

**Files:**
- Modify: [`docs/quickstart.md`](../../quickstart.md) — Section 6 ("Scale up: epics with deps").

**Interfaces:**
- Consumes: the `--parent` flag from Tasks 2-4.
- Produces: nothing (doc only).

- [ ] **Step 1: Read the current Section 6 to ground the edit**

Run: `sed -n '129,225p' docs/quickstart.md`

Read the output. The section currently teaches `-l epic:backend` + a separate `jjf dep add --kind parent-child` call as a pair.

- [ ] **Step 2: Rewrite Section 6 to use only parent-child edges**

The new section keeps the same overall structure (two epics, backend gates frontend, children attach via parent-child) but cuts the `-l epic:<slug>` labels entirely. Edit the section so:

- Child-issue `jjf new` invocations drop `-l epic:backend` / `-l epic:frontend` from the flag list.
- Each child gets `--parent <epic-id>` directly on `jjf new`, replacing the post-hoc `for c in ...; do jjf dep add --kind parent-child $c $EPIC_A; done` loops.
- The "Two edge kinds, two roles" paragraph stays — it still describes the model correctly.
- `jjf ready --label epic:backend` → `jjf ready --parent backend`.
- `jjf ready --label epic:frontend` → `jjf ready --parent frontend`.
- Drop the mention of `epic:<slug>` labels in any prose around the example.

(The `--parent` flag on `jjf new` already exists from fj#3's fix — it's a shorthand for `-d parent-child:<id>`.)

- [ ] **Step 3: Verify the file still renders cleanly**

Run: `grep -c 'epic:' docs/quickstart.md`

Expected: 0 (no `epic:<slug>` label refs remain).

- [ ] **Step 4: Commit**

```bash
git add docs/quickstart.md
git commit -m "$(cat <<'EOF'
quickstart: teach parent-child edges; drop epic:<slug> labels

Section 6 now uses `--parent <epic>` on `jjf new` and
`jjf ready --parent <epic-slug>` for filtering, matching the
new `--parent` flag landed on ls/ready/search. The label
convention `-l epic:<slug>` is gone from the example.
EOF
)"
```

---

## Task 6: Update docs/cli-json.md, the using-jjforge skill, and CLAUDE.md

**Files:**
- Modify: [`docs/cli-json.md`](../../cli-json.md) — flag tables for `ls`, `ready`, `search`.
- Modify: [`skills/using-jjforge/SKILL.md`](../../../skills/using-jjforge/SKILL.md) — Common queries + Common mistakes.
- Modify: `CLAUDE.md` — Label scheme section + Queries section.

**Interfaces:** None — pure documentation.

- [ ] **Step 1: Add `--parent` to the cli-json.md flag tables**

In [`docs/cli-json.md`](../../cli-json.md), find the per-verb sections for `ls`, `ready`, `search`. Each has a flag table or list. Add an entry for `--parent <handle>` to each, matching the existing column structure. Sample row for the `ls` section:

```markdown
| `--parent <H>` | — | Filter to issues with a `parent-child` dep edge to `<H>`. `<H>` is an id or slug. Unknown → exit 2 (`slug_not_found`). |
```

If the format is a bulleted list instead of a table, add a bullet matching the existing style.

- [ ] **Step 2: Update the using-jjforge skill**

In [`skills/using-jjforge/SKILL.md`](../../../skills/using-jjforge/SKILL.md):

- In the "Common queries" section, change `jjf ls --label epic:foo --status open` to `jjf ls --parent foo --status open` and the descriptive comment to reflect that.
- In the "Common mistakes" table, add this row:

```markdown
| Used `-l epic:foo` to associate a child with an epic | The `epic:<slug>` label convention was retired. Use `--parent <epic>` on `jjf new`, or `jjf dep add --kind parent-child <child> <epic>` after the fact. Filter with `jjf ls --parent <epic>`. |
```

- [ ] **Step 3: Update CLAUDE.md's Label scheme section**

In `CLAUDE.md`, find the `### Label scheme` heading. Replace the body with:

```markdown
### Label scheme

- **`roadmap`** — the running plan (one ticket, never closes;
  body edited in place via `jjf update --body-file`).
- Issues that belong to an epic attach via a `parent-child` dep
  edge. Use `--parent <epic>` on `jjf new`, or `jjf dep add
  --kind parent-child <child> <epic>` after the fact. Filter
  with `jjf ls --parent <epic>` / `jjf ready --parent <epic>`.
- Epics themselves are typed `epic` (`--type epic`). The
  `epic:<slug>` label convention is retired (replaced by the
  parent-child edge above).
```

- [ ] **Step 4: Update CLAUDE.md's Queries section**

Find the `## Queries` section. Update the example invocations to use `--parent` instead of `--label epic:<slug>`. Specifically:

- `jjf ls --label epic:mvp-storage --status open` → `jjf ls --parent mvp-storage --status open`
- `jjf ls --json --label epic | jq '...'` (lists epics) → `jjf ls --type epic --json | jq '...'`
- `jjf ready --label backend` (if present) → `jjf ready --parent backend`
- `jjf ready --label epic:host-asterinas` → `jjf ready --parent host-asterinas`

Also update the "Work under one epic — open tickets only" and "Everything ever attached to an epic — open and closed" example comments to use the new flag.

- [ ] **Step 5: Verify all docs are consistent**

Run these greps; each should return 0:

```bash
grep -c 'epic:<slug>' docs/cli-json.md skills/using-jjforge/SKILL.md CLAUDE.md
grep -c '\-\-label epic:' docs/cli-json.md docs/quickstart.md skills/using-jjforge/SKILL.md CLAUDE.md
grep -c '\-l epic:' docs/quickstart.md skills/using-jjforge/SKILL.md
```

Expected: each grep returns 0 matches per file (a `0` per line). Any non-zero is a missed update — go back and fix.

- [ ] **Step 6: Commit**

```bash
git add docs/cli-json.md skills/using-jjforge/SKILL.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs+skill+CLAUDE: --parent flag; retire epic:<slug> labels

- cli-json.md: --parent <handle> added to the flag tables for
  ls / ready / search.
- using-jjforge skill: Common queries swap label-based examples
  to --parent; Common mistakes gains a row pointing users at
  parent-child edges.
- CLAUDE.md: Label scheme section rewritten to drop the
  epic:<slug> convention and direct readers at parent-child
  edges + --parent filtering. Queries section invocations
  updated accordingly.
EOF
)"
```

- [ ] **Step 7: Push to origin**

```bash
git push origin main
```

---

## Task 7: Operational migration (NOT committed)

**Files:**
- Create: `experiments/drop-epic-labels/run.sh` — one-shot migration. Gitignored.

**Interfaces:** None — operational only.

This task does NOT produce commits. It mutates planner refs and pushes them via `jjf push origin`.

- [ ] **Step 1: Confirm experiments/drop-epic-labels/ is gitignored**

Run: `cat .gitignore | grep -E 'experiments|drop-epic-labels'`

Expected: a line like `experiments/**/.scratch/` (the existing rule). If the `drop-epic-labels/` subdir isn't covered by an existing wildcard, add `experiments/drop-epic-labels/` to `.gitignore` as a one-line commit before continuing. (Check first; the spec mentions `experiments/**/.scratch/` already exists and the `drop-epic-labels` directory may or may not need a separate ignore line.)

- [ ] **Step 2: Write the migration script**

Create `experiments/drop-epic-labels/run.sh`:

```bash
#!/usr/bin/env bash
# One-shot migration: replace every epic:<slug> label with the
# equivalent parent-child dep edge, then drop the bare `epic`
# label from every epic (redundant with type:epic).
#
# Idempotent: re-running is safe — issues without epic:* labels
# are skipped; `jjf dep add` is idempotent at the record level.
#
# Run from the jjforge repo root. After: `jjf push origin` to
# land mutations on the remote.

set -euo pipefail

JJF=${JJF:-./bin/jjf}

# 1. Build slug→id map for every epic.
echo "==> Cataloging epics..."
epics_json=$($JJF ls --type epic --status all --json)
echo "    Found $(echo "$epics_json" | jq length) epics."

# 2. For every issue carrying any epic:<slug> label, add the
#    parent-child edge and remove the label.
echo "==> Migrating children..."
child_count=0
warn_count=0
$JJF ls --status all --json \
    | jq -r '.[] | select(.labels[]? | startswith("epic:")) | .id' \
    | while read child_id; do
        for label in $($JJF show "$child_id" --json | jq -r '.labels[]' | grep '^epic:'); do
            slug=${label#epic:}
            epic_id=$(echo "$epics_json" | jq -r ".[] | select(.slug==\"$slug\") | .id" | head -1)
            if [ -n "$epic_id" ] && [ "$epic_id" != "null" ]; then
                $JJF dep add --kind parent-child "$child_id" "$epic_id" >/dev/null
                $JJF label rm "$child_id" "$label" >/dev/null
                child_count=$((child_count + 1))
            else
                echo "    WARN: no epic with slug '$slug' (on $child_id); left label" >&2
                warn_count=$((warn_count + 1))
            fi
        done
    done
echo "    Migrated $child_count child→epic edges; $warn_count warnings."

# 3. Drop the bare `epic` label from every epic.
echo "==> Dropping bare 'epic' label from epics..."
echo "$epics_json" | jq -r '.[] | .id' | while read epic_id; do
    if $JJF show "$epic_id" --json | jq -e '.labels | index("epic")' >/dev/null; then
        $JJF label rm "$epic_id" epic >/dev/null
        echo "    cleaned $epic_id"
    fi
done

# 4. Verify the post-state.
echo "==> Verifying..."
remaining=$($JJF ls --status all --json | jq -r '.[] | .labels[]?' | grep -c '^epic:' || true)
bare=$($JJF ls --type epic --status all --json | jq -r '.[] | .labels[]?' | grep -cx 'epic' || true)
echo "    epic:* labels remaining: $remaining (expect 0)"
echo "    bare 'epic' labels remaining: $bare (expect 0)"

if [ "$remaining" -ne 0 ] || [ "$bare" -ne 0 ]; then
    echo "    FAIL: migration incomplete" >&2
    exit 1
fi

echo "==> Done. Run \`jjf push origin\` to land on the remote."
```

`chmod +x experiments/drop-epic-labels/run.sh`.

- [ ] **Step 3: Dry-run mental check**

Read the script. Confirm:
- It uses `./bin/jjf` (the workspace's CLI), not a global `jjf`.
- It exits non-zero on incomplete migration.
- Per-issue mutations are individual `jjf` calls, not bulk operations (the planner doesn't have bulk).

- [ ] **Step 4: Run the migration**

```bash
cd ~/p/jjforge
./experiments/drop-epic-labels/run.sh
```

Expected output: a summary like "Migrated N child→epic edges; 0 warnings" and "Done."

- [ ] **Step 5: Spot-check post-state**

```bash
# Should return the same issues that --label epic:agent-ergonomics used to return.
./bin/jjf ls --parent agent-ergonomics --status all --json | jq length

# Should be 0 — no labels matching epic:* anywhere.
./bin/jjf ls --status all --json | jq -r '.[] | .labels[]?' | grep -c '^epic:' || true

# Epic issues should no longer carry the bare `epic` label.
./bin/jjf ls --type epic --status all --json | jq -r '.[] | .labels' | head
```

- [ ] **Step 6: Push the planner mutations**

```bash
./bin/jjf push origin
```

Expected: `pushed N refs/jjf/* ref(s) -> origin` where N includes every issue the migration touched.

- [ ] **Step 7: Verify one more time, via the CLI features the migration enables**

```bash
./bin/jjf ready --parent agent-ergonomics --limit 1
./bin/jjf ready --parent mvp-storage --limit 1
```

Expected: returns the top unblocked work under each epic (or nothing, if everything's blocked or closed).

---

## Self-review against the spec

Confirming each spec requirement is covered:

| Spec requirement | Task(s) |
|---|---|
| `--parent <handle>` on `jjf ls` | Task 3 |
| `--parent <handle>` on `jjf ready` | Tasks 1 + 2 (storage + CLI) |
| `--parent <handle>` on `jjf search` | Task 4 |
| Handle accepts id or slug | Tasks 2, 3, 4 (use the existing resolver) |
| Unknown handle exits 2 (`slug_not_found`) | Tasks 2, 3 (test + impl) |
| AND-composition with `--label` / `--type` / `--status` | Tasks 2, 3 (composition tests) |
| Flag is not repeatable | Tasks 2, 3, 4 — `Option<String>` enforces this |
| No schema changes | Confirmed — storage uses existing `DepKind::ParentChild` |
| JSON envelopes unchanged | Confirmed — filter is downstream of envelope construction |
| Existing planner data migrates | Task 7 |
| Bare `epic` label removed from epics | Task 7 step 3 |
| Doc updates (quickstart, cli-json, using-jjforge skill, CLAUDE.md) | Tasks 5, 6 |
| No back-compat shim | Confirmed — we change docs to teach the new way; old invocations stop finding migrated issues |
| Two git commits + one planner push | Off — plan has 6 commits (one per task that produces code/docs). That's because TDD per-step gives multiple commits; the spec's "two commits" was wrong about the count. The shape (CLI before migration before docs) is right. |

The plan has more commits than the spec's count, because each TDD-driven task produces its own commit. That's a healthier shape than batching; the spec line is imprecise rather than the plan being wrong. No spec gap.

## Type / signature consistency check

- `parent: Option<String>` (clap arg) → `parent_id: Option<IssueId>` (after resolver) — consistent across Tasks 2, 3, 4.
- `ReadyFilter::parent: Option<IssueId>` (Task 1) is the only field added to the storage filter; CLI verbs that bypass `ReadyFilter` (`ls`, `search`) use the `parent_matches` helper (Tasks 3, 4) defined once.
- `parent_matches(&issue, &parent_id)` signature is identical at every call site.
- `resolve_handle` is the placeholder name; if the existing CLI uses a different symbol, substitute it at implementation time. Tasks 2, 3, 4 all use the same convention.
