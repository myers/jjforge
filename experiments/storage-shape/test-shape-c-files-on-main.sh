#!/usr/bin/env bash
# Shape C: Files on main. Issues are markdown files in `issues/` on
# the main branch (like zfs-workspace). jj is just the VCS that
# happens to be there. Every mutation is a normal commit on main.
#
# Tests the same dcd4b57 questions:
#   - Does it survive `jj fetch` cleanly when two clones diverge?
#   - Does it survive `jj git push` to a vanilla git remote?
#   - Clean merge or conflict markers?
#   - Inspectable with stock jj commands?
#   - How awkward to mutate when @ is on a different working topic?

set -u

SCRATCH="$(cd "$(dirname "$0")" && pwd)/.scratch/shape-c"
RUNS_DIR="$(cd "$(dirname "$0")" && pwd)/runs"
mkdir -p "$RUNS_DIR"
TRANSCRIPT="$RUNS_DIR/shape-c.transcript.txt"
exec > >(tee "$TRANSCRIPT") 2>&1

rm -rf "$SCRATCH"
mkdir -p "$SCRATCH"
cd "$SCRATCH"

banner() { printf "\n===== %s =====\n" "$*"; }

jjA() { ( cd "$SCRATCH/alice" && jj "$@" ); }
jjB() { ( cd "$SCRATCH/bob" && jj "$@" ); }

banner "SHAPE C: files on main (zfs-workspace style)"

# --- 1. set up bare git remote ---
banner "(1) bare remote, seeded with empty main"
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
banner "(2) jj git clone twice"
jj git clone "$SCRATCH/remote.git" alice
jj git clone "$SCRATCH/remote.git" bob
jjA config set --repo user.name alice
jjA config set --repo user.email alice@example.com
jjB config set --repo user.name bob
jjB config set --repo user.email bob@example.com

# --- 3. alice creates a bug as a file on main, pushes ---
banner "(3) alice: create issues/aa6600b.json on main; push main"
jjA new "main" -m "jjf: bug aa6600b - create

Jjf-Op: create
Jjf-Bug: aa6600b
Jjf-Title: first bug"
mkdir -p "$SCRATCH/alice/issues"
cat > "$SCRATCH/alice/issues/aa6600b.json" <<'EOF'
{"title":"first bug","status":"open","comments":[]}
EOF
jjA st
# Move main bookmark forward to @, step @ off.
jjA bookmark set main -r @ --allow-backwards
jjA new "root()"
jjA git push --bookmark main
echo "alice's bookmarks after push:"
jjA bookmark list --all

# --- 4. bob fetches and sees ---
banner "(4) bob: fetch and inspect"
jjB git fetch
echo "bob's main now:"
jjB bookmark list --all
echo "bob's log of the bug file:"
jjB log -r 'all()' 'root:issues/aa6600b.json' -T 'change_id.short() ++ " " ++ description.first_line() ++ "\n"' --no-graph
echo "(Auto-tracking main@origin into local main? jj usually does on fetch.)"

# --- 5. concurrent edits ---
banner "(5a) alice: retitle issue, push main"
jjA new main -m "jjf: bug aa6600b - set-title

Jjf-Op: set-title
Jjf-Bug: aa6600b
Jjf-Title: alice's title"
cat > "$SCRATCH/alice/issues/aa6600b.json" <<'EOF'
{"title":"alice's title","status":"open","comments":[]}
EOF
jjA bookmark set main -r @ --allow-backwards
jjA new "root()"
jjA git push --bookmark main

banner "(5b) bob: independently retitle the same issue"
# Bob still has the pre-push main locally; fetch his old view first to confirm.
jjB new main -m "jjf: bug aa6600b - set-title

Jjf-Op: set-title
Jjf-Bug: aa6600b
Jjf-Title: bob's title"
cat > "$SCRATCH/bob/issues/aa6600b.json" <<'EOF'
{"title":"bob's title","status":"open","comments":[]}
EOF
jjB bookmark set main -r @ --allow-backwards
jjB new "root()"

banner "(5c) bob: fetch alice's push; what does jj say?"
jjB git fetch
echo "bookmarks after fetch:"
jjB bookmark list --all
echo "log of all heads of main:"
jjB log -r 'heads(main | main@origin)' -T 'change_id.short() ++ " " ++ description.first_line() ++ "\n"' --no-graph

banner "(5d) bob tries jj git push to vanilla git remote"
jjB git push --bookmark main 2>&1 || echo "(push refused — main is conflicted)"

banner "(5e) bob: merge concurrent main heads"
# bugs is conflicted, so address heads by ID via bookmarks(main).
jjB new 'bookmarks(main)' -m "jjf: bug aa6600b - merge concurrent titles

Jjf-Op: merge
Jjf-Bug: aa6600b"
echo "@ after merge attempt, status:"
jjB st
echo "file content in working copy:"
cat "$SCRATCH/bob/issues/aa6600b.json"
echo "is @ marked as conflict?"
jjB log -r @ -T 'change_id.short() ++ " conflict=" ++ if(conflict, "YES", "no") ++ "\n"' --no-graph

# Resolve by picking alice's title manually (real workflow: a tool would).
cat > "$SCRATCH/bob/issues/aa6600b.json" <<'EOF'
{"title":"alice's title","status":"open","comments":[]}
EOF
jjB bookmark set main -r @ --allow-backwards
jjB new "root()"
jjB git push --bookmark main
echo "bob pushed resolved main."

banner "(6) op history for the file: stock jj log <path>"
jjA git fetch
echo "alice's view of issues file history after fetching bob's merge:"
jjA log -r 'all()' 'root:issues/aa6600b.json' -T 'change_id.short() ++ " " ++ description.first_line() ++ "\n"' --no-graph

banner "(7) mutating when @ is on a different working topic"
# Pretend alice has unrelated work-in-progress on @ that she's not ready
# to commit. Can she still write a bug commit on main without losing
# her WIP?
echo "alice creates a wip file on @:"
echo "scratch wip" > "$SCRATCH/alice/wip.txt"
jjA st
echo "alice's @ before mutation:"
jjA log -r @ -T 'change_id.short() ++ " " ++ description.first_line() ++ "\n"' --no-graph
# We need to put a new bug commit on main without taking wip.txt with us.
# Strategy: jj new -B @ main  (creates new commit before @, with main as parent)
# Wait - actually the easier mental model: jj new main makes a child of main, but @ moves to it.
# That would carry the WIP if it's snapshotted before. Test:
jjA new main -m "jjf: bug cc8800d - create

Jjf-Op: create
Jjf-Bug: cc8800d
Jjf-Title: yet another"
mkdir -p "$SCRATCH/alice/issues"
cat > "$SCRATCH/alice/issues/cc8800d.json" <<'EOF'
{"title":"yet another","status":"open","comments":[]}
EOF
jjA bookmark set main -r @ --allow-backwards
echo "did wip.txt come along into the bug commit? listing the commit's files:"
jjA log -r @ -T 'change_id.short() ++ "\n"' --no-graph
jjA log -r @ -T '"FILES:\n"' --no-graph
( cd "$SCRATCH/alice" && jj diff -r @ --summary )
jjA new "root()"
echo "alice's @ after, status:"
jjA st

banner "DONE shape C"
