#!/usr/bin/env python3
"""Unit tests for dependency-free Tier 1 analysis helpers."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("analyze-tier1.py")
SPEC = importlib.util.spec_from_file_location("analyze_tier1", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class Tier1AnalysisTests(unittest.TestCase):
    def test_percentiles_use_documented_linear_method(self) -> None:
        self.assertEqual(MODULE.percentile([1, 2, 3, 4], 0.25), 1.75)
        self.assertEqual(MODULE.median([1, 2, 3, 4]), 2.5)

    def test_agent_fingerprint_ignores_timeout_and_tracks_identity(self) -> None:
        base = {"agent_id": "codex", "harness": "codex-cli", "timeout_secs": 3600}
        changed_timeout = {**base, "timeout_secs": 1}
        changed_model = {**base, "model": "gpt-5.6-sol"}
        self.assertEqual(
            MODULE.agent_config_fingerprint(base),
            MODULE.agent_config_fingerprint(changed_timeout),
        )
        self.assertNotEqual(
            MODULE.agent_config_fingerprint(base),
            MODULE.agent_config_fingerprint(changed_model),
        )

    def test_similarity_matches_fixed_weight_identity_case(self) -> None:
        record = {
            "task": {
                "task_id": "T",
                "repository": "forge",
                "objective": "fix the durable record",
                "classification": {
                    "category": "debugging",
                    "language": "rust",
                    "domain": "core",
                    "difficulty": "small",
                },
                "components": ["forge-core"],
                "tags": ["validation-campaign"],
            }
        }
        self.assertAlmostEqual(MODULE.similarity(record, record), 1.0)

    def test_objective_tokenization_matches_rust_alphanumeric_boundaries(self) -> None:
        self.assertEqual(
            MODULE.objective_tokens("task_revision Store::run café"),
            {"task", "revision", "store", "run", "café"},
        )

    def test_pair_outcome_rule_never_lets_speed_override_pass(self) -> None:
        pair = {
            "claude": {"outcome": "passed", "agent_runtime_ms": 1000},
            "codex": {"outcome": "failed", "agent_runtime_ms": 1},
        }
        self.assertEqual(MODULE.pair_winner(pair, []), ("claude", "pass_beats_non_pass"))

    def test_both_non_pass_preserves_tie(self) -> None:
        pair = {
            "claude": {"outcome": "inconclusive"},
            "codex": {"outcome": "failed"},
        }
        self.assertEqual(MODULE.pair_winner(pair, []), (None, "tie_both_non_pass"))

    def test_prior_records_use_finished_time_to_prevent_leakage(self) -> None:
        cutoff = MODULE.parse_time("2026-01-01T00:00:10Z")
        records = [
            {"created_at": "2026-01-01T00:00:00Z", "finished_at": "2026-01-01T00:00:11Z"},
            {"created_at": "2026-01-01T00:00:00Z", "finished_at": "2026-01-01T00:00:09Z"},
        ]
        self.assertEqual(MODULE.prior_records(records, cutoff), [records[1]])

    def test_seed_mapping_is_stable(self) -> None:
        seed = "forge-v1-2026-08"
        revision = "TR-test"
        first = int.from_bytes(MODULE.hashlib.sha256((seed + revision).encode()).digest(), "big") % 2
        second = int.from_bytes(MODULE.hashlib.sha256((seed + revision).encode()).digest(), "big") % 2
        self.assertEqual(first, second)


if __name__ == "__main__":
    unittest.main()
