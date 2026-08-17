# Routing holdout v1

This directory is a pre-registration, not a result set. The campaign must not
be executed until an operator selects 10–20 genuinely new tasks without seeing
their Claude/Codex outcomes, records each immutable baseline commit, and freezes
the effective agent configurations.

For every task, Forge first runs recommendation mode and persists either the
recommendation or abstention. Only then do Claude and Codex run independently
from the same commit. Abstention is ground truth too: both runs still happen,
and no later manual choice is relabeled as a router decision.

Task selection must reject Tier 1 tasks, synthetic fixtures, toy benchmarks,
non-deterministic evaluators, tasks already attempted by either candidate
configuration, and tasks whose baseline cannot be reproduced. Selection should
balance the preregistered category/language slots without choosing tasks because
one agent is expected to win.

Validate the frozen definition without running anything:

    python3 scripts/validate-routing-holdout.py

The threshold remains 0.05. Tier 1 is calibration evidence only and did not
authorize a production-default change. Any threshold proposal after this
holdout is a separate reviewed policy change.

