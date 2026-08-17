#!/usr/bin/env python3
"""Validate the pre-registration without executing tasks or routing."""

import json
import pathlib

root = pathlib.Path(__file__).resolve().parents[1]
campaign = json.loads(
    (root / "validation/routing-holdout/campaign.json").read_text()
)
assert campaign["status"] == "preregistered_not_executed"
assert campaign["router"]["version"] == "historical-baseline-v1"
assert campaign["router"]["minimum_score_margin"] == 0.05
assert campaign["execution"]["decision_before_ground_truth"] is True
assert campaign["execution"]["agents"] == ["claude", "codex"]
assert campaign["execution"]["automatic_execution"] is False
tasks = campaign["task_slots"]
assert 10 <= len(tasks) <= 20
assert len({task["slot"] for task in tasks}) == len(tasks)
assert all(task["status"] == "unselected" for task in tasks)
for forbidden in ("outcome", "winner", "selected_agent"):
    assert all(forbidden not in task for task in tasks)
print(f"routing holdout preregistration valid: {len(tasks)} unseen task slots")

