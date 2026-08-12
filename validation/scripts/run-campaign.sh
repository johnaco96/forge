#!/usr/bin/env bash
# Tier 1 paired Claude/Codex campaign — `independent-clone-v1` isolation.
#
# Each participant runs in its own Git repository cloned from the frozen
# baseline, not in a sibling worktree. Worktrees share an object database and a
# ref namespace, so the participant running second can read the first one's
# finished candidate by branch name or by object id. See campaign-clone.sh.
#
# The two participants still solve the identical task from the identical commit;
# only the physical repository differs. Pairing is on
# (campaign_id, task_revision_id, base_commit, agent config fingerprint), never
# on having shared a parent repository.
#
# Usage: validation/scripts/run-campaign.sh [--dry-run] [task-id ...]
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FORGE="${FORGE_BIN:-$ROOT/target/release/forge}"
MANIFEST="$ROOT/validation/campaign.yaml"
CLONE="$ROOT/validation/scripts/campaign-clone.sh"
ARCHIVE="${FORGE_CAMPAIGN_ARCHIVE:-$ROOT/.forge/validation-archive}"
AGENTS="${AGENTS:-claude,codex}"
SESSION="${SESSION:-$(date -u +%Y%m%dT%H%M%SZ)}"
ISOLATION="independent-clone-v1"
DRY=0

[ "${1:-}" = "--dry-run" ] && { DRY=1; shift; }

manifest() { # manifest <top-level-key>
    sed -n "s/^$1: *//p" "$MANIFEST" | head -1 | tr -d '"'
}

if [ ! -x "$FORGE" ]; then
    (cd "$ROOT" && cargo build --release --bin forge) || exit 1
fi

echo "campaign readiness"
ready=0

# --- corpus ----------------------------------------------------------------

if "$ROOT/validation/scripts/validate-corpus.sh" --quiet >/dev/null 2>&1; then
    echo "  ok      corpus validates (schema + taxonomy)"
else
    echo "  BLOCKED corpus does not validate"; ready=1
fi

# --- baseline --------------------------------------------------------------
# Never fall back to HEAD, and never to v1.0.0: that commit could not capture a
# patch from any agent that compiled the project, so a campaign run from it
# would have recorded twenty infrastructure errors and no evidence.

FROZEN="$(manifest baseline_frozen)"
BASELINE="$(manifest baseline_commit)"
BASELINE_TAG="$(manifest baseline_tag)"

if [ "$FROZEN" != "true" ]; then
    echo "  BLOCKED campaign baseline is not frozen (baseline_frozen: ${FROZEN:-unset})"
    echo "          freeze it in campaign.yaml after the hardening release is reviewed"
    ready=1
elif [ -z "$BASELINE" ] || [ "$BASELINE" = "null" ]; then
    echo "  BLOCKED campaign baseline commit is unset"; ready=1
elif ! git -C "$ROOT" cat-file -e "$BASELINE" 2>/dev/null; then
    echo "  BLOCKED baseline $BASELINE does not exist in this repository"; ready=1
else
    echo "  ok      baseline pinned to ${BASELINE_TAG:-$BASELINE}"
fi

# --- agents ----------------------------------------------------------------
# An agent is runnable only when BOTH its adapter is implemented and its CLI is
# on PATH. `forge agent list` reports these in separate columns, and the adapter
# column says "ready" for Codex even when the binary is absent.

for agent in ${AGENTS//,/ }; do
    row="$("$FORGE" agent list 2>/dev/null | grep -E "^[[:space:]]*$agent[[:space:]]")"
    if [ -z "$row" ]; then
        echo "  BLOCKED agent $agent is not a known agent"; ready=1
    elif ! echo "$row" | grep -q 'not implemented' && echo "$row" | grep -q 'found ('; then
        echo "  ok      agent $agent runnable (adapter + CLI)"
    elif echo "$row" | grep -q 'not implemented'; then
        echo "  BLOCKED agent $agent has no adapter"; ready=1
    else
        echo "  BLOCKED agent $agent adapter is ready but its CLI is not on PATH"; ready=1
    fi
done

# --- isolation mechanism ---------------------------------------------------
# Proven deterministically rather than assumed. The old preflight — refusing to
# start when forge/* branches already existed — could not address the real
# problem, which is the branch the *other participant* creates mid-pair.

if [ -x "$ROOT/validation/scripts/test-isolation.sh" ] \
   && "$ROOT/validation/scripts/test-isolation.sh" >/dev/null 2>&1; then
    echo "  ok      $ISOLATION isolation verified"
else
    echo "  BLOCKED isolation self-test failed; participants may not be independent"; ready=1
fi

echo
if [ "$ready" -ne 0 ]; then
    echo "campaign is NOT ready. Nothing was run."
    [ "$DRY" -eq 1 ] && exit 0
    exit 2
fi
if [ "$DRY" -eq 1 ]; then
    echo "dry run: readiness satisfied, nothing executed."
    exit 0
fi

# --- execution -------------------------------------------------------------

OUT="$ARCHIVE/campaign-$SESSION"
WORKSPACES="$OUT/participants"
mkdir -p "$WORKSPACES"

{
    echo "{"
    echo "  \"campaign_id\": \"$(manifest campaign_id)\","
    echo "  \"session\": \"$SESSION\","
    echo "  \"tier\": \"tier-1-paired\","
    echo "  \"isolation_strategy\": \"$ISOLATION\","
    echo "  \"agents\": [\"${AGENTS//,/\", \"}\"],"
    echo "  \"baseline_commit\": \"$BASELINE\","
    echo "  \"baseline_tag\": \"$BASELINE_TAG\","
    echo "  \"source_repository\": \"$ROOT\","
    echo "  \"forge_version\": \"$("$FORGE" --version 2>&1 | head -1)\","
    echo "  \"claude_version\": \"$(claude --version 2>&1 | head -1 || echo unavailable)\","
    echo "  \"codex_version\": \"$(codex --version 2>&1 | head -1 || echo unavailable)\","
    echo "  \"recorded_at\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\""
    echo "}"
} > "$OUT/environment.json"

if [ "$#" -gt 0 ]; then
    ids=("$@")
else
    ids=()
    for f in "$ROOT"/validation/tasks/T-VAL-*.yaml; do ids+=("$(basename "$f" .yaml)"); done
fi

for id in "${ids[@]}"; do
    echo "=== $id ==="
    for agent in ${AGENTS//,/ }; do
        participant="$WORKSPACES/$id-$agent"

        # Fail closed: no agent is started until its repository is verified
        # independent and pinned. campaign-clone.sh exits non-zero otherwise.
        if ! "$CLONE" "$ROOT" "$BASELINE" "$participant" main; then
            echo "  BLOCKED: isolation verification failed for $id/$agent; skipping"
            continue
        fi

        # The task definition travels with the baseline, so both participants
        # read byte-identical task text, evaluators, and protected paths.
        ( cd "$participant" && "$FORGE" run "validation/tasks/$id.yaml" \
              --agent "$agent" --base "$BASELINE" --keep-workspace ) \
            2>&1 | tee "$OUT/$id.$agent.report.txt"

        # Each participant keeps its own ledger. Export it out before the next
        # one starts, so nothing a later participant can reach ever holds an
        # earlier participant's result.
        ( cd "$participant" && "$FORGE" export --format jsonl ) \
            > "$OUT/$id.$agent.export.jsonl" 2>/dev/null || true
        echo
    done
done

echo "---"
echo "evidence archived to $OUT"
echo "aggregate with: validation/scripts/analyze.sh <(cat $OUT/*.export.jsonl)"
