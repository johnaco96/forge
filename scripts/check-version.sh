#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

python3 - <<'PY'
import json
import pathlib
import subprocess
import tomllib

root = pathlib.Path.cwd()
workspace = tomllib.loads((root / "Cargo.toml").read_text())
expected = workspace["workspace"]["package"]["version"]
metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--no-deps", "--format-version", "1"],
    text=True,
))
wrong = sorted(
    (package["name"], package["version"])
    for package in metadata["packages"]
    if package["version"] != expected
)
if wrong:
    raise SystemExit(f"workspace version is {expected}, mismatched packages: {wrong}")

migrations = sorted((root / "crates/forge-store/migrations").glob("*.sql"))
latest_file = max(int(path.name.split("_", 1)[0]) for path in migrations)
backup_source = (root / "crates/forge-store/src/backup.rs").read_text()
needle = f"pub const LATEST_MIGRATION_VERSION: i64 = {latest_file};"
if needle not in backup_source:
    raise SystemExit(
        f"latest migration file is {latest_file}, but backup compatibility constant differs"
    )
print(f"version consistency: forge {expected}; migration {latest_file}")
PY

cargo build -q -p forge-cli --bin forge
expected=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml","rb"))["workspace"]["package"]["version"])')
reported=$(target/debug/forge --version)
test "$reported" = "forge $expected"

