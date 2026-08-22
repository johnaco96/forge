#!/usr/bin/env python3
"""Offline validator for the frozen RC2 supervised-pilot stratum."""

from __future__ import annotations

import hashlib
import pathlib
import sqlite3
import subprocess


root = pathlib.Path(__file__).resolve().parents[2]
pilot = root / "pilot" / "v1.1.0-rc2"
manifest_path = pilot / "manifest.yaml"
manifest = manifest_path.read_text()


def sha256(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


assert "status: frozen-before-model-outcomes" in manifest
assert "qualification_stratum: production-qualification-stratum-2" in manifest
assert "mode: recommendation-shadow-only" in manifest
assert "minimum_score_margin: 0.05" in manifest
assert "automatic_routing: false" in manifest
assert "automatic_merge: false" in manifest
assert "automatic_policy_promotion: false" in manifest
assert "automatic_team_dispatch: false" in manifest
assert "contained_evaluator_without_provider_credentials: mandatory-pass" in manifest
assert manifest.count("  - { id: F-PILOT-") == 9
assert manifest.count("  - { task: F-PILOT-") == 9

assert sha256(root / "pilot/v1.1.0-rc/manifest.yaml") == (
    "5861383dae16e01aea5ae817a8fc2494f753121e76abc59f5edcf5fa476a5ce3"
)
assert sha256(root / ".forge/validation-archive/tier1-master.jsonl") == (
    "b283ef15c92f3c4c54f104900234638c2c46b2919a2f13a14f7435f3b27903b9"
)

source_patch = subprocess.run(
    ["git", "diff", "--binary", "--", "crates"],
    cwd=root,
    check=True,
    stdout=subprocess.PIPE,
).stdout
assert hashlib.sha256(source_patch).hexdigest() == (
    "0338a65d3489e5d7972cd0ce7abd6b6f9a91f53d9bfd3ce8652e853728f884f4"
)

expected = {
    ".github/workflows/ci.yml": "2278adb596ee17c7a4e6ed7edc337f3857f9589835aa7296b4f75a4eb2fa6a9d",
    "scripts/validate-pilot.py": "e1b05f8c12967d03fc4b8bec7b2dcfb0e1dd7e57d43c32739145fe20c7b609b0",
    "validation/external-pilot/plan.json": "95be71c73a7c7784c62ba3dada115593c633b3b46015e07d9591cca1b9f2fbf1",
    "pilot/v1.1.0-rc2/profiles/fd-claude.toml": "fc9210a38c859acfdaa5c508ab07e0af8bc3f4478e0888d293ca08c19a9e7290",
    "pilot/v1.1.0-rc2/profiles/httpx-codex.toml": "ba4923b4f2f60f3b5104d9cd7695ad7bf82a8410d8c04d27f457fd6756e5270e",
    "pilot/v1.1.0-rc2/profiles/zod-claude.toml": "f65121977a0a8a6be4848f6cf8c6c1d64a98f3ad5651df597c0a6845ecd5d79f",
    "pilot/v1.1.0-rc2/runtime/Dockerfile": "e3f7dba682f2fdf2c140ed330bf41a76295edde756f98f3be8740da8553d134a",
    "pilot/v1.1.0-rc2/runtime/codex-forge": "09994274eadb0e0932453539401805bdd9791eaecdadf7c5177eb8b919c71c60",
    "pilot/v1.1.0-rc/tasks/fd/F-PILOT-FD-001.yaml": "3453fcc112325e709b9f360733d966a089a9faccf21987d8965716c74d7018a9",
    "pilot/v1.1.0-rc/tasks/fd/F-PILOT-FD-002.yaml": "64c43a4d0219492577690d5e738aa1df87667100bf1a6e74e71a3344540ce27b",
    "pilot/v1.1.0-rc/tasks/fd/F-PILOT-FD-003.yaml": "51b82deb24ed0b2a37dccfd47300f63960b899c6a8f30c904252405b13e5acaf",
    "pilot/v1.1.0-rc/tasks/httpx/F-PILOT-HTTPX-001.yaml": "45e099577c020b90de3cb1d5aa4464df8408b74679fb9944ea220146a4f14f18",
    "pilot/v1.1.0-rc/tasks/httpx/F-PILOT-HTTPX-002.yaml": "a1ae3b4505515e861e9727972aba2b5af0bb3dc38e0f14c8dcc869714da39d8b",
    "pilot/v1.1.0-rc/tasks/httpx/F-PILOT-HTTPX-003.yaml": "18e18ef141412ef6f7764db5ab13f13b90407b912b593fb3bd1a05d5ea7c7a13",
    "pilot/v1.1.0-rc/tasks/zod/F-PILOT-ZOD-001.yaml": "4ba93aa74390041a3fc53e846b8405e186bf65b379fb41c7afba4a59464c28bb",
    "pilot/v1.1.0-rc/tasks/zod/F-PILOT-ZOD-002.yaml": "4c97d73d38b5421522d23102310fe50f68551b838e8c5f8de88da51e561c7b20",
    "pilot/v1.1.0-rc/tasks/zod/F-PILOT-ZOD-003.yaml": "97620ff13f28aaf9b8c94d90e62234a4a85159ca401693bf4a64b4d5e50be0aa",
}
for relative, digest in expected.items():
    actual = sha256(root / relative)
    assert actual == digest, f"frozen input drift: {relative}: {actual} != {digest}"
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
external_root = pathlib.Path(
    "/Users/drewcook/Documents/Projects/forge-pilot-v1.1.0-rc2"
)
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
    assert sha256(repo / ".forge/config.toml") == sha256(profiles[repository])
    assert len(list((repo / ".forge/tasks").glob("*.yaml"))) == 3
    with sqlite3.connect(repo / ".forge/forge.db") as connection:
        runs = connection.execute("SELECT COUNT(*) FROM runs").fetchone()[0]
        decisions = connection.execute(
            "SELECT COUNT(*) FROM routing_decisions"
        ).fetchone()[0]
        margins = connection.execute(
            "SELECT json_extract(record_json, '$.decision.decision_margin') "
            "FROM routing_decisions"
        ).fetchall()
    assert runs == 0
    assert decisions == 3
    assert margins == [(0.0,), (0.0,), (0.0,)]

print(
    "RC2 pilot manifest valid: 3 exact baselines, 9 unchanged tasks, "
    "9 pre-outcome router observations, 0 model runs; manifest sha256="
    + sha256(manifest_path)
)
