#!/usr/bin/env bash
# Proves the formal campaign's participant isolation, and proves the mechanism
# it replaced was genuinely broken.
#
# Deterministic: synthetic repositories, plain Git, no agent, no credits.
#
# Usage: validation/scripts/test-isolation.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CLONE="$ROOT/validation/scripts/campaign-clone.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/forge-isolation-XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

SENTINEL="FORGE_CAMPAIGN_SECRET_A_12345"
pass=0
fail=0

check() { # check <description> <expected: yes|no> <actual: yes|no>
    if [ "$2" = "$3" ]; then
        printf '  ok    %s\n' "$1"; pass=$((pass + 1))
    else
        printf '  FAIL  %s (expected %s, got %s)\n' "$1" "$2" "$3"; fail=$((fail + 1))
    fi
}
reachable() { git -C "$1" cat-file -e "$2" 2>/dev/null && echo yes || echo no; }
ref_visible() { [ -n "$(git -C "$1" for-each-ref --format='%(refname:short)' | grep -Fx "$2" || true)" ] && echo yes || echo no; }

# --- a source repository standing in for the frozen campaign baseline -------

SRC="$WORK/source"
git init -q "$SRC"
git -C "$SRC" config user.email campaign@forge && git -C "$SRC" config user.name campaign
echo "fn main() {}" > "$SRC/main.rs"
git -C "$SRC" add -A && git -C "$SRC" commit -qm "campaign baseline"
BASELINE="$(git -C "$SRC" rev-parse HEAD)"

# Maintainer history the participants must never see: a prior candidate branch
# that is not an ancestor of the baseline, exactly like forge/R-0003.
git -C "$SRC" checkout -q -b forge/R-0003 "$BASELINE"
echo "HISTORICAL_DOGFOOD_CANDIDATE" > "$SRC/dogfood.rs"
git -C "$SRC" add -A && git -C "$SRC" commit -qm "historical dogfood candidate"
HISTORICAL="$(git -C "$SRC" rev-parse HEAD)"
git -C "$SRC" checkout -q main

echo "baseline   $BASELINE"
echo "historical $HISTORICAL (must never be reachable by a participant)"
echo

# --- 1. the mechanism this replaces, reproduced ----------------------------

echo "the old sibling-worktree approach:"
git -C "$SRC" worktree add -q -b old/A "$WORK/old-A" "$BASELINE"
git -C "$SRC" worktree add -q -b old/B "$WORK/old-B" "$BASELINE"
git -C "$WORK/old-A" config user.email a@forge && git -C "$WORK/old-A" config user.name a
echo "$SENTINEL" > "$WORK/old-A/answer.rs"
git -C "$WORK/old-A" add -A && git -C "$WORK/old-A" commit -qm "A candidate"
OLD_A_CANDIDATE="$(git -C "$WORK/old-A" rev-parse HEAD)"

check "B sees A's branch"                    yes "$(ref_visible "$WORK/old-B" old/A)"
check "B can reach A's candidate object"     yes "$(reachable "$WORK/old-B" "$OLD_A_CANDIDATE")"
leaked="$(git -C "$WORK/old-B" show "${OLD_A_CANDIDATE}:answer.rs" 2>/dev/null || true)"
check "B can read A's answer"                yes "$([ "$leaked" = "$SENTINEL" ] && echo yes || echo no)"
echo "  (these three are expected to hold — they are why the campaign does not use worktrees)"
echo

# --- 2. independent clones -------------------------------------------------

echo "independent-clone-v1:"
A="$WORK/participant-A"
B="$WORK/participant-B"
"$CLONE" "$SRC" "$BASELINE" "$A" >/dev/null || { echo "  FAIL  could not materialize A"; exit 1; }

# A runs first and finishes, exactly as in a sequential pair.
git -C "$A" config user.email a@forge && git -C "$A" config user.name a
git -C "$A" checkout -q -b forge/R-A
echo "$SENTINEL" > "$A/answer.rs"
git -C "$A" add -A && git -C "$A" commit -qm "A candidate"
A_CANDIDATE="$(git -C "$A" rev-parse HEAD)"

# B is materialized only afterwards — the ordering the old preflight could not fix.
"$CLONE" "$SRC" "$BASELINE" "$B" >/dev/null || { echo "  FAIL  could not materialize B"; exit 1; }

check "A and B start from the same baseline" yes \
    "$([ "$(git -C "$A" rev-parse "$BASELINE")" = "$(git -C "$B" rev-parse "$BASELINE")" ] && echo yes || echo no)"
check "B has its own git directory"          yes "$([ -d "$B/.git" ] && echo yes || echo no)"
check "B has no object alternates"           no  "$([ -e "$B/.git/objects/info/alternates" ] && echo yes || echo no)"
check "B has no remote to fetch from"        no  "$([ -n "$(git -C "$B" remote)" ] && echo yes || echo no)"

check "B cannot see A's branch"              no  "$(ref_visible "$B" forge/R-A)"
check "B cannot reach A's candidate object"  no  "$(reachable "$B" "$A_CANDIDATE")"
denied="$(git -C "$B" show "${A_CANDIDATE}:answer.rs" 2>/dev/null || true)"
check "B cannot read A's answer"             no  "$([ "$denied" = "$SENTINEL" ] && echo yes || echo no)"

check "B cannot see maintainer candidate refs" no "$(ref_visible "$B" forge/R-0003)"
check "B cannot reach historical candidate"    no "$(reachable "$B" "$HISTORICAL")"

# A must be equally blind to B, so ordering confers no advantage either way.
git -C "$B" checkout -q -b forge/R-B
echo "SECRET_SOLUTION_FROM_B" > "$B/answer.rs"
git -C "$B" add -A && git -C "$B" commit -qm "B candidate"
B_CANDIDATE="$(git -C "$B" rev-parse HEAD)"
check "A cannot reach B's candidate object"  no  "$(reachable "$A" "$B_CANDIDATE")"

echo
echo "  A candidate $A_CANDIDATE"
echo "  B candidate $B_CANDIDATE"
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
