#!/bin/sh
# Instantiates a fixture as a fresh Git repository, ready for `forge run`.
#
#   fixtures/new-fixture-repo.sh median /tmp/smoke
#
# Used for smoke-testing a real agent against a small, controlled target.
set -eu

fixture="${1:?usage: new-fixture-repo.sh <fixture-name> <destination>}"
dest="${2:?usage: new-fixture-repo.sh <fixture-name> <destination>}"
here=$(cd "$(dirname "$0")" && pwd)
src="$here/test-repositories/$fixture"

[ -d "$src" ] || { echo "no such fixture: $fixture" >&2; exit 1; }

rm -rf "$dest"
mkdir -p "$dest"
cp -R "$src/." "$dest/"

git -C "$dest" init --quiet --initial-branch=main
git -C "$dest" config user.email "forge-fixture@example.invalid"
git -C "$dest" config user.name "Forge Fixture"
git -C "$dest" add -A
git -C "$dest" commit --quiet -m "initial commit"

echo "$dest"
