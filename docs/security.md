# Security and containment

Forge distinguishes workspace isolation from host containment. A Git worktree
keeps ordinary candidate edits away from the primary checkout, but it is not a
security boundary. Production runs require the Docker-compatible OCI boundary.

## Modes

Development compatibility is explicit and visibly unsafe:

    [containment]
    mode = "none"

Supervised production uses fail-closed containment:

    [containment]
    mode = "required"
    runtime = "docker"
    image = "forge-agent-runtime@sha256:<digest>"
    network = "none"
    cpu_millis = 2000
    memory_bytes = 4294967296
    pids_limit = 256
    workspace_limit_bytes = 21474836480
    credential_env = ["CODEX_API_KEY"]

When required mode cannot reach the runtime, image, configured network, or a
credential explicitly requested for the selected agent invocation, Forge
records a typed infrastructure failure and does not fall back to a host
process. The credential list is an allowlist, not a sandbox-wide requirement;
evaluator commands request none.

## Enforced container boundary

Agent and evaluator commands use the same execution abstraction. The container
has a read-only root, all capabilities dropped, no-new-privileges, the current
UID/GID, a private temporary HOME, a bounded temporary directory, CPU and
memory limits, a process limit, timeout cleanup, and active workspace/disk
monitoring. The candidate worktree is the only writable host bind mount;
container-private temporary filesystems remain writable. The repository Git
common directory is mounted read-only so Git inspection works; the primary checkout,
user home, SSH keys, cloud configuration, Codex/Claude configuration, and
arbitrary host paths are not mounted.

Credentials are named explicitly in `credential_env`. Each contained agent
invocation selects only one supported, present alternative and must request it
explicitly. The Docker client receives only its safe connection environment
plus that selected value, and only the named value is injected into the
container. Claude and Codex production wrappers move authentication into the
private ephemeral HOME and unset the credential before model-directed tools
start. Provider output is redacted with the exact invocation value, including
short values. Evaluators run as separate commands with no provider credential.

Before staging or durable patch capture, Forge scans changed tracked,
untracked, ignored, binary, symlink, and path-name evidence for exact configured
credential values. A match is a typed credential-policy violation and forces
destruction of the disposable workspace, even when failure retention was
requested.

Network policies are:

- none: Docker network namespace has no network interface beyond loopback.
- restricted: attach an operator-created Docker network. Forge verifies that
  it exists, but Docker alone does not prove hostname or destination allowlists;
  the operator must enforce egress controls outside Forge.
- allowed: ordinary Docker bridge access. This is explicit and unsuitable for
  tasks that require restricted egress.

The frozen RC4 production profiles use `allowed` because both provider APIs and
reproducible evaluator dependency installation require outbound networking.
Forge does not implement hostname/IP/destination allowlisting. That limitation
is explicitly accepted for supervised v1.1.0 and remains operator-managed; it
does not alter the mount, capability, privilege, credential, or host-home
controls above.

The workspace byte limit is enforced by Forge's active watchdog. Memory OOM is
read from container state and recorded as MemoryLimitExceeded. CPU is a
throttling limit, so Forge does not falsely claim it can identify every
CPU-bound failure as CpuLimitExceeded.

## Known boundaries

The read-only Git common directory exposes this repository's objects and refs.
That is adequate host protection but is not formal campaign isolation. Paired
validation still uses independent clones to prevent one participant from
reading another candidate. OCI containment protects the host; independent
clones protect experimental blindness.

Approved evidence returns through the writable worktree and captured command
output. Container HOME, temporary files, and layers are ephemeral. No automatic
merge, policy promotion, or team dispatch is enabled by containment.

Native Codex uses its own `workspace-write` sandbox by default. Required
production Codex instead uses Forge OCI as the authoritative boundary and the
CLI's documented externally-contained mode, avoiding a nested Bubblewrap
namespace that cannot run inside the capability-free container. This is
reported as `inner sandbox=bypassed, boundary=Forge OCI`; the bypass applies to
the redundant inner sandbox, not to Forge's outer containment.

## Verification

Deterministic unit tests inspect every Docker argument and verify no home or
secret mount is present. The dedicated CI job runs local Alpine fixtures that
attempt host-sentinel writes, host-secret reads, ambient-secret access,
network access, orphan descendants, and memory exhaustion. It consumes no
Claude or Codex usage.

Run locally when Alpine 3.20 is already present:

    cargo test -p forge-executor -- --ignored --nocapture
