# Codex CLI contract used by Forge

This records the interface inspected for the Phase 1A adapter. It is a transport
contract, not an evaluation contract: Codex receives the same prompt and
worktree as Claude Code, and Forge judges the resulting patch with the same
pipeline.

## Inspected installation

```text
executable  /Applications/ChatGPT.app/Contents/Resources/codex
version     codex-cli 0.147.0-alpha.6.5
```

The commands inspected were:

```bash
codex --help
codex exec --help
codex exec resume --help
codex exec --version
```

The installed CLI's unknown-option path was also exercised. It printed a usage
error and exited `2`; `codex exec --version` exited `0`. Forge does not attach
provider meaning to a particular nonzero code: it records the exact code and
classifies any completed nonzero process separately from the independently
evaluated Forge outcome.

## Invocation and prompt transport

`codex exec` (alias `codex e`) is the non-interactive command. It accepts the
initial prompt as one positional argument, reads it from stdin when it is
omitted, or reads it explicitly when the argument is `-`.

Forge passes the deterministic shared coding-agent prompt as one direct-process
argument. It is never interpolated into a shell. The process starts in the
assigned Forge worktree and also receives `--cd <worktree>` so Codex's workspace
root is explicit. Forge worktrees are Git repositories, so the adapter does not
use `--skip-git-repo-check`.

The effective command shape is:

```text
codex --sandbox <mode> \
  --ask-for-approval <policy> \
  --cd <forge worktree> \
  [--model <model>] \
  exec [extra args] --json \
  <shared prompt>
```

The inspected `exec --help` text lists `--ask-for-approval`, but release
`0.147.0-alpha.6.5` rejects that flag after `exec` with exit code `2`. The same
flag is accepted before the subcommand, where it is inherited as global
configuration. Forge uses the behavior accepted by the installed binary.

## Permissions and sandboxing

The inspected CLI exposes `read-only`, `workspace-write`, and
`danger-full-access` sandbox modes, plus `untrusted`, `on-request`, and `never`
approval policies.

Forge defaults to `workspace-write` plus `never`. This lets an unattended run
edit and execute inside the Codex workspace sandbox while returning denied
operations to the model instead of waiting for an approval Forge cannot answer.
It does not use `--dangerously-bypass-approvals-and-sandbox`.

These settings can be changed under `[agents.codex]`:

```toml
[agents.codex]
executable = "codex"
model = "gpt-5-codex"
timeout_secs = 1800
sandbox_mode = "workspace-write"
approval_policy = "never"
extra_args = ["--ephemeral"]
```

Security-, workspace-, output-, and model-selection flags cannot be repeated in
`extra_args`; their typed settings are the source of truth for both invocation
and reporting.

Forge reports the Codex sandbox and approval settings as the agent permission
mode. It still reports `host containment: none`: Forge has not put the Codex CLI
process in a container, and a Git worktree is candidate isolation rather than
host containment.

## Output and metadata

Without `--json`, `codex exec` streams progress to stderr and prints the final
message to stdout. `--output-last-message` can also write that message to a
file. Forge uses `--json`, which changes stdout into a JSON Lines event stream.

Documented event types include `thread.started`, `turn.started`,
`turn.completed`, `turn.failed`, `item.*`, and `error`. Item types include agent
messages, reasoning, command executions, file changes, MCP calls, web searches,
and plan updates.

Forge captures stdout and stderr as run artifacts and extracts only fields
present in the stream:

- thread/session ID;
- input, cached-input, cache-write, output, and reasoning-output token counts;
- final agent message as the untrusted self-report;
- terminal event type;
- observed completed-item counts for documented tool item types.

The configured sandbox, approval policy, and requested model (when explicitly
configured) are recorded as invocation metadata. The JSONL stream does not
expose cost, so Forge leaves cost unset. It also does not label the effective
model in the inspected stream, so Forge does not fabricate one when no model was
explicitly requested.

Malformed, partial, or evolving JSONL degrades to less metadata. It never
changes process status and never affects Forge evaluation.

## Timeouts and environment

The existing Forge process runner applies the configured wall-clock timeout,
captures bounded stdout/stderr, and terminates the Codex process group on
timeout. Candidate files already written in the worktree remain available for
normal patch capture and evaluation.

The adapter inherits non-secret environment variables, allows `CODEX_API_KEY`
back through the secret filter, and removes parent Codex session markers. Saved
CLI authentication remains available through the normal home directory.
