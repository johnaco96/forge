#!/usr/bin/env bash
# Schema-validate the campaign corpus through Forge itself, then check the
# campaign's own controlled vocabulary (which forge-core deliberately does not
# enforce — classification values are repository-defined strings).
#
# Usage: validation/scripts/validate-corpus.sh [--quiet]
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TASKS="$ROOT/validation/tasks"
FORGE="${FORGE_BIN:-$ROOT/target/release/forge}"
QUIET="${1:-}"

if [ ! -x "$FORGE" ]; then
    echo "building forge..." >&2
    (cd "$ROOT" && cargo build --release --bin forge) || exit 1
fi

CATEGORIES="debugging feature refactor testing performance persistence"
DIFFICULTIES="small medium hard"
fail=0
count=0

for file in "$TASKS"/T-VAL-*.yaml; do
    count=$((count + 1))
    name="$(basename "$file")"

    if ! out="$("$FORGE" task validate "$file" 2>&1)"; then
        echo "FAIL  $name — schema"
        echo "$out" | sed 's/^/      /'
        fail=1
        continue
    fi

    # Campaign vocabulary. Forge accepts any string here; the campaign does not.
    cat_val="$(grep -A4 '^classification:' "$file" | sed -n 's/^  category: *//p')"
    dif_val="$(grep -A4 '^classification:' "$file" | sed -n 's/^  difficulty: *//p')"

    if ! echo "$CATEGORIES" | grep -qw "$cat_val"; then
        echo "FAIL  $name — category '$cat_val' is not in the campaign taxonomy"
        fail=1
        continue
    fi
    if ! echo "$DIFFICULTIES" | grep -qw "$dif_val"; then
        echo "FAIL  $name — difficulty '$dif_val' is not in the campaign taxonomy"
        fail=1
        continue
    fi
    if ! grep -q 'validation-campaign' "$file"; then
        echo "FAIL  $name — missing the validation-campaign tag"
        fail=1
        continue
    fi
    if ! grep -q '^protected_paths:' "$file"; then
        echo "FAIL  $name — no protected paths; the campaign definition must be protected"
        fail=1
        continue
    fi

    [ "$QUIET" = "--quiet" ] || echo "ok    $name  ($cat_val/$dif_val)"
done

echo "---"
if [ "$fail" -eq 0 ]; then
    echo "$count task(s) valid: schema + campaign taxonomy"
else
    echo "corpus has failures"
fi
exit "$fail"
