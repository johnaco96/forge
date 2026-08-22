# Qualification status

The deterministic local release and operational gates are green, and both
required job-scoped credentials were present without being printed or
persisted. All three frozen pilot doctor profiles passed repository, Forge
version, schema, capacity, pinned OCI image, resource policy, credential
presence, and both in-image CLI version checks.

The supervised pilot is **stopped fail-closed after 1 of 9 tasks**. Claude
completed `F-PILOT-FD-001`, but Forge's independent test and lint evaluators
could not start. Evaluators intentionally use a credential-free environment;
the reused required-containment sandbox nevertheless required the agent's
`ANTHROPIC_API_KEY` for every contained process. The ledger records two
`CredentialUnavailable` infrastructure failures, an inconclusive evaluation,
and an overall `ERROR`. The remaining eight tasks were not attempted.

The run retained its clean-integrity patch, provider stream, prompt, ledger,
and failed workspace. Credential-value scans found no matches, and the run
container was removed. No merge, policy promotion, automatic dispatch, tag,
push, or publication occurred.

Actual-pilot recovery is **PASS**: WAL-safe backup took 0.10 seconds, staged
restore took 0.03 seconds, source/restored run exports and routing records were
identical, and counts matched. Rollback rehearsal is **PASS WITH LIMITATION**:
the `v1.0.1`-tagged source binary read the schema-12 restored run with a
version-compatible config, but its older strict TOML parser rejected the v1.1
configuration. A rollback must restore the versioned configuration as well as
the previous binary.

Final verdict: **NOT READY FOR SUPERVISED PRODUCTION**. Remote CI is also
pending because no Git remote is configured. Autonomous production remains
not ready.
