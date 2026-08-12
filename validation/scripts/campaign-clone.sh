#!/usr/bin/env bash
# Materializes one formal-campaign participant repository — `independent-clone-v1`.
#
# Forge's ordinary execution isolates runs with Git worktrees, and that is the
# right mechanism for ordinary use: cheap, fast, and enough to keep two runs from
# treading on each other's files. It is not enough for a controlled experiment.
# Sibling worktrees share one object database and one ref namespace, so a
# participant can read another participant's finished candidate:
#
#     $ git -C wt-B show forge/R-A:answer.rs
#     SECRET_SOLUTION_FROM_A_12345
#     $ git -C wt-B cat-file -e <A's candidate sha>   # succeeds
#
# The formal Claude-vs-Codex comparison requires that neither agent can see the
# other's work, so each participant gets its own repository instead.
#
# Two things make that real, and both are easy to get wrong:
#
#   --no-local    A local `git clone` does NOT negotiate a pack. It copies (or
#                 hardlinks) the whole objects directory, so every object in the
#                 source arrives regardless of which refs were asked for.
#                 `--single-branch` filters refs and leaves the objects behind,
#                 which looks isolated and is not: the other participant's
#                 candidate remains reachable by object id. `--no-local` forces
#                 the transport path, and only objects reachable from the
#                 requested ref are transferred.
#
#   remote removal  A participant that keeps `origin` can fetch the refs the
#                 clone declined to take. The remote is removed after checkout.
#
# Usage: campaign-clone.sh <source-repo> <baseline-commit> <dest> [branch]
set -uo pipefail

ISOLATION_STRATEGY="independent-clone-v1"

SOURCE="${1:-}"
BASELINE="${2:-}"
DEST="${3:-}"
BRANCH="${4:-main}"

if [ -z "$SOURCE" ] || [ -z "$BASELINE" ] || [ -z "$DEST" ]; then
    echo "usage: $0 <source-repo> <baseline-commit> <dest> [branch]" >&2
    exit 2
fi
if [ -e "$DEST" ]; then
    echo "refusing: $DEST already exists; a participant never reuses a clone" >&2
    exit 2
fi

fail() { echo "BLOCKED: campaign repository isolation verification failed — $1" >&2; exit 3; }

# --- materialize -----------------------------------------------------------

if ! git clone --no-local --single-branch --branch "$BRANCH" --no-tags \
        "$SOURCE" "$DEST" >/dev/null 2>&1; then
    echo "BLOCKED: could not clone $SOURCE ($BRANCH) into $DEST" >&2
    exit 3
fi

# Pin to the exact baseline. A detached HEAD is deliberate: there is no branch
# for an agent to accidentally follow somewhere else.
git -C "$DEST" cat-file -e "$BASELINE" 2>/dev/null \
    || fail "baseline $BASELINE is not present in the clone"
git -C "$DEST" checkout --quiet --detach "$BASELINE" \
    || fail "could not check out baseline $BASELINE"

# Sever the path back to the source before anything else runs here.
git -C "$DEST" remote remove origin >/dev/null 2>&1 || true
git -C "$DEST" for-each-ref --format='%(refname)' refs/remotes/ \
    | while read -r ref; do git -C "$DEST" update-ref -d "$ref"; done
git -C "$DEST" reflog expire --expire=now --all >/dev/null 2>&1 || true

# --- verify ----------------------------------------------------------------
# Fail closed. Every check below has to hold before an agent is allowed near it.

[ -d "$DEST/.git" ] || fail "$DEST/.git is not a directory; the git dir is shared"

if [ -e "$DEST/.git/objects/info/alternates" ]; then
    fail "clone has object alternates: $(cat "$DEST/.git/objects/info/alternates")"
fi

head_sha="$(git -C "$DEST" rev-parse HEAD)"
[ "$head_sha" = "$BASELINE" ] || fail "HEAD is $head_sha, expected baseline $BASELINE"

if [ -n "$(git -C "$DEST" status --porcelain)" ]; then
    fail "clone is not clean immediately after checkout"
fi

forbidden="$(git -C "$DEST" for-each-ref --format='%(refname:short)' | grep -c '^forge/' || true)"
[ "$forbidden" -eq 0 ] || fail "$forbidden candidate ref(s) named forge/* are visible"

if [ -n "$(git -C "$DEST" remote 2>/dev/null)" ]; then
    fail "clone still has a remote; a participant could fetch withheld refs"
fi

if [ -e "$DEST/.forge/validation-archive" ]; then
    fail "private validation archive is present in the participant clone"
fi

echo "$ISOLATION_STRATEGY $head_sha $DEST"
