#!/usr/bin/env python3
"""Offline validator for the frozen RC3 supervised-pilot stratum."""

from __future__ import annotations

import hashlib
import pathlib
import sqlite3
import subprocess


root = pathlib.Path(__file__).resolve().parents[2]
pilot = root / "pilot" / "v1.1.0-rc3"
manifest_path = pilot / "manifest.yaml"
manifest = manifest_path.read_text()


def sha256(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def prerequisite_sha256(path: pathlib.Path) -> str:
    canonical = "".join(
        line.strip() + "\n"
        for line in path.read_text().splitlines()
        if "executable:" in line
    )
    return hashlib.sha256(canonical.encode()).hexdigest()


assert "status: frozen-before-model-outcomes" in manifest
assert "qualification_stratum: production-qualification-stratum-3" in manifest
assert "mode: recommendation-shadow-only" in manifest
assert "minimum_score_margin: 0.05" in manifest
assert "automatic_routing: false" in manifest
assert "automatic_merge: false" in manifest
assert "automatic_policy_promotion: false" in manifest
assert "automatic_team_dispatch: false" in manifest
assert "commit_deferred_by_instruction: true" in manifest
assert "evaluator_substrate: all-nine-task-plans-qualified" in manifest
assert manifest.count("  - { id: F-PILOT-") == 9
assert manifest.count("  - { task: F-PILOT-") == 9

historical = {
    "pilot/v1.1.0-rc/manifest.yaml": (
        "5861383dae16e01aea5ae817a8fc2494f753121e76abc59f5edcf5fa476a5ce3"
    ),
    "pilot/v1.1.0-rc2/manifest.yaml": (
        "23cf4fffb58d4a20f6579fc9c6026f1f8cb07b6122687e15193aa6be90adc00f"
    ),
    ".forge/validation-archive/tier1-master.jsonl": (
        "b283ef15c92f3c4c54f104900234638c2c46b2919a2f13a14f7435f3b27903b9"
    ),
}
for relative, digest in historical.items():
    assert sha256(root / relative) == digest, f"historical evidence drift: {relative}"
    assert digest in manifest

rc1_ledger = pathlib.Path(
    "/Users/drewcook/Documents/Projects/forge-pilot-v1.1.0-rc/fd/.forge/forge.db"
)
assert sha256(rc1_ledger) == (
    "695136daa1c6541268c46aff5f17e41db44cdd9701b5ae1224741515a61a80c4"
)
assert pathlib.Path(
    "/Users/drewcook/Documents/Projects/forge-pilot-v1.1.0-rc/fd/.forge/worktrees/R-0001"
).is_dir()

rc2_run = pathlib.Path(
    "/Users/drewcook/Documents/Projects/forge-pilot-v1.1.0-rc2/fd/.forge/runs/R-0001"
)
rc2_artifacts = {
    "agent.stdout.log": "fdffb95059a828574cddb2aaaa5230d72b49692792b8af8de67b2cc9d28fc947",
    "patch.diff": "7cf5bec3705a8aa85de3b7a6f1bd275de69db93153a06a18f81e48c503c80b26",
    "checks/tests.log": "74b359507746269ea98914fe0669e28a87bb8c17f0367aeb652a294726e63808",
    "checks/lint.log": "c7a3c8d7a8f5caa5684b249f1cdf7fa4b797d364e7c30e905d80ecf93c14a636",
    "prompt.txt": "b1ac5e92d5c85347136e65352c11e14f51b641b31be559948a0e8f310954f81b",
}
for relative, digest in rc2_artifacts.items():
    assert sha256(rc2_run / relative) == digest, f"RC2 artifact drift: {relative}"
    assert digest in manifest
assert not pathlib.Path(
    "/Users/drewcook/Documents/Projects/forge-pilot-v1.1.0-rc2/fd/.forge/worktrees/R-0001"
).exists()

source_patch = subprocess.run(
    ["git", "diff", "--binary", "--", "crates"],
    cwd=root,
    check=True,
    stdout=subprocess.PIPE,
).stdout
assert hashlib.sha256(source_patch).hexdigest() == (
    "3598e4cc17bbc888a2424f126eb47cbe87c7517a62b3a1241bba5c98663e23af"
)

expected = {
    ".github/workflows/ci.yml": "2278adb596ee17c7a4e6ed7edc337f3857f9589835aa7296b4f75a4eb2fa6a9d",
    "scripts/validate-pilot.py": "e9b8b1b9eb9114f5394b39e2a75eec2af1830013a76d73f788550617496c4844",
    "validation/external-pilot/plan.json": "6d94f2f4574407b0f22a258cb3305df3a14c22580d936daea0e5b2407a7a588e",
    "pilot/v1.1.0-rc3/amendment.md": "715d33dbaec3fe64f742fc66a1ccd7619a4e78c490f63bd179c67730c0ff150a",
    "pilot/v1.1.0-rc3/substrate-qualification.md": "0b930f66fca025c86147012ee3aa1ebe8d37b452d41a04ea69b1d730381fee10",
    "pilot/v1.1.0-rc3/profiles/fd-claude.toml": "2234cb592c66e4639f8752c5625769906bb65f6c9a193f9f5559851a1b055ce5",
    "pilot/v1.1.0-rc3/profiles/httpx-codex.toml": "41ef01aae12be832204fce0cd89831c139d0e50c4e369771ffa65df3de3edb0f",
    "pilot/v1.1.0-rc3/profiles/zod-claude.toml": "ad88848ba8ab4d08d205747ece88cc9f0684fd6e1a365369b2d5c2ca208d43cd",
    "pilot/v1.1.0-rc3/runtime/Dockerfile": "f134fe10b6808d8eb8f65312622dcfdbfd374f2fa7e164326ab506f94e3ddc13",
    "pilot/v1.1.0-rc3/runtime/codex-forge": "09994274eadb0e0932453539401805bdd9791eaecdadf7c5177eb8b919c71c60",
    "pilot/v1.1.0-rc3/tasks/fd/F-PILOT-FD-001.yaml": "d816623dcdf7cdb9d1c584c0d56deb953224327934daf5a5b0ba3061ac85be08",
    "pilot/v1.1.0-rc3/tasks/fd/F-PILOT-FD-002.yaml": "ae22af1aa5b991f230e661c8eb7b2e4f80ab0ad262ab5b3f023098508edd5573",
    "pilot/v1.1.0-rc3/tasks/fd/F-PILOT-FD-003.yaml": "9745b5dc4745ebab6ca0a6b6f10993380778b57c3a5063fa4ee7984cc58d9d33",
    "pilot/v1.1.0-rc3/tasks/httpx/F-PILOT-HTTPX-001.yaml": "667042199647dd5f53527f9b8be29a4ae767b7a9a209a841522aa831b4b924f9",
    "pilot/v1.1.0-rc3/tasks/httpx/F-PILOT-HTTPX-002.yaml": "86e8b9aa40ebb36d1028846c5750613bcc3fdf47116c74a59feb015a6c2ab836",
    "pilot/v1.1.0-rc3/tasks/httpx/F-PILOT-HTTPX-003.yaml": "e97b1e1ecbac6fcb3d3a04aab5af90f04ae7af67f40f9b4a8ab830011a22a571",
    "pilot/v1.1.0-rc3/tasks/zod/F-PILOT-ZOD-001.yaml": "3f0dfb9128994b401cfd45a6e2b8967a40fc0c1b4ebd68d63a50ad8d74c0b3f7",
    "pilot/v1.1.0-rc3/tasks/zod/F-PILOT-ZOD-002.yaml": "485877dc6f48a3a9bf4ba96edab722539130679e42efee87559994acd61250c2",
    "pilot/v1.1.0-rc3/tasks/zod/F-PILOT-ZOD-003.yaml": "9bc2c20c420b7a8fe0ddb42e01c25b09b3b685abcfc9b88adb6201915cbcaacc",
}
for relative, digest in expected.items():
    actual = sha256(root / relative)
    assert actual == digest, f"frozen input drift: {relative}: {actual} != {digest}"
    assert digest in manifest

prerequisites = {
    "F-PILOT-FD-001": "3cf6c8b94519564c798a3b5287acc28a6652e0e7bdbfb12269ea4101e71b1d47",
    "F-PILOT-FD-002": "5dc05971d641f2be98c479408fc902f9008df2ea0794e21b2b7e6b7d465d88a1",
    "F-PILOT-FD-003": "3cf6c8b94519564c798a3b5287acc28a6652e0e7bdbfb12269ea4101e71b1d47",
    "F-PILOT-HTTPX-001": "84ec3538cda7990b16fafc50418970603f00c5e9d1f031f872eb63cfadbc9d3e",
    "F-PILOT-HTTPX-002": "84ec3538cda7990b16fafc50418970603f00c5e9d1f031f872eb63cfadbc9d3e",
    "F-PILOT-HTTPX-003": "84ec3538cda7990b16fafc50418970603f00c5e9d1f031f872eb63cfadbc9d3e",
    "F-PILOT-ZOD-001": "0cd0f6a2bdd318b36f1d2be4dae4103fb8b016a4f67d63feb0ba977e8354f716",
    "F-PILOT-ZOD-002": "0cd0f6a2bdd318b36f1d2be4dae4103fb8b016a4f67d63feb0ba977e8354f716",
    "F-PILOT-ZOD-003": "0cd0f6a2bdd318b36f1d2be4dae4103fb8b016a4f67d63feb0ba977e8354f716",
}
for task_path in sorted((pilot / "tasks").glob("*/*.yaml")):
    task_id = task_path.stem
    digest = prerequisite_sha256(task_path)
    assert digest == prerequisites[task_id], f"prerequisite drift: {task_id}"
    assert digest in manifest

baselines = {
    "fd": "ee20f426ddf338ac7ead5c5f00ea49258005caaf",
    "httpx": "b5addb64f0161ff6bfe94c124ef76f6a1fba5254",
    "zod": "9f0a3d81221e3ab7c09ca4911ef35b54817869a4",
}
profiles = {
    "fd": pilot / "profiles/fd-claude.toml",
    "httpx": pilot / "profiles/httpx-codex.toml",
    "zod": pilot / "profiles/zod-claude.toml",
}
task_ids = {
    "fd": ["F-PILOT-FD-001", "F-PILOT-FD-002", "F-PILOT-FD-003"],
    "httpx": ["F-PILOT-HTTPX-001", "F-PILOT-HTTPX-002", "F-PILOT-HTTPX-003"],
    "zod": ["F-PILOT-ZOD-001", "F-PILOT-ZOD-002", "F-PILOT-ZOD-003"],
}
config_fingerprints = {
    "fd": ("ff21843798397451", "8906d27fdb625e6c"),
    "httpx": ("4bf31f1a9d95a4ce", "453100d5b4d75e1e"),
    "zod": ("4341cccbdd7bdda9", "4e955a2623b55888"),
}
external_root = pathlib.Path(
    "/Users/drewcook/Documents/Projects/forge-pilot-v1.1.0-rc3"
)
for repository, baseline in baselines.items():
    repo = external_root / repository
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo, check=True, text=True,
        stdout=subprocess.PIPE,
    ).stdout.strip()
    assert head == baseline
    subprocess.run(["git", "diff", "--quiet"], cwd=repo, check=True)
    subprocess.run(["git", "diff", "--cached", "--quiet"], cwd=repo, check=True)
    assert sha256(repo / ".forge/config.toml") == sha256(profiles[repository])
    linked_tasks = sorted((repo / ".forge/tasks").glob("*.yaml"))
    assert [task.stem for task in linked_tasks] == task_ids[repository]
    for linked_task in linked_tasks:
        frozen = pilot / "tasks" / repository / linked_task.name
        assert sha256(linked_task) == sha256(frozen)

    database_uri = f"file:{repo / '.forge/forge.db'}?mode=ro"
    with sqlite3.connect(database_uri, uri=True) as connection:
        runs = connection.execute("SELECT COUNT(*) FROM runs").fetchone()[0]
        decisions = connection.execute(
            "SELECT decision_id, task_id, task_revision_id, decision_kind, "
            "run_id, evidence_fingerprint, record_json "
            "FROM routing_decisions ORDER BY decision_id"
        ).fetchall()
    assert runs == 0
    assert len(decisions) == 3
    assert [row[1] for row in decisions] == task_ids[repository]
    for decision_id, task_id, revision, kind, run_id, evidence, record in decisions:
        assert kind == "insufficient_evidence"
        assert run_id is None
        assert decision_id in manifest
        assert task_id in manifest
        assert revision in manifest
        assert evidence in manifest
        assert '"decision_margin":0.0' in record
        assert all(fingerprint in record for fingerprint in config_fingerprints[repository])
    assert not any((repo / ".forge/worktrees").iterdir())
    assert not any((repo / ".forge/runs").iterdir())

image = "localhost:5000/forge/pilot-runtime@sha256:ede1b16e60b242ddb5edd00a32327cb4ba535b08ba08cef16a3715e05f296104"
for profile in profiles.values():
    text = profile.read_text()
    assert image in text
    assert "minimum_score_margin = 0.05" in text
    assert "keep_on_failure = true" in text

print(
    "RC3 pilot manifest valid: 3 exact fresh baselines, 9 frozen tasks with "
    "explicit prerequisites, 9 pre-outcome shadow decisions, 0 model runs; "
    "manifest sha256=" + sha256(manifest_path)
)
