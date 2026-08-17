#!/usr/bin/env python3
"""Validate the supervised-pilot plan without cloning or executing it."""

import json
import pathlib

root = pathlib.Path(__file__).resolve().parents[1]
plan = json.loads((root / "validation/external-pilot/plan.json").read_text())
assert plan["status"] == "defined_not_executed"
assert plan["minimum_repositories"] >= 2
assert plan["minimum_ecosystems"] >= 2
repositories = plan["candidate_repositories"]
assert len(repositories) >= plan["minimum_repositories"]
assert len({item["ecosystem"] for item in repositories}) >= plan["minimum_ecosystems"]
assert all(item["required_commands"] for item in repositories)
policy = plan["execution_policy"]
assert policy == {
    "agent_selection": "manual",
    "router": "recommendation_only",
    "containment": "required",
    "merge": "human_approval",
    "policy_auto_promotion": False,
    "team_auto_dispatch": False,
    "automatic_merge": False,
}
assert len(plan["required_drills"]) == 13
assert "full_commit_sha" in plan["baseline_fields_required"]
assert "container_image_digest" in plan["baseline_fields_required"]
print(
    "external pilot plan valid: "
    f"{len(repositories)} candidates, "
    f"{len(plan['required_drills'])} required drills"
)

