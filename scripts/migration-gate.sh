#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

./scripts/check-version.sh
cargo test -p forge-store backup::tests::restore_migrates_a_verified_older_schema_only_in_staging
cargo test -p forge-store sqlite::tests

