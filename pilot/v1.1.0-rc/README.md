# Forge v1.1.0 release-candidate pilot

This directory contains the frozen, manual-agent supervised-pilot definition.
`manifest.yaml` is the pre-registration authority. The three profiles require
the immutable OCI image and never fall back to host execution.

The profile split is intentional: Forge injects exactly one provider's named
credential into a job. fd and Zod are assigned to Claude; HTTPX is assigned to
Codex. Both CLI versions are preflighted inside the image by `forge doctor`.

Network policy is explicitly `allowed` because provider API access and locked
dependency installation require egress. Forge selects Docker bridge networking;
destination restrictions remain an operator responsibility for this pilot.

Do not run a task until its repository's doctor result is green. Do not copy
host Claude/Codex configuration into the container. Do not merge candidates,
consume the prospective routing holdout, tag, push, or publish from this plan.

Retention and recovery targets are proposals requiring human approval: keep
mandatory evidence for 365 days, raw provider streams for 30 days, diagnostic
failed workspaces for 7 days, and clear disposable build/cache/worktree state
after verified evidence capture. Proposed RPO is one completed run or 24 hours,
whichever is smaller; proposed RTO is 15 minutes. Both are conservative relative
to the measured local recovery drill and intended single-operator deployment.
