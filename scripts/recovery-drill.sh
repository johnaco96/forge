#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"
cargo build -q -p forge-cli --bin forge
forge_bin="$repo_root/target/debug/forge"

drill_root=$(mktemp -d "/tmp/forge-recovery.XXXXXX")
trap 'rm -rf "$drill_root"' EXIT HUP INT TERM
repo="$drill_root/repository"
mkdir -p "$repo"
git -C "$repo" init --quiet
git -C "$repo" config user.email forge@example.invalid
git -C "$repo" config user.name "Forge Recovery Drill"
printf 'fixture\n' > "$repo/README.md"
git -C "$repo" add README.md
git -C "$repo" commit --quiet -m fixture

"$forge_bin" --repo "$repo" init
backup="$drill_root/forge-backup.db"
"$forge_bin" --repo "$repo" store backup --output "$backup"
"$forge_bin" store verify --path "$backup"

python3 - "$repo/.forge/forge.db" <<'PY'
import sqlite3
import sys
with sqlite3.connect(sys.argv[1]) as db:
    db.execute(
        "INSERT INTO counters(name, value) VALUES('recovery_drill_mutation', 1)"
    )
PY

restore_repo="$drill_root/restore-target"
mkdir -p "$restore_repo"
git -C "$restore_repo" init --quiet
git -C "$restore_repo" config user.email forge@example.invalid
git -C "$restore_repo" config user.name "Forge Recovery Drill"
printf 'restore target\n' > "$restore_repo/README.md"
git -C "$restore_repo" add README.md
git -C "$restore_repo" commit --quiet -m fixture
"$forge_bin" --repo "$restore_repo" init
"$forge_bin" --repo "$restore_repo" store restore --from "$backup" --force
"$forge_bin" --repo "$restore_repo" store verify
python3 - "$restore_repo/.forge/forge.db" <<'PY'
import sqlite3
import sys
with sqlite3.connect(sys.argv[1]) as db:
    count = db.execute(
        "SELECT COUNT(*) FROM counters WHERE name='recovery_drill_mutation'"
    ).fetchone()[0]
assert count == 0, "post-backup mutation leaked into restored store"
PY
printf 'recovery drill passed: %s\n' "$drill_root"
