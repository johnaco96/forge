#!/usr/bin/env bash
# Deterministic analysis over an exported ledger, following
# validation/analysis-plan.md. Descriptive statistics only.
#
# Usage: validation/scripts/analyze.sh <export.jsonl>
set -uo pipefail

EXPORT="${1:-}"
if [ -z "$EXPORT" ] || [ ! -f "$EXPORT" ]; then
    echo "usage: $0 <export.jsonl>" >&2
    exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "jq is required" >&2
    exit 2
fi

echo "Forge validation analysis"
echo "source: $EXPORT"
echo

# §1 inclusion/exclusion. Exclusions are reported, never silently dropped.
total=$(wc -l < "$EXPORT" | tr -d ' ')
live=$(jq -c 'select(.execution_provenance == "live")' "$EXPORT" | wc -l | tr -d ' ')
campaign=$(jq -c 'select(.execution_provenance == "live")
                  | select(.task.definition.tags // [] | index("validation-campaign"))' \
            "$EXPORT" | wc -l | tr -d ' ')
errored=$(jq -c 'select(.execution_provenance == "live")
                 | select(.task.definition.tags // [] | index("validation-campaign"))
                 | select(.outcome == "errored")' "$EXPORT" | wc -l | tr -d ' ')

echo "Population"
printf '  %-34s %s\n' "records in export" "$total"
printf '  %-34s %s\n' "live provenance" "$live"
printf '  %-34s %s\n' "campaign-tagged (n_attempted)" "$campaign"
printf '  %-34s %s\n' "infrastructure failures (excluded)" "$errored"
printf '  %-34s %s\n' "n_included" "$((campaign - errored))"
echo

if [ "$campaign" -eq 0 ]; then
    echo "No campaign evidence in this export."
    echo "Claude-only dogfood runs are tagged validation-campaign but are NOT"
    echo "paired evidence; see analysis-plan.md §3 before reading them as such."
    exit 0
fi

# §2/§5 per-agent, over included runs only.
echo "Per agent (included runs only)"
printf '  %-10s %5s %5s %7s %10s %10s\n' AGENT N PASS RATE MED_MS INTEGRITY
jq -r 'select(.execution_provenance == "live")
       | select(.task.definition.tags // [] | index("validation-campaign"))
       | select(.outcome != "errored")
       | [ .agent.agent_id,
           (if .outcome == "passed" then 1 else 0 end),
           (.agent_runtime_ms // 0),
           (if (.integrity != null and .integrity.status != "clean") then 1 else 0 end)
         ] | @tsv' "$EXPORT" \
| awk -F'\t' '
    { n[$1]++; pass[$1]+=$2; viol[$1]+=$4; if ($3>0) { rt[$1]=rt[$1]" "$3 } }
    END {
      for (a in n) {
        split(rt[a], v, " "); c=0; delete s
        for (i in v) if (v[i] != "") { s[++c]=v[i]+0 }
        for (i=1;i<=c;i++) for (j=i+1;j<=c;j++) if (s[j]<s[i]) { t=s[i]; s[i]=s[j]; s[j]=t }
        med = (c==0) ? 0 : (c%2 ? s[(c+1)/2] : int((s[c/2]+s[c/2+1])/2))
        printf "  %-10s %5d %5d %6.1f%% %10d %10d\n", a, n[a], pass[a], (pass[a]*100/n[a]), med, viol[a]
      }
    }'
echo

# §5 per category.
echo "PASS by category"
printf '  %-14s %-10s %5s %5s %7s\n' CATEGORY AGENT N PASS RATE
jq -r 'select(.execution_provenance == "live")
       | select(.task.definition.tags // [] | index("validation-campaign"))
       | select(.outcome != "errored")
       | [ (.task.classification.category // "unclassified"),
           .agent.agent_id,
           (if .outcome == "passed" then 1 else 0 end) ] | @tsv' "$EXPORT" \
| awk -F'\t' '{ k=$1"\t"$2; n[k]++; p[k]+=$3 }
    END { for (k in n) { split(k,a,"\t");
        printf "  %-14s %-10s %5d %5d %6.1f%%\n", a[1], a[2], n[k], p[k], (p[k]*100/n[k]) } }' \
| sort
echo

# §4 pairing. A pair requires identical task revision AND base commit.
echo "Paired comparisons (same task_revision_id + base_commit)"
pairs=$(jq -r 'select(.execution_provenance == "live")
       | select(.task.definition.tags // [] | index("validation-campaign"))
       | select(.outcome != "errored")
       | [ (.task_revision_id + "|" + .base_commit), .agent.agent_id ] | @tsv' "$EXPORT" \
| awk -F'\t' '{ seen[$1"\t"$2]=1 } END { for (k in seen) { split(k,a,"\t"); c[a[1]]++ }
    n=0; for (p in c) if (c[p] > 1) n++; print n }')
printf '  %-34s %s\n' "complete pairs" "$pairs"
if [ "$pairs" -eq 0 ]; then
    echo
    echo "  No paired evidence. Win/loss/tie, routing accuracy, and every"
    echo "  agent-comparison statistic require pairs and are NOT reported."
fi
echo

echo "Not computed by this script (require the full campaign):"
echo "  routing coverage/accuracy/regret   analysis-plan.md §7-8"
echo "  context A/B deltas                 analysis-plan.md §9"
echo "  team resource multipliers          analysis-plan.md §10"
echo "  health dimension trends            analysis-plan.md §11  (use: forge health trend)"
echo "  policy candidate vs control        analysis-plan.md §12  (use: forge policy compare)"
