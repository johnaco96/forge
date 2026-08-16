#!/usr/bin/env bash
# End-to-end Tier 1 post-campaign analysis. This never invokes a coding agent.
set -euo pipefail

ROOT=$(git rev-parse --show-toplevel)
ARCHIVE=${1:-"$ROOT/.forge/validation-archive"}
SESSIONS=${2:?usage: $0 [archive] <codex-sessions> [output]}
OUTPUT=${3:-"$ROOT/validation/analysis/tier1"}
MASTER="$ARCHIVE/tier1-master.jsonl"
ACCOUNTING="$OUTPUT/codex-accounting.jsonl"

cargo build -p forge-accounting --manifest-path "$ROOT/Cargo.toml"
python3 "$ROOT/validation/scripts/enrich-tier1-accounting.py" \
  --binary "$ROOT/target/debug/forge-accounting" \
  --archive "$ARCHIVE" \
  --sessions "$SESSIONS" \
  --output "$ACCOUNTING"
python3 "$ROOT/validation/scripts/analyze-tier1.py" \
  --repo "$ROOT" \
  --campaign "$ROOT/validation/campaign.yaml" \
  --master "$MASTER" \
  --archive "$ARCHIVE" \
  --accounting "$ACCOUNTING" \
  --output "$OUTPUT"
