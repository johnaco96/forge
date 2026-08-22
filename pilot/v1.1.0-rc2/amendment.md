# RC2 contained-evaluator qualification amendment

RC1 remains immutable. Its first FD-001 attempt (`fd/.forge/forge.db`, run
`R-0001`) is production qualification stratum 1 and retains the original
overall infrastructure `ERROR`, clean integrity evidence, candidate commit,
workspace, artifacts, and two `CredentialUnavailable` evaluator failures.

The defect was invocation-boundary coupling: the reusable `DockerSandbox`
treated its configured agent credential list as mandatory for every command.
The runner correctly gave independent evaluators a credential-free environment,
but `DockerSandbox::wrap` rejected that environment before the evaluator could
start. RC2 scopes credential requirements to each `ExecRequest`; the sandbox
configuration is only an allowlist. A credential-free request injects none and
does not fail because an agent credential is absent. A credential-bearing
request validates presence, injects only its declared approved names, and
redacts their values. Each command remains a fresh, cleaned OCI container, so
the agent container's environment and private HOME cannot be inherited by the
evaluator container.

All nine tasks restart in fresh clones because this changes the execution
substrate. Reusing the RC1 FD-001 engineering result would mix evidence from
the defective and remediated containment implementations. RC2 therefore keeps
the repositories, exact commits, task bytes, agent assignments, evaluator
definitions, resource limits, retention semantics, manual selection, router
threshold `0.05`, recommendation-only routing, and human merge requirement.

Two necessary operational identity changes are preregistered:

- the OCI digest changes to the RC2 image and fresh repository/branch namespace;
- `minimum_free_percent` increases from 5% to 10%, matching the already proposed
  production archive/off-volume threshold and preventing a live restart at the
  previously observed 8.05% free space.

The remediation remains uncommitted because the qualification request forbids
committing before architectural review. Its immutable local source identity is
the starting commit plus the exact `git diff --binary -- crates` SHA-256 stored
in the RC2 manifest. No methodology field in that manifest may be changed after
the first RC2 model invocation.
