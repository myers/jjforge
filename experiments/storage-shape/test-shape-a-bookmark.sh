#!/usr/bin/env bash
# Shape A: Bookmark-branch. Issues are markdown files on a dedicated
# jj bookmark `bugs`. Operations are commits to that bookmark.
#
# Tests the dcd4b57 questions for this shape:
#   - Does it survive `jj fetch` cleanly when two clones diverge?
#   - Does it survive `jj git push` to a vanilla git remote?
#   - Does it produce a clean merge, or conflict markers?
#   - Can a human/agent inspect bug history with stock jj commands?
#   - How awkward is it to mutate from a different bookmark?

set -u

SCRATCH="$(cd "$(dirname "$0")" && pwd)/.scratch/shape-a"
RUNS_DIR="$(cd "$(dirname "$0")" && pwd)/runs"
mkdir -p "$RUNS_DIR"
TRANSCRIPT="$RUNS_DIR/shape-a.transcript.txt"
exec > >(tee "$TRANSCRIPT") 2>&1

rm -rf "$SCRATCH"
mkdir -p "$SCRATCH"
cd "$SCRATCH"

banner() { printf "\n===== %s =====\n" "$*"; }

jjA() { ( cd "$SCRATCH/alice" && jj "$@" ); }
jjB() { ( cd "$SCRATCH/bob" && jj "$@" ); }

banner "SHAPE A: bookmark-branch (dedicated 'bugs' bookmark)"

# --- 1. set up bare git remote ---
banner "(1) set up bare git remote, seed with empty main"
git init --bare --initial-branch=main remote.git
git init --initial-branch=main seed
(
  cd seed
  git config user.email seed@example.com
  git config user.name seed
  git commit --allow-empty -m "seed"
  git remote add origin "$SCRATCH/remote.git"
  git push -u origin main
)

# --- 2. clone twice ---
banner "(2) jj git clone twice (alice, bob)"
jj git clone "$SCRATCH/remote.git" alice
jj git clone "$SCRATCH/remote.git" bob

# Set per-repo identity so commits are pushable.
jjA config set --repo user.name alice
jjA config set --repo user.email alice@example.com
jjB config set --repo user.name bob
jjB config set --repo user.email bob@example.com

# --- 3. alice creates a bug on `bugs` bookmark ---
banner "(3) alice: create bug aa6600b on a fresh 'bugs' bookmark; push"
jjA new "root()" -m "jjf: bug aa6600b - create

Jjf-Op: create
Jjf-Bug: aa6600b
Jjf-Title: first bug"
mkdir -p "$SCRATCH/alice/bugs"
cat > "$SCRATCH/alice/bugs/aa6600b.json" <<'EOF'
{"title":"first bug","status":"open","comments":[]}
EOF
jjA st
ALICE_C1=$(jjA log --no-graph -r @ -T 'change_id ++ "\n"' | head -n1)
echo "alice change at @ = $ALICE_C1"
jjA bookmark create bugs -r @
jjA new "root()"
jjA git push --bookmark bugs --allow-new
echo "alice's bookmarks after push:"
jjA bookmark list --all

# --- 4. bob fetches and sees the bug ---
banner "(4) bob: fetch and inspect"
jjB git fetch
echo "bob's bookmarks (should show bugs@origin):"
jjB bookmark list --all
echo "bob's log of bugs@origin:"
jjB log -r 'bugs@origin' -T 'change_id.short() ++ " " ++ description.first_line() ++ "\n"' --no-graph
echo "bob's per-file history at root:bugs/aa6600b.json:"
jjB log -r 'all()' 'root:bugs/aa6600b.json' -T 'change_id.short() ++ " " ++ description.first_line() ++ "\n"' --no-graph

# --- 5. concurrent edits ---
banner "(5a) alice: retitle, push"
jjA new bugs -m "jjf: bug aa6600b - set-title

Jjf-Op: set-title
Jjf-Bug: aa6600b
Jjf-Title: alice's title"
cat > "$SCRATCH/alice/bugs/aa6600b.json" <<'EOF'
{"title":"alice's title","status":"open","comments":[]}
EOF
ALICE_C2=$(jjA log --no-graph -r @ -T 'change_id ++ "\n"' | head -n1)
jjA bookmark set bugs -r @ --allow-backwards
jjA new "root()"
jjA git push --bookmark bugs
echo "alice pushed concurrent change $ALICE_C2"

banner "(5b) bob: independently retitle the same bug to a different value"
jjB bookmark track 'bugs@origin'
jjB bookmark list --all
jjB new bugs -m "jjf: bug aa6600b - set-title

Jjf-Op: set-title
Jjf-Bug: aa6600b
Jjf-Title: bob's title"
cat > "$SCRATCH/bob/bugs/aa6600b.json" <<'EOF'
{"title":"bob's title","status":"open","comments":[]}
EOF
BOB_C2=$(jjB log --no-graph -r @ -T 'change_id ++ "\n"' | head -n1)
jjB bookmark set bugs -r @ --allow-backwards
jjB new "root()"
echo "bob created concurrent change $BOB_C2"

banner "(5c) bob fetches alice's push; bookmarks diverge"
jjB git fetch
echo "bob's bookmarks (should show divergence: bugs vs bugs@origin):"
jjB bookmark list --all

banner "(5d) bob tries to push: refused (bookmark conflicted)"
jjB git push --bookmark bugs 2>&1 || echo "(push refused as expected)"

banner "(5e) bob creates a merge commit by selecting both heads via bookmarks() revset"
# Cannot use bare 'bugs' because the name is conflicted; use bookmarks(bugs).
jjB new 'bookmarks(bugs)' -m "jjf: bug aa6600b - merge concurrent titles

Jjf-Op: merge
Jjf-Bug: aa6600b"
echo "@ after merge attempt:"
jjB st
echo "working copy file content (should show jj conflict markers):"
cat "$SCRATCH/bob/bugs/aa6600b.json" || echo "(no file)"
echo "is @ marked as conflict?"
jjB log -r @ -T 'change_id.short() ++ " conflict=" ++ if(conflict, "YES", "no") ++ "\n"' --no-graph

banner "(5f) bob resolves manually (picks alice's title) and pushes"
cat > "$SCRATCH/bob/bugs/aa6600b.json" <<'EOF'
{"title":"alice's title","status":"open","comments":[]}
EOF
jjB st
jjB bookmark set bugs -r @ --allow-backwards
jjB new "root()"
jjB git push --bookmark bugs

banner "(6) op history for the file: stock jj log <path>"
jjA git fetch
echo "alice's view of bugs file history after fetching bob's merge:"
jjA log -r 'all()' 'root:bugs/aa6600b.json' -T 'change_id.short() ++ " " ++ description.first_line() ++ "\n"' --no-graph

banner "(7) mutating from a different bookmark: how awkward?"
echo "alice's @ before mutation:"
jjA log -r @ -T 'change_id.short() ++ " " ++ description.first_line() ++ "\n"' --no-graph
echo "running the 4-CLI dance: new bugs -> write -> bookmark set -> new root()"
jjA new bugs -m "jjf: bug bb7700c - create

Jjf-Op: create
Jjf-Bug: bb7700c
Jjf-Title: another bug"
cat > "$SCRATCH/alice/bugs/bb7700c.json" <<'EOF'
{"title":"another bug","status":"open","comments":[]}
EOF
NEW_C=$(jjA log --no-graph -r @ -T 'change_id ++ "\n"' | head -n1)
jjA bookmark set bugs -r @ --allow-backwards
jjA new "root()"
echo "alice's @ after mutation:"
jjA log -r @ -T 'change_id.short() ++ " empty=" ++ if(empty, "YES", "no") ++ "\n"' --no-graph
jjA git push --bookmark bugs

banner "(8) a different awkwardness: mutating while alice has WIP on @"
echo "alice has uncommitted WIP on @ (a file unrelated to bugs):"
echo "scratch work" > "$SCRATCH/alice/wip.txt"
jjA st
echo "now run the new bugs dance — does WIP survive?"
WIP_CHANGE=$(jjA log -r @ -T 'change_id ++ "\n"' --no-graph | head -n1)
jjA new bugs -m "jjf: bug dd9900e - create

Jjf-Op: create
Jjf-Bug: dd9900e
Jjf-Title: third bug"
echo '{"title":"third bug","status":"open","comments":[]}' > "$SCRATCH/alice/bugs/dd9900e.json"
jjA bookmark set bugs -r @ --allow-backwards
jjA new "root()"
echo "alice's full log now (looking for the wip change):"
jjA log -r 'all() ~ root()' -T 'change_id.short() ++ " files=" ++ if(empty, "0", "n") ++ " " ++ description.first_line() ++ "\n"' --no-graph
echo "is wip.txt still findable under the prior change_id $WIP_CHANGE?"
jjA diff --summary -r "$WIP_CHANGE" || true

banner "DONE shape A"
