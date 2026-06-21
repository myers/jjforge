#!/usr/bin/env bash
# Shape B: Side-channel jj operation type. Custom data attached
# directly to jj operations.
#
# This is a feasibility probe, not a working implementation, because
# jj does not expose an extension point for custom op types
# (confirmed in 2130de1: no hooks, no third-party backend
# registration, no plugin API). The empirical question that
# remains: even if we could attach data, would it propagate to
# clones?
#
# This script confirms the second half — the op store is local-only
# — by reproducing the cleanest version of a60bb95's experiment:
# run a bunch of mutations in one clone, clone it, count ops, see
# what's missing.

set -u

SCRATCH="$(cd "$(dirname "$0")" && pwd)/.scratch/shape-b"
RUNS_DIR="$(cd "$(dirname "$0")" && pwd)/runs"
mkdir -p "$RUNS_DIR"
TRANSCRIPT="$RUNS_DIR/shape-b.transcript.txt"
exec > >(tee "$TRANSCRIPT") 2>&1

rm -rf "$SCRATCH"
mkdir -p "$SCRATCH"
cd "$SCRATCH"

banner() { printf "\n===== %s =====\n" "$*"; }
jjA() { ( cd "$SCRATCH/alice" && jj "$@" ); }
jjB() { ( cd "$SCRATCH/bob" && jj "$@" ); }

banner "SHAPE B: side-channel jj op type (feasibility probe)"

banner "(0) sanity: does jj have a public way to record a custom op type?"
echo "subcommands of 'jj op' (looking for create/insert/extend):"
jj op --help 2>&1 | sed -n '/^Commands:/,/^Options:/p' | sed 's/^/  /'
echo "(Note: 'integrate' is a recovery tool for orphaned ops, not a"
echo " way to write custom op metadata. There is no 'jj op create',"
echo " no 'jj op annotate', no plugin API.)"

banner "(1) bare remote, jj git clone twice"
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
jj git clone "$SCRATCH/remote.git" alice
jj git clone "$SCRATCH/remote.git" bob
jjA config set --repo user.name alice
jjA config set --repo user.email alice@example.com
jjB config set --repo user.name bob
jjB config set --repo user.email bob@example.com

banner "(2) alice does several mutations: 5 distinct jj transactions"
# Each of these creates an op in the local op log AND emits an
# accompanying snapshot op.
jjA new "main" -m "create bug aa6600b"
mkdir -p "$SCRATCH/alice/bugs"
echo '{"title":"bug a"}' > "$SCRATCH/alice/bugs/aa6600b.json"
jjA describe -m "create bug aa6600b - now with title"
jjA bookmark create bugs -r @
jjA new "main" -m "create bug bb7700c"
echo '{"title":"bug b"}' > "$SCRATCH/alice/bugs/bb7700c.json"

banner "(3) count ops in alice's op log"
ALICE_OPS=$(jjA op log --no-graph --limit 100 -T 'id ++ "\n"' | wc -l | tr -d ' ')
echo "alice ops: $ALICE_OPS"
echo "first 5 alice op descriptions:"
jjA op log --no-graph --limit 5 -T 'id.short() ++ "  " ++ description ++ "\n"'

banner "(4) push, fetch, count ops in bob"
jjA git push --bookmark bugs --allow-new --bookmark main 2>&1 | tail -3 || true
jjB git fetch
BOB_OPS=$(jjB op log --no-graph --limit 100 -T 'id ++ "\n"' | wc -l | tr -d ' ')
echo "bob ops: $BOB_OPS"
echo "first 5 bob op descriptions:"
jjB op log --no-graph --limit 5 -T 'id.short() ++ "  " ++ description ++ "\n"'

banner "(5) verdict: are alice's transaction ops in bob's op log?"
echo "Compare counts: alice=$ALICE_OPS, bob=$BOB_OPS"
echo "If bob_ops << alice_ops, the op log is local-only — confirming"
echo "a60bb95's finding that 'jj op log is not the audit surface.'"
echo "If we attached side-channel data to jj ops in alice's clone,"
echo "none of it would reach bob via jj fetch."

banner "(6) what about jj op log content? still local-flavored?"
echo "alice's full op log:"
jjA op log --no-graph -T 'id.short() ++ "  " ++ description ++ "  tags=" ++ tags ++ "\n"' --limit 20

banner "DONE shape B (feasibility probe)"
