#!/usr/bin/env bash
# Claude-only Forge-on-Forge dogfooding.
#
# Runs a task through the Forge control plane — isolated worktree, independent
# evaluation, persisted trajectory — rather than letting an agent edit the
# repository directly. That is the entire point: this exercises Forge, not
# Claude.
#
# Nothing is merged. The run leaves a candidate branch and a report; accepting
# it is a separate, explicit human decision.
#
# Usage: validation/scripts/run-dogfood.sh T-VAL-021 [T-VAL-022 ...]
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FORGE="${FORGE_BIN:-$ROOT/target/release/forge}"
SESSION="${SESSION:-$(date -u +%Y%m%dT%H%M%SZ)}"
# Raw output goes to the gitignored archive, never to validation/results/,
# which is public campaign material a participant clone will contain.
ARCHIVE="${FORGE_CAMPAIGN_ARCHIVE:-$ROOT/.forge/validation-archive}"
OUT="$ARCHIVE/dogfood-$SESSION"
CAP=5

if [ "$#" -eq 0 ]; then
    echo "usage: $0 <task-id> [task-id ...]" >&2
    exit 2
fi
if [ "$#" -gt "$CAP" ]; then
    echo "refusing: $# tasks exceeds the pre-campaign dogfood cap of $CAP" >&2
    echo "the deliverable of this stage is campaign readiness, not results" >&2
    exit 2
fi

if [ ! -x "$FORGE" ]; then
    (cd "$ROOT" && cargo build --release --bin forge) || exit 1
fi

# The agent works from a commit. Uncommitted changes are invisible to it, and
# a dirty tree makes the base commit a poor description of what was attempted.
if ! git -C "$ROOT" diff --quiet || ! git -C "$ROOT" diff --cached --quiet; then
    echo "refusing: working tree is dirty" >&2
    echo "agents work from a commit; commit or stash first" >&2
    exit 2
fi

mkdir -p "$OUT"

# Record the environment before anything runs. Configuration drift is only
# analysable if it was captured at the time.
{
    echo "{"
    echo "  \"session\": \"$SESSION\","
    echo "  \"tier\": \"dogfood\","
    echo "  \"agents\": [\"claude\"],"
    echo "  \"base_commit\": \"$(git -C "$ROOT" rev-parse HEAD)\","
    echo "  \"forge_version\": \"$("$FORGE" --version 2>&1 | head -1)\","
    echo "  \"claude_version\": \"$(claude --version 2>&1 | head -1)\","
    echo "  \"codex_version\": \"$(codex --version 2>&1 | head -1 || echo 'unavailable')\","
    echo "  \"recorded_at\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\""
    echo "}"
} > "$OUT/environment.json"

echo "session   $SESSION"
echo "output    ${OUT#"$ROOT"/}"
echo "base      $(git -C "$ROOT" rev-parse --short HEAD)"
echo

status=0
for id in "$@"; do
    task="$ROOT/validation/tasks/$id.yaml"
    if [ ! -f "$task" ]; then
        echo "no such task: $id (retired tasks are listed in campaign.yaml)" >&2
        status=1
        continue
    fi

    echo "=== $id ==="
    # --keep-workspace so the candidate is inspectable afterwards. A dogfood run
    # whose workspace was torn down cannot be reviewed.
    "$FORGE" run "$task" --agent claude --keep-workspace 2>&1 \
        | tee "$OUT/$id.report.txt"
    rc="${PIPESTATUS[0]}"
    echo "$id exit=$rc" >> "$OUT/exit-codes.txt"
    echo
done

# Raw evidence is primary. Export after every session so the ledger state that
# produced these reports is captured alongside them.
"$FORGE" export --format jsonl > "$OUT/export.jsonl" 2>/dev/null || true

echo "---"
echo "reports and export written to ${OUT#"$ROOT"/} (private archive)"
echo "NOTHING WAS MERGED. Candidate branches are forge/<run-id>; review before accepting."
exit "$status"
