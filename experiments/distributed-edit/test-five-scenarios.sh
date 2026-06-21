#!/usr/bin/env bash
# Distributed-edit behavior on Shape A (bookmark-branch) — the five
# scenarios from issue 8d3e045.
#
# dcd4b57 closed with: test only Shape A. Two clones of a stock bare
# git remote, bug data under bugs/<id>.json on a `bugs` bookmark, push
# via `jj git push --bookmark bugs`.
#
# This run exercises the five scenarios. Each scenario uses its own
# bug id so the runs don't bleed into each other:
#   S1 (aa01) same field, same bug, different values
#   S2 (bb02) different fields, same bug (title vs status)
#   S3 (cc03) identical idempotent edit (same field, same value)
#   S4 (dd04) concurrent `set-status closed`, different content sources
#   S5 (ee05) comment in A while B closes
#
# We re-run S2/S5/S3 against TWO file layouts to see how jj's textual
# auto-merger treats them:
#   - blob: a single JSON object per bug (newline-light)
#   - lines: a key:value-per-line layout, comments appended at EOF
# The shape-a transcript already covered S1 with the blob layout, but
# we re-run it here for a self-contained record.

set -u

SCRATCH="$(cd "$(dirname "$0")" && pwd)/.scratch"
RUNS_DIR="$(cd "$(dirname "$0")" && pwd)/runs"
mkdir -p "$RUNS_DIR"
TRANSCRIPT="$RUNS_DIR/distributed-edit.transcript.txt"
exec > >(tee "$TRANSCRIPT") 2>&1

rm -rf "$SCRATCH"
mkdir -p "$SCRATCH"
cd "$SCRATCH"

banner() { printf "\n===== %s =====\n" "$*"; }
sub()    { printf "\n--- %s ---\n" "$*"; }

# --- setup helpers ---------------------------------------------------

setup_remote_and_clones() {
  # Bare git remote + two jj clones (alice, bob), seeded with a `bugs`
  # bookmark that already exists on the remote so both clones agree on
  # its starting position.
  local root="$1"
  mkdir -p "$root"
  ( cd "$root"
    git init --bare --initial-branch=main remote.git >/dev/null
    git init --initial-branch=main seed >/dev/null
    cd seed
    git config user.email seed@example.com
    git config user.name seed
    git commit --allow-empty -m "seed" >/dev/null
    git remote add origin "$root/remote.git"
    git push -u origin main >/dev/null 2>&1
  )

  jj git clone "$root/remote.git" "$root/alice" >/dev/null
  jj git clone "$root/remote.git" "$root/bob"   >/dev/null
  ( cd "$root/alice"
    jj config set --repo user.name alice
    jj config set --repo user.email alice@example.com )
  ( cd "$root/bob"
    jj config set --repo user.name bob
    jj config set --repo user.email bob@example.com )
}

# Write a bug file at $1 (path) with the blob layout.
write_blob() {
  local path="$1" title="$2" status="$3" comments_json="$4"
  cat > "$path" <<EOF
{"title":"$title","status":"$status","comments":$comments_json}
EOF
}

# Write a bug file at $1 with the lines layout. Comments is a bash array
# expanded into one line each, prefixed "comment: ".
write_lines() {
  local path="$1" title="$2" status="$3"; shift 3
  {
    echo "title: $title"
    echo "status: $status"
    for c in "$@"; do echo "comment: $c"; done
  } > "$path"
}

# Run the 4-CLI dance to commit a mutation:
#   jj new bugs -m "<msg>" ; <writes happen> ; jj bookmark set bugs -r @ ; jj new root()
# Caller writes the file between `prep` and `finalize`.
prep_mutation() {
  local repo="$1" msg="$2"
  ( cd "$repo" && jj new bugs -m "$msg" >/dev/null )
}
finalize_mutation() {
  local repo="$1"
  ( cd "$repo"
    jj bookmark set bugs -r @ --allow-backwards >/dev/null
    jj new "root()" >/dev/null )
}

# Show the bug file's content and the change history for it.
show_state() {
  local repo="$1" file="$2"
  ( cd "$repo"
    echo "--- file content [$repo/$file] ---"
    if [ -f "$file" ]; then cat "$file"; else echo "(missing)"; fi
    echo "--- jj log for $file ---"
    jj log -r 'all()' "root:$file" \
      -T 'change_id.short() ++ " " ++ if(conflict, "(C) ", "    ") ++ description.first_line() ++ "\n"' \
      --no-graph
    echo "--- @ conflict? ---"
    jj log -r @ -T 'change_id.short() ++ " conflict=" ++ if(conflict, "YES", "no") ++ "\n"' --no-graph )
}

# Bootstrap the bugs bookmark on alice and push so bob can fetch it.
seed_bugs_bookmark() {
  local root="$1"
  ( cd "$root/alice"
    jj new "root()" -m "jjf: seed bugs bookmark" >/dev/null
    mkdir -p bugs
    # placeholder file so commits diff cleanly; deleted by first real op
    echo "{}" > bugs/.keep
    jj bookmark create bugs -r @ >/dev/null
    jj new "root()" >/dev/null
    jj git push --bookmark bugs --allow-new >/dev/null
  )
  ( cd "$root/bob"
    jj git fetch >/dev/null
    jj bookmark track 'bugs@origin' >/dev/null 2>&1 || true
  )
}

# --- scenario runner -------------------------------------------------

# Each scenario: prep a fresh test root, seed bookmark, both run the
# initial create commit (alice creates, pushes; bob fetches), then both
# concurrently edit per the scenario, alice pushes, bob fetches → see
# what jj does.

run_scenario() {
  local name="$1" layout="$2"
  banner "SCENARIO $name (layout=$layout)"
  local root="$SCRATCH/$name"
  setup_remote_and_clones "$root"
  seed_bugs_bookmark "$root"
}

# ==============================================================
# SCENARIO 1 — same field, same bug, different values  (blob)
# ==============================================================
run_scenario S1 blob
ROOT="$SCRATCH/S1"; BUG=aa01; FILE="bugs/$BUG.json"

sub "S1.create (alice)"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - create

Jjf-Op: create
Jjf-Bug: $BUG"
write_blob "$ROOT/alice/$FILE" "first" "open" "[]"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )

sub "S1.bob fetches"
( cd "$ROOT/bob" && jj git fetch >/dev/null )

sub "S1.alice retitles → 'alice title'"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - set-title alice

Jjf-Op: set-title
Jjf-Bug: $BUG"
write_blob "$ROOT/alice/$FILE" "alice title" "open" "[]"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )

sub "S1.bob retitles → 'bob title' (without fetching)"
prep_mutation "$ROOT/bob" "jjf: bug $BUG - set-title bob

Jjf-Op: set-title
Jjf-Bug: $BUG"
write_blob "$ROOT/bob/$FILE" "bob title" "open" "[]"
finalize_mutation "$ROOT/bob"

sub "S1.bob fetches → divergent bookmark"
( cd "$ROOT/bob" && jj git fetch )

sub "S1.bob tries to push (should refuse)"
( cd "$ROOT/bob" && jj git push --bookmark bugs 2>&1 || echo "(push refused as expected)" )

sub "S1.bob merges by selecting bookmarks(bugs)"
( cd "$ROOT/bob" && jj new 'bookmarks(bugs)' -m "jjf: bug $BUG - auto-merge attempt

Jjf-Op: merge
Jjf-Bug: $BUG" )
show_state "$ROOT/bob" "$FILE"

# ==============================================================
# SCENARIO 2 — different fields, same bug (title vs status)
# blob layout AND lines layout, to see how the textual merger
# distinguishes them.
# ==============================================================

# ---- S2-blob ----
run_scenario S2-blob blob
ROOT="$SCRATCH/S2-blob"; BUG=bb02; FILE="bugs/$BUG.json"

sub "S2-blob.create"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - create"
write_blob "$ROOT/alice/$FILE" "shared" "open" "[]"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )
( cd "$ROOT/bob" && jj git fetch >/dev/null )

sub "S2-blob.alice edits TITLE only"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - set-title"
write_blob "$ROOT/alice/$FILE" "alice changed title" "open" "[]"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )

sub "S2-blob.bob edits STATUS only"
prep_mutation "$ROOT/bob" "jjf: bug $BUG - set-status"
write_blob "$ROOT/bob/$FILE" "shared" "closed" "[]"
finalize_mutation "$ROOT/bob"

sub "S2-blob.bob fetches and merges"
( cd "$ROOT/bob" && jj git fetch )
( cd "$ROOT/bob" && jj new 'bookmarks(bugs)' -m "jjf: bug $BUG - auto-merge" )
show_state "$ROOT/bob" "$FILE"

# ---- S2-lines ----
run_scenario S2-lines lines
ROOT="$SCRATCH/S2-lines"; BUG=bb02; FILE="bugs/$BUG.md"

sub "S2-lines.create"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - create"
write_lines "$ROOT/alice/$FILE" "shared" "open"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )
( cd "$ROOT/bob" && jj git fetch >/dev/null )

sub "S2-lines.alice edits TITLE only"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - set-title"
write_lines "$ROOT/alice/$FILE" "alice changed title" "open"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )

sub "S2-lines.bob edits STATUS only"
prep_mutation "$ROOT/bob" "jjf: bug $BUG - set-status"
write_lines "$ROOT/bob/$FILE" "shared" "closed"
finalize_mutation "$ROOT/bob"

sub "S2-lines.bob fetches and merges"
( cd "$ROOT/bob" && jj git fetch )
( cd "$ROOT/bob" && jj new 'bookmarks(bugs)' -m "jjf: bug $BUG - auto-merge" )
show_state "$ROOT/bob" "$FILE"

# ==============================================================
# SCENARIO 3 — identical idempotent edit (same field, same value)
# Both set title to "same title". Does jj see one commit or two?
# ==============================================================
run_scenario S3 blob
ROOT="$SCRATCH/S3"; BUG=cc03; FILE="bugs/$BUG.json"

sub "S3.create"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - create"
write_blob "$ROOT/alice/$FILE" "orig" "open" "[]"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )
( cd "$ROOT/bob" && jj git fetch >/dev/null )

sub "S3.alice retitles → 'same title'"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - set-title (alice)

Jjf-Op: set-title
Jjf-Bug: $BUG"
write_blob "$ROOT/alice/$FILE" "same title" "open" "[]"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )

sub "S3.bob retitles → 'same title' (independently, without fetching)"
prep_mutation "$ROOT/bob" "jjf: bug $BUG - set-title (bob)

Jjf-Op: set-title
Jjf-Bug: $BUG"
write_blob "$ROOT/bob/$FILE" "same title" "open" "[]"
finalize_mutation "$ROOT/bob"

sub "S3.bob fetches"
( cd "$ROOT/bob" && jj git fetch )

sub "S3.bob attempts merge — same-value, different commits"
( cd "$ROOT/bob" && jj new 'bookmarks(bugs)' -m "jjf: bug $BUG - merge identical edits" )
show_state "$ROOT/bob" "$FILE"

# Check whether both Jjf-Op trailers exist in history (operations applied
# twice in the audit log, even though the file content is identical).
sub "S3.audit log — count distinct set-title commits"
( cd "$ROOT/bob" && jj log -r 'all()' "root:$FILE" \
    -T 'change_id.short() ++ " " ++ description.first_line() ++ "\n"' \
    --no-graph )

# ==============================================================
# SCENARIO 4 — concurrent `set-status closed` in both
# Same op, same value. Like S3 but for status. Symmetry check.
# ==============================================================
run_scenario S4 blob
ROOT="$SCRATCH/S4"; BUG=dd04; FILE="bugs/$BUG.json"

sub "S4.create"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - create"
write_blob "$ROOT/alice/$FILE" "thebug" "open" "[]"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )
( cd "$ROOT/bob" && jj git fetch >/dev/null )

sub "S4.alice closes"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - set-status closed (alice)

Jjf-Op: set-status
Jjf-Bug: $BUG
Jjf-Status: closed"
write_blob "$ROOT/alice/$FILE" "thebug" "closed" "[]"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )

sub "S4.bob closes (independently, without fetching)"
prep_mutation "$ROOT/bob" "jjf: bug $BUG - set-status closed (bob)

Jjf-Op: set-status
Jjf-Bug: $BUG
Jjf-Status: closed"
write_blob "$ROOT/bob/$FILE" "thebug" "closed" "[]"
finalize_mutation "$ROOT/bob"

sub "S4.bob fetches, merges"
( cd "$ROOT/bob" && jj git fetch )
( cd "$ROOT/bob" && jj new 'bookmarks(bugs)' -m "jjf: bug $BUG - merge identical close" )
show_state "$ROOT/bob" "$FILE"

sub "S4.audit log — both close commits preserved?"
( cd "$ROOT/bob" && jj log -r 'all()' "root:$FILE" \
    -T 'change_id.short() ++ " " ++ description.first_line() ++ "\n"' \
    --no-graph )

# ==============================================================
# SCENARIO 5 — comment in A while B closes
# blob layout AND lines layout. Comment is an append; status is an
# overwrite. We want to know whether jj's auto-merge keeps the
# comment without conflict.
# ==============================================================

# ---- S5-blob ----
run_scenario S5-blob blob
ROOT="$SCRATCH/S5-blob"; BUG=ee05b; FILE="bugs/$BUG.json"

sub "S5-blob.create"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - create"
write_blob "$ROOT/alice/$FILE" "bug5" "open" "[]"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )
( cd "$ROOT/bob" && jj git fetch >/dev/null )

sub "S5-blob.alice adds a comment"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - add-comment

Jjf-Op: add-comment
Jjf-Bug: $BUG"
write_blob "$ROOT/alice/$FILE" "bug5" "open" '[{"author":"alice","body":"hello"}]'
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )

sub "S5-blob.bob closes"
prep_mutation "$ROOT/bob" "jjf: bug $BUG - set-status closed

Jjf-Op: set-status
Jjf-Bug: $BUG
Jjf-Status: closed"
write_blob "$ROOT/bob/$FILE" "bug5" "closed" "[]"
finalize_mutation "$ROOT/bob"

sub "S5-blob.bob fetches and merges"
( cd "$ROOT/bob" && jj git fetch )
( cd "$ROOT/bob" && jj new 'bookmarks(bugs)' -m "jjf: bug $BUG - auto-merge comment-vs-close" )
show_state "$ROOT/bob" "$FILE"

# ---- S5-lines ----
run_scenario S5-lines lines
ROOT="$SCRATCH/S5-lines"; BUG=ee05l; FILE="bugs/$BUG.md"

sub "S5-lines.create"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - create"
write_lines "$ROOT/alice/$FILE" "bug5" "open"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )
( cd "$ROOT/bob" && jj git fetch >/dev/null )

sub "S5-lines.alice appends a comment line"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - add-comment

Jjf-Op: add-comment
Jjf-Bug: $BUG"
write_lines "$ROOT/alice/$FILE" "bug5" "open" "alice: hello there"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )

sub "S5-lines.bob edits status only"
prep_mutation "$ROOT/bob" "jjf: bug $BUG - set-status closed

Jjf-Op: set-status
Jjf-Bug: $BUG
Jjf-Status: closed"
write_lines "$ROOT/bob/$FILE" "bug5" "closed"
finalize_mutation "$ROOT/bob"

sub "S5-lines.bob fetches and merges"
( cd "$ROOT/bob" && jj git fetch )
( cd "$ROOT/bob" && jj new 'bookmarks(bugs)' -m "jjf: bug $BUG - auto-merge comment-vs-close" )
show_state "$ROOT/bob" "$FILE"

# ==============================================================
# SCENARIO 6 — both add DIFFERENT comments (lines layout)
# A real "both append" test: alice adds comment A; bob adds comment B.
# Order matters. Does jj's textual merge keep both or conflict?
# ==============================================================
run_scenario S6-lines lines
ROOT="$SCRATCH/S6-lines"; BUG=ff06; FILE="bugs/$BUG.md"

sub "S6-lines.create (with one initial comment so EOF anchor differs)"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - create"
write_lines "$ROOT/alice/$FILE" "bug6" "open" "seed: original report"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )
( cd "$ROOT/bob" && jj git fetch >/dev/null )

sub "S6-lines.alice appends comment A"
prep_mutation "$ROOT/alice" "jjf: bug $BUG - add-comment alice"
write_lines "$ROOT/alice/$FILE" "bug6" "open" \
    "seed: original report" "alice: i can reproduce"
finalize_mutation "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )

sub "S6-lines.bob appends comment B"
prep_mutation "$ROOT/bob" "jjf: bug $BUG - add-comment bob"
write_lines "$ROOT/bob/$FILE" "bug6" "open" \
    "seed: original report" "bob: i found a fix"
finalize_mutation "$ROOT/bob"

sub "S6-lines.bob fetches and merges"
( cd "$ROOT/bob" && jj git fetch )
( cd "$ROOT/bob" && jj new 'bookmarks(bugs)' -m "jjf: bug $BUG - auto-merge two comments" )
show_state "$ROOT/bob" "$FILE"

banner "DONE"
