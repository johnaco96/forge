#!/usr/bin/env python3
"""Offline and local-image validator for the frozen RC4 qualification stratum."""

from __future__ import annotations

import hashlib
import json
import pathlib
import sqlite3
import subprocess
import tarfile


root = pathlib.Path(__file__).resolve().parents[2]
pilot = root / "pilot" / "v1.1.0-rc4"
manifest_path = pilot / "manifest.yaml"
manifest = manifest_path.read_text()


def sha256(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def product_paths() -> list[pathlib.Path]:
    paths = [root / "Cargo.toml", root / "Cargo.lock", root / "rust-toolchain.toml"]
    paths.extend(path for path in (root / "crates").rglob("*") if path.is_file())
    return sorted(set(paths), key=lambda path: path.relative_to(root).as_posix())


def product_source_identity(paths: list[pathlib.Path]) -> str:
    digest = hashlib.sha256()
    for path in paths:
        relative = path.relative_to(root).as_posix().encode()
        contents = path.read_bytes()
        digest.update(len(relative).to_bytes(8, "big"))
        digest.update(relative)
        digest.update(len(contents).to_bytes(8, "big"))
        digest.update(contents)
    return digest.hexdigest()


def complete_product_diff() -> bytes:
    scope = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "crates"]
    parts = [
        subprocess.run(
            ["git", "diff", "--binary", "--", *scope],
            cwd=root,
            check=True,
            stdout=subprocess.PIPE,
        ).stdout
    ]
    untracked = subprocess.run(
        ["git", "ls-files", "--others", "--exclude-standard", "--", *scope],
        cwd=root,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout.splitlines()
    assert untracked == ["crates/forge-cli/src/commands/agent_probe.rs"]
    for relative in sorted(untracked):
        result = subprocess.run(
            ["git", "diff", "--binary", "--no-index", "--", "/dev/null", relative],
            cwd=root,
            stdout=subprocess.PIPE,
        )
        assert result.returncode == 1
        parts.append(result.stdout)
    return b"".join(parts)


assert "status: frozen-before-model-outcomes" in manifest
assert "qualification_stratum: production-qualification-stratum-4" in manifest
assert "mode: recommendation-shadow-only" in manifest
assert "minimum_score_margin: 0.05" in manifest
assert "automatic_routing: false" in manifest
assert "automatic_merge: false" in manifest
assert "automatic_policy_promotion: false" in manifest
assert "automatic_team_dispatch: false" in manifest
assert "attempts_required: 9" in manifest
assert "independently_resolved_forge_outcomes_required: 9" in manifest
assert "successful_claude_live_probe: false" in manifest
assert "successful_codex_live_probe: false" in manifest
assert "rc4_pilot_attempts: 0" in manifest
assert "/Users/" not in manifest
assert manifest.count("  - { id: F-PILOT-") == 9
assert manifest.count("  - { task: F-PILOT-") == 9

paths = product_paths()
assert len(paths) == 139
assert product_source_identity(paths) == (
    "8742c698919b3810d26d0795620e11f35eaaf55924d91d008adc7fa9966de839"
)
candidate_diff = complete_product_diff()
assert len(candidate_diff) == 239266
assert hashlib.sha256(candidate_diff).hexdigest() == (
    "543a2d1b2d68a462b11bc3aba0d356826877089f417b2369ef80d006ba89c910"
)

expected = {
    "pilot/v1.1.0-rc/manifest.yaml": "5861383dae16e01aea5ae817a8fc2494f753121e76abc59f5edcf5fa476a5ce3",
    "pilot/v1.1.0-rc2/manifest.yaml": "23cf4fffb58d4a20f6579fc9c6026f1f8cb07b6122687e15193aa6be90adc00f",
    "pilot/v1.1.0-rc3/manifest.yaml": "70c7e7f17cf0809e2fc24d80428d58b653f87e4aed5b0426571ee869fe51379e",
    ".forge/validation-archive/tier1-master.jsonl": "b283ef15c92f3c4c54f104900234638c2c46b2919a2f13a14f7435f3b27903b9",
    "pilot/v1.1.0-rc4/amendment.md": "c8743709faf765991b8a20228ea41042e4892ae9ca58555bf8bb9e1c0114bac1",
    "pilot/v1.1.0-rc4/runtime/Dockerfile": "07e855ccaa20611a2302e1557d65dbbede01ffb6db359eec2322de7f35ce7f7b",
    "pilot/v1.1.0-rc4/runtime/claude-forge": "91eff694f5eda177ddb0cddc2e8f34caf5c6dac0f22387f9f3aaad30507fbb5f",
    "pilot/v1.1.0-rc4/runtime/codex-forge": "14b9bdddb5cf075bc330f8367b97d265698c40e094c434d0e5ce433f08d57c61",
    "pilot/v1.1.0-rc4/profiles/fd-claude.toml": "a9b906e75dfb445544ad65fb924e39f1733e5dacc6e95c9c297103cb4315f063",
    "pilot/v1.1.0-rc4/profiles/httpx-codex.toml": "8474e34aaaf7cd805a71a077a727a0bf27a2fa4e82561fb1407668b6295733fa",
    "pilot/v1.1.0-rc4/profiles/zod-claude.toml": "042262eb4dede83865d9e8dadc005c6ab74af9992b4fde56f31da649de4d44e6",
    ".github/workflows/ci.yml": "2278adb596ee17c7a4e6ed7edc337f3857f9589835aa7296b4f75a4eb2fa6a9d",
    ".github/workflows/security.yml": "aedd9603870101fc0447529b87bcadbccb4f2d7f215c72e5248f67ef10b8dec7",
    ".github/workflows/release.yml": "993781a76e4c673c4426665d31fb7fd1dc1dea179028c4536a2c98600d03ed70",
    "scripts/validate-pilot.py": "e9b8b1b9eb9114f5394b39e2a75eec2af1830013a76d73f788550617496c4844",
    "validation/external-pilot/plan.json": "6d94f2f4574407b0f22a258cb3305df3a14c22580d936daea0e5b2407a7a588e",
    "dist/v1.1.0-rc4/forge-1.1.0-darwin-arm64.tar.gz": "13f887a5886456642ddee6e5a73389af19df843f1a783a0457cd5db585aae097",
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
for relative, expected_digest in expected.items():
    actual = sha256(root / relative)
    assert actual == expected_digest, f"frozen input drift: {relative}: {actual}"
    assert expected_digest in manifest

external_root = root.parent / "forge-pilot-v1.1.0-rc4"
baselines = {
    "fd": "ee20f426ddf338ac7ead5c5f00ea49258005caaf",
    "httpx": "b5addb64f0161ff6bfe94c124ef76f6a1fba5254",
    "zod": "9f0a3d81221e3ab7c09ca4911ef35b54817869a4",
}
profile_paths = {
    "fd": pilot / "profiles/fd-claude.toml",
    "httpx": pilot / "profiles/httpx-codex.toml",
    "zod": pilot / "profiles/zod-claude.toml",
}
task_ids = {
    "fd": ["F-PILOT-FD-001", "F-PILOT-FD-002", "F-PILOT-FD-003"],
    "httpx": [
        "F-PILOT-HTTPX-001",
        "F-PILOT-HTTPX-002",
        "F-PILOT-HTTPX-003",
    ],
    "zod": ["F-PILOT-ZOD-001", "F-PILOT-ZOD-002", "F-PILOT-ZOD-003"],
}
config_fingerprints = {
    "fd": ("448216ee9c77f693", "ef48815b9b611461"),
    "httpx": ("aed8a79533aa0b26", "10f30c1b525492ef"),
    "zod": ("9d274a31f608dc3c", "403a90c3a6739c4a"),
}
for repository, baseline in baselines.items():
    repo = external_root / repository
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=repo,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout.strip()
    assert head == baseline
    subprocess.run(["git", "diff", "--quiet"], cwd=repo, check=True)
    subprocess.run(["git", "diff", "--cached", "--quiet"], cwd=repo, check=True)
    assert sha256(repo / ".forge/config.toml") == sha256(profile_paths[repository])
    linked_tasks = sorted((repo / ".forge/tasks").glob("*.yaml"))
    assert [task.stem for task in linked_tasks] == task_ids[repository]
    for linked_task in linked_tasks:
        frozen = root / "pilot/v1.1.0-rc3/tasks" / repository / linked_task.name
        assert sha256(linked_task) == sha256(frozen)

    database_uri = f"file:{repo / '.forge/forge.db'}?mode=ro"
    with sqlite3.connect(database_uri, uri=True) as connection:
        runs = connection.execute("SELECT COUNT(*) FROM runs").fetchone()[0]
        decisions = connection.execute(
            "SELECT decision_id, task_id, decision_kind, run_id, "
            "evidence_fingerprint, record_json FROM routing_decisions "
            "ORDER BY decision_id"
        ).fetchall()
    assert runs == 0
    assert len(decisions) == 3
    assert [row[1] for row in decisions] == task_ids[repository]
    for decision_id, task_id, kind, run_id, evidence, record in decisions:
        assert kind == "insufficient_evidence"
        assert run_id is None
        assert decision_id in manifest
        assert task_id in manifest
        assert evidence in manifest
        payload = json.loads(record)
        snapshot = payload["decision"]["snapshot"]
        assert snapshot["candidate_config_fingerprints"] == {
            "claude": config_fingerprints[repository][0],
            "codex": config_fingerprints[repository][1],
        }
        assert payload["decision"]["explanation"]["reasons"][-1] == {
            "kind": "score_margin",
            "actual": 0.0,
            "required": 0.05,
        }
    assert not any((repo / ".forge/worktrees").iterdir())
    assert not any((repo / ".forge/runs").iterdir())

image = "localhost:5000/forge/pilot-runtime@sha256:5624e2d6abe5fb52282963dbd41e1c9e7c1f3a18653bef2726b4c17e42fecde2"
inspection = subprocess.run(
    ["docker", "image", "inspect", image],
    check=True,
    text=True,
    stdout=subprocess.PIPE,
)
image_record = json.loads(inspection.stdout)[0]
assert image_record["Id"] == "sha256:44deba43a2ed6c8f3d40d551164c7ccc128ab9b04b5f3fdeea525f86fee8b1c3"
assert image_record["Os"] == "linux"
assert image_record["Architecture"] == "arm64"
assert image_record["Size"] == 2303808514
assert image_record["Config"]["Labels"]["org.opencontainers.image.qualification.source-tree"] == (
    "8742c698919b3810d26d0795620e11f35eaaf55924d91d008adc7fa9966de839"
)

archive = root / "dist/v1.1.0-rc4/forge-1.1.0-darwin-arm64.tar.gz"
with tarfile.open(archive, "r:gz") as package:
    names = package.getnames()
    assert names == [
        "forge-1.1.0-darwin-arm64",
        "forge-1.1.0-darwin-arm64/RELEASE-METADATA.json",
        "forge-1.1.0-darwin-arm64/forge",
    ]
    metadata_file = package.extractfile(
        "forge-1.1.0-darwin-arm64/RELEASE-METADATA.json"
    )
    assert metadata_file is not None
    metadata = json.load(metadata_file)
assert metadata == {
    "architecture": "arm64",
    "artifact_schema_version": 1,
    "commit": "be770b858e3aeff90618494b4fc0d133333527ef",
    "latest_migration": 12,
    "platform": "darwin",
    "sandbox_runtime": "Docker-compatible OCI",
    "version": "1.1.0",
}

print(
    "RC4 manifest valid: 139-file product source identity, complete product "
    "diff identity, 3 fresh baselines, 9 unchanged tasks, 9 pre-outcome "
    "routing abstentions, exact linux/arm64 image, and Darwin ARM64 package; "
    "manifest sha256=" + sha256(manifest_path)
)
