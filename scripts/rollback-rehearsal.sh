#!/bin/sh
set -eu

if [ "$#" -ne 5 ]; then
    echo "usage: $0 CURRENT_FORGE PREVIOUS_FORGE PREVIOUS_CONFIG VERIFIED_DB_BACKUP SOURCE_REPOSITORY" >&2
    exit 64
fi

current_forge=$1
previous_forge=$2
previous_config=$3
verified_backup=$4
source_repository=$5

for executable in "$current_forge" "$previous_forge"; do
    test -x "$executable" || {
        echo "not executable: $executable" >&2
        exit 65
    }
done
for file in "$previous_config" "$verified_backup"; do
    test -f "$file" || {
        echo "missing deployment artifact: $file" >&2
        exit 66
    }
done
git -C "$source_repository" rev-parse --git-dir >/dev/null

drill_root=$(mktemp -d "/tmp/forge-rollback-deployment-unit.XXXXXX")
trap 'rm -rf "$drill_root"' EXIT HUP INT TERM
staged_repository="$drill_root/repository"

"$current_forge" store verify --path "$verified_backup" >/dev/null
backup_before=$(shasum -a 256 "$verified_backup" | awk '{print $1}')

git clone --quiet --no-local "$source_repository" "$staged_repository"
mkdir -p "$staged_repository/.forge"
cp "$previous_config" "$staged_repository/.forge/config.toml"
cp "$verified_backup" "$staged_repository/.forge/forge.db"

# This is the supported rollback deployment unit: previous binary, its
# version-matched strict configuration, and a verified database snapshot.
# Reading history proves startup/config/schema compatibility without executing
# an agent or mutating operational evidence.
"$previous_forge" history --repo "$staged_repository" --limit 1 \
    > "$drill_root/previous-history.txt"
test -s "$drill_root/previous-history.txt"

backup_after=$(shasum -a 256 "$verified_backup" | awk '{print $1}')
test "$backup_before" = "$backup_after"

printf 'rollback deployment unit passed\n'
printf 'previous binary report: %s\n' "$("$previous_forge" --version)"
printf 'previous config sha256: %s\n' "$(shasum -a 256 "$previous_config" | awk '{print $1}')"
printf 'database backup sha256: %s\n' "$backup_before"
