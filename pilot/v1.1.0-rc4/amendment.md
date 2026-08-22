# Forge v1.1.0 RC4 qualification amendment

RC4 is a new qualification stratum. It preserves RC1, RC2, and RC3 as
historical evidence and does not reinterpret their results.

RC3 stopped after four of nine live attempts. Two fd tasks passed, the third
could not run because the provider account reported insufficient credit, and
the first httpx task exposed a production-class containment defect: Codex
attempted to start Bubblewrap inside Forge's capability-free OCI container and
could not create a nested user namespace.

RC4 defines Forge's hardened OCI container as the production security boundary
for Codex and invokes Codex's documented externally-sandboxed mode rather than
weakening the outer boundary. The container remains read-only, capability-free,
no-new-privileges, resource-limited, and restricted to the candidate worktree,
read-only Git metadata, private tmpfs HOME/tmp, and the configured network.
Docker's init shim forwards signals and reaps descendants. No Docker socket,
host home, primary worktree, arbitrary host path, privileged mode, broad
capability, or provider credential is available to evaluators.

RC4 also requires production provider wrappers. Claude runs in bare mode and
receives its API key through a private one-shot apiKeyHelper whose source file
is removed before model-directed tools can run. Codex logs in only inside the
private ephemeral HOME and starts after its credential environment variable is
unset. Forge redacts invocation credentials from all subprocess output and,
before durable capture, scans tracked, untracked, ignored, binary, symlink, and
path-name candidate evidence for exact credential values. Any match is a typed
credential-policy failure and forces workspace destruction even when failure
retention was requested.

The same nine task intents, repository commits, evaluator plans, explicit agent
assignments, 0.05 routing threshold, recommendation-only routing, human merge
policy, and acceptance gates remain unchanged. All nine live tasks must restart
at zero under RC4. No RC3 candidate work is imported.

RC4 additionally contains fixes found during the production audit: bounded
container control-plane calls; complete process-group cancellation and timeout
cleanup; timeout/cancellation-safe verdict derivation; hardened host Git
commands and linked-worktree validation; no-clobber SQLite backup publication;
exclusive restore locking; and an exact production-representative live provider
probe. Full details and test evidence belong in the RC4 manifest and final
qualification report.
