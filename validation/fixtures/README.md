# Deterministic validation fixtures

`tier1-router-replay.jsonl` is the tracked, public-safe input for the exact
Tier 1 Rust router replay. It was derived from the immutable 40-record
operational export whose SHA-256 is
`b283ef15c92f3c4c54f104900234638c2c46b2919a2f13a14f7435f3b27903b9`.

The fixture retains task revisions, agent configurations, outcomes, integrity,
evaluation summaries and metrics, timestamps, provenance, and usage fields
that affect routing. It omits operational artifact paths, patch summaries,
run warnings, evaluator commands, evaluator output paths, and raw evaluator
failure details. Replaying the sanitized fixture is byte-identical to replaying
the operational export.

- Fixture SHA-256:
  `fd8c450c15832d8e6df099b30926ce58a2bae3457e1411bc8c985f40cfa18233`
- Summary replay SHA-256:
  `a1f48136b207e022f8d9dd2e655a9b6ff75ced8221d129a4aa918ca39a0cf5a0`

Do not replace this fixture with raw `.forge` state or provider logs.
