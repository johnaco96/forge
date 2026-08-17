#!/bin/sh
set -e

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: $0 REPOSITORY OUTPUT.tar.gz [--include-provider-streams]" >&2
  exit 2
fi
repo=$(CDPATH= cd -- "$1" && pwd)
output=$2
include_streams=
if [ "$#" -eq 3 ]; then
  include_streams=$3
fi
test ! -e "$output"

script_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
forge_bin=$FORGE_BIN
if [ -z "$forge_bin" ]; then
  forge_bin="$script_root/target/release/forge"
fi
if [ ! -x "$forge_bin" ]; then
  forge_bin="$script_root/target/debug/forge"
fi
test -x "$forge_bin"

stage=$(mktemp -d "/tmp/forge-evidence.XXXXXX")
trap 'rm -rf "$stage"' EXIT HUP INT TERM
mkdir -p "$stage/evidence"
"$forge_bin" --repo "$repo" store backup --output "$stage/evidence/forge.db"
cp "$repo/.forge/config.toml" "$stage/evidence/config.toml"
if [ -d "$repo/.forge/tasks" ]; then
  cp -R "$repo/.forge/tasks" "$stage/evidence/tasks"
fi
if [ -d "$repo/.forge/runs" ]; then
  mkdir -p "$stage/evidence/runs"
  find "$repo/.forge/runs" -type f | while IFS= read -r file; do
    relative=$(printf '%s\n' "$file" | sed "s|^$repo/.forge/runs/||")
    case "$file" in
      */patch.diff|*/checks/*|*/prompt.txt)
        mkdir -p "$stage/evidence/runs/$(dirname "$relative")"
        cp "$file" "$stage/evidence/runs/$relative"
        ;;
      */agent.stdout.log|*/agent.stderr.log)
        if [ "$include_streams" = "--include-provider-streams" ]; then
          mkdir -p "$stage/evidence/runs/$(dirname "$relative")"
          cp "$file" "$stage/evidence/runs/$relative"
        fi
        ;;
    esac
  done
fi
tar -C "$stage" -czf "$output" evidence
printf 'evidence archive created: %s\n' "$output"

