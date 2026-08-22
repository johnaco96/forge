# Forge v1.1.0 RC3 qualification amendment

RC3 is a new qualification stratum. It does not continue RC2 and does not
reinterpret either historical incident.

RC2 stopped after `F-PILOT-FD-001` exposed two production-class defects:

1. the pinned contained evaluator image omitted `rustfmt` and Clippy, so a
   missing trusted evaluator prerequisite was reported as candidate failure;
2. normal runner completion removed a failed workspace even though
   `workspaces.keep_on_failure = true`.

RC3 adds explicit, version-aware evaluator prerequisites; fail-closed
preflight and typed evaluator-infrastructure classification; exact-image doctor
coverage; a single typed workspace-retention decision path; durable workspace
disposition evidence; and deterministic qualification drills for both defects.
The OCI image is rebuilt and pinned under a new digest.

The three repository SHAs, nine task intents, explicit agent assignments,
0.05 routing threshold, recommendation-only routing, human merge policy,
resource limits, and no-automation policy remain unchanged. Evaluator commands
were made self-bootstrapping and corrected against the frozen baselines before
this stratum was frozen. No RC1 or RC2 candidate work is imported. All nine
live tasks must restart from zero outcomes.

The Forge source remains uncommitted because the qualification instruction
explicitly forbids commits. The manifest therefore identifies the starting
commit plus a SHA-256 of the complete `crates` binary diff and hashes every
qualification input. Commit, tag, push, publication, automatic routing, merge,
promotion, and dispatch remain forbidden pending human review.
