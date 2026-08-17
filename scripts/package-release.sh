#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: $0 PATH_TO_FORGE OUTPUT_DIRECTORY" >&2
  exit 2
fi
binary=$1
output=$2
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

test -x "$binary"
mkdir -p "$output"
version=$("$binary" --version | awk '{print $2}')
expected=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml","rb"))["workspace"]["package"]["version"])')
test "$version" = "$expected"
commit=$(git rev-parse HEAD)
platform=$(uname -s | tr '[:upper:]' '[:lower:]')
architecture=$(uname -m)
latest_migration=$(python3 -c 'import pathlib,re; s=pathlib.Path("crates/forge-store/src/backup.rs").read_text(); print(re.search(r"LATEST_MIGRATION_VERSION: i64 = ([0-9]+)", s).group(1))')
name="forge-$version-$platform-$architecture"
stage=$(mktemp -d "/tmp/forge-release.XXXXXX")
trap 'rm -rf "$stage"' EXIT HUP INT TERM
mkdir -p "$stage/$name"
cp "$binary" "$stage/$name/forge"

python3 - "$stage/$name/RELEASE-METADATA.json" "$version" "$commit" "$platform" "$architecture" "$latest_migration" <<'PY'
import json
import pathlib
import sys

path, version, commit, platform, architecture, latest_migration = sys.argv[1:]
metadata = {
    "artifact_schema_version": 1,
    "version": version,
    "commit": commit,
    "platform": platform,
    "architecture": architecture,
    "latest_migration": int(latest_migration),
    "sandbox_runtime": "Docker-compatible OCI",
}
pathlib.Path(path).write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n")
PY

archive="$output/$name.tar.gz"
epoch=$(git show -s --format=%ct HEAD)
python3 - "$stage" "$name" "$archive" "$epoch" <<'PY'
import gzip
import pathlib
import tarfile
import sys

stage, name, archive, epoch = sys.argv[1:]
root = pathlib.Path(stage) / name
with open(archive, "wb") as raw:
    with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=int(epoch)) as compressed:
        with tarfile.open(fileobj=compressed, mode="w") as output:
            for path in [root, *sorted(root.rglob("*"))]:
                relative = pathlib.Path(name) / path.relative_to(root)
                info = output.gettarinfo(str(path), arcname=str(relative))
                info.uid = info.gid = 0
                info.uname = info.gname = "root"
                info.mtime = int(epoch)
                if path.is_file():
                    with path.open("rb") as source:
                        output.addfile(info, source)
                else:
                    output.addfile(info)
PY
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$archive" > "$output/SHA256SUMS"
else
  shasum -a 256 "$archive" > "$output/SHA256SUMS"
fi
printf '%s\n' "$archive"
