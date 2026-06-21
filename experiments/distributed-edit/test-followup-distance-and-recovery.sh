#!/usr/bin/env bash
# Follow-up probes after test-five-scenarios.sh:
#
# A. Does diff-distance matter? Two edits to DIFFERENT lines that are
#    far apart in the file — does jj's textual auto-merger now succeed?
# B. Are jj's conflict markers machine-parseable enough for an agent
#    to do per-field semantic resolution (last-write-wins, set-union
#    for comments) without a human?

set -u

SCRATCH="$(cd "$(dirname "$0")" && pwd)/.scratch-followup"
RUNS_DIR="$(cd "$(dirname "$0")" && pwd)/runs"
TRANSCRIPT="$RUNS_DIR/followup.transcript.txt"
mkdir -p "$RUNS_DIR"
exec > >(tee "$TRANSCRIPT") 2>&1

rm -rf "$SCRATCH"
mkdir -p "$SCRATCH"
cd "$SCRATCH"

banner() { printf "\n===== %s =====\n" "$*"; }
sub()    { printf "\n--- %s ---\n" "$*"; }

setup_remote_and_clones() {
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

seed_bugs_bookmark() {
  local root="$1"
  ( cd "$root/alice"
    jj new "root()" -m "jjf: seed bugs bookmark" >/dev/null
    mkdir -p bugs
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

prep() { ( cd "$1" && jj new bugs -m "$2" >/dev/null ); }
fin()  { ( cd "$1" && jj bookmark set bugs -r @ --allow-backwards >/dev/null && jj new "root()" >/dev/null ); }

# ==============================================================
# A. distance: title at line 1, status at line 8, separated by
#    immutable padding lines. Alice edits title; bob edits status.
# ==============================================================
banner "A. far-apart edits — does line distance avoid a conflict?"
setup_remote_and_clones "$SCRATCH/A"
seed_bugs_bookmark "$SCRATCH/A"
ROOT="$SCRATCH/A"; BUG=aa01; FILE="bugs/$BUG.md"

write_padded() {
  local path="$1" title="$2" status="$3"
  cat > "$path" <<EOF
title: $title
---
(metadata block)
labels:
priority:
assignee:
---
status: $status
---
comments:
EOF
}

sub "A.create (alice)"
prep "$ROOT/alice" "jjf: bug $BUG - create"
write_padded "$ROOT/alice/$FILE" "shared" "open"
fin "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )
( cd "$ROOT/bob" && jj git fetch >/dev/null )

sub "A.alice edits title only"
prep "$ROOT/alice" "jjf: bug $BUG - set-title"
write_padded "$ROOT/alice/$FILE" "alice changed title" "open"
fin "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )

sub "A.bob edits status only"
prep "$ROOT/bob" "jjf: bug $BUG - set-status"
write_padded "$ROOT/bob/$FILE" "shared" "closed"
fin "$ROOT/bob"

sub "A.bob fetches and merges"
( cd "$ROOT/bob" && jj git fetch )
( cd "$ROOT/bob" && jj new 'bookmarks(bugs)' -m "jjf: bug $BUG - auto-merge far-apart" )
( cd "$ROOT/bob"
  echo "@ conflict?:"
  jj log -r @ -T 'change_id.short() ++ " conflict=" ++ if(conflict, "YES", "no") ++ "\n"' --no-graph
  echo "file content:"
  cat "$FILE" )

# ==============================================================
# B. machine-readability of conflict markers
#    Reuse the SAME-FIELD conflict from S1 (we know it conflicts).
#    Then try to (1) read the conflicted file and (2) `jj resolve` it.
#    Also try `jj file show --conflict` to see if there's a structured
#    form jj will emit.
# ==============================================================
banner "B. conflict markers + resolution affordances"
setup_remote_and_clones "$SCRATCH/B"
seed_bugs_bookmark "$SCRATCH/B"
ROOT="$SCRATCH/B"; BUG=bb02; FILE="bugs/$BUG.json"

sub "B.create"
prep "$ROOT/alice" "jjf: bug $BUG - create"
echo '{"title":"first","status":"open","comments":[]}' > "$ROOT/alice/$FILE"
fin "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )
( cd "$ROOT/bob" && jj git fetch >/dev/null )

sub "B.alice retitles"
prep "$ROOT/alice" "jjf: bug $BUG - set-title alice"
echo '{"title":"alice title","status":"open","comments":[]}' > "$ROOT/alice/$FILE"
fin "$ROOT/alice"
( cd "$ROOT/alice" && jj git push --bookmark bugs >/dev/null )

sub "B.bob retitles"
prep "$ROOT/bob" "jjf: bug $BUG - set-title bob"
echo '{"title":"bob title","status":"open","comments":[]}' > "$ROOT/bob/$FILE"
fin "$ROOT/bob"

sub "B.bob fetches and merges (will conflict)"
( cd "$ROOT/bob" && jj git fetch >/dev/null )
( cd "$ROOT/bob" && jj new 'bookmarks(bugs)' -m "jjf: bug $BUG - merge" >/dev/null )

sub "B.what does jj report about the conflict?"
( cd "$ROOT/bob"
  echo "--- jj status ---"
  jj status
  echo "--- jj resolve --list ---"
  jj resolve --list || true )

sub "B.what does the file look like as written?"
cat "$ROOT/bob/$FILE"

sub "B.can we get the three sides programmatically?"
( cd "$ROOT/bob"
  echo "--- jj file show -r @ <FILE> (working-copy materialized) ---"
  jj file show -r @ "$FILE" || true
  echo
  echo "--- the two parent revs of @ ---"
  jj log -r '@-' --no-graph -T 'change_id.short() ++ " " ++ description.first_line() ++ "\n"'
  echo "--- alice's version (first parent file) ---"
  jj file show -r 'parents(@) ~ ancestors(@-, 1)' "$FILE" 2>&1 || true
  echo "--- attempt: jj file show -r @- (both parents) ---"
  jj file show -r '@-' "$FILE" 2>&1 || true )

sub "B.attempt agent-style resolution: read the conflict, pick a side, write it back"
python3 - "$ROOT/bob/$FILE" <<'PY'
import re, sys, json
p = sys.argv[1]
text = open(p).read()
print("--- raw bytes (repr) ---")
print(repr(text))
# jj's text conflict marker shape:
# <<<<<<< conflict 1 of 1
# +++++++ <change_id> "<desc>"
# <side A content lines>
# %%%%%%% diff from: <base>
# \\\\\\\        to: <other>
# <diff hunk lines, prefixed by ' ', '+', '-'>
# >>>>>>> conflict 1 of 1 ends
#
# We want to demonstrate: an agent can parse this, extract the
# "side A" (plus side) and the "diff to side B" hunk, reconstruct
# side B by applying the diff to the base, and choose a winner.
m = re.search(r'<<<<<<< conflict \d+ of \d+\n(.*?)>>>>>>> conflict \d+ of \d+ ends\n?',
              text, re.S)
if not m:
    print("no jj conflict marker found"); sys.exit(0)
block = m.group(1)
print("--- conflict block ---")
print(block)
# Split on the `%%%%%%%` line that introduces the diff hunk.
parts = re.split(r'%%%%%%%.*?\n\\\\\\\\.*?\n', block, maxsplit=1, flags=re.S)
print("--- parsed parts ---")
for i, part in enumerate(parts):
    print(f"part[{i}]: {part!r}")
# parts[0] is "+++++++ <id> ...\n<side A content>\n"
# parts[1] is the diff hunk for side B (relative to base)
side_a_lines = parts[0].split('\n', 1)[1] if len(parts) > 0 else ''
print("--- side A reconstructed ---")
print(side_a_lines)
# To get side B, replay the diff hunk on the base. The diff shows
# lines prefixed ' ', '-', '+'. The base is the ' ' + '-' lines;
# side B is the ' ' + '+' lines.
side_b_lines = []
for line in parts[1].split('\n'):
    if not line: continue
    if line[0] == ' ': side_b_lines.append(line[1:])
    elif line[0] == '+': side_b_lines.append(line[1:])
    # '-' lines were in the base but not in side B; drop.
print("--- side B reconstructed from diff ---")
print('\n'.join(side_b_lines))
print("--- agent decision: last-write-wins per field; pick side B (bob) for title ---")
chosen = side_b_lines[0]  # only one line in this scenario
print("chosen line:", repr(chosen))
PY

sub "B.resolve the conflict by writing the chosen content"
echo '{"title":"bob title","status":"open","comments":[]}' > "$ROOT/bob/$FILE"
( cd "$ROOT/bob"
  echo "--- after write ---"
  jj status
  echo "--- jj log @ now ---"
  jj log -r @ -T 'change_id.short() ++ " conflict=" ++ if(conflict, "YES", "no") ++ "\n"' --no-graph )

banner "DONE"
