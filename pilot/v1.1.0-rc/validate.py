#!/usr/bin/env python3
"""Offline validator for the frozen v1.1.0-rc supervised-pilot definition."""

from __future__ import annotations

import hashlib
import pathlib


root = pathlib.Path(__file__).resolve().parents[2]
pilot = root / "pilot" / "v1.1.0-rc"
manifest = (pilot / "manifest.yaml").read_text()

assert "status: frozen-before-model-outcomes" in manifest
assert "mode: recommendation-shadow-only" in manifest
assert "minimum_score_margin: 0.05" in manifest
assert "automatic_routing: false" in manifest
assert "automatic_merge: false" in manifest
assert "automatic_policy_promotion: false" in manifest
assert "automatic_team_dispatch: false" in manifest
assert manifest.count("  - { id: F-PILOT-") == 9
assert len(list((pilot / "tasks").glob("*/*.yaml"))) == 9
assert len(list((pilot / "profiles").glob("*.toml"))) == 3

expected = {
    "pilot/v1.1.0-rc/profiles/fd-claude.toml": "873cdc36273c4f19d9903522b11f9254f91f4de51360aaf47380ad8ba96f5e9e",
    "pilot/v1.1.0-rc/profiles/httpx-codex.toml": "31a1078fa2d39489e418d0c217477f816ca449696bcf9817eb1552d76af3a80f",
    "pilot/v1.1.0-rc/profiles/zod-claude.toml": "b06d08026e7e6abfd2930721c5c7536ce2732ac087a9d230a5670d8fd50f1b73",
    "pilot/v1.1.0-rc/tasks/fd/F-PILOT-FD-001.yaml": "3453fcc112325e709b9f360733d966a089a9faccf21987d8965716c74d7018a9",
    "pilot/v1.1.0-rc/tasks/fd/F-PILOT-FD-002.yaml": "64c43a4d0219492577690d5e738aa1df87667100bf1a6e74e71a3344540ce27b",
    "pilot/v1.1.0-rc/tasks/fd/F-PILOT-FD-003.yaml": "51b82deb24ed0b2a37dccfd47300f63960b899c6a8f30c904252405b13e5acaf",
    "pilot/v1.1.0-rc/tasks/httpx/F-PILOT-HTTPX-001.yaml": "45e099577c020b90de3cb1d5aa4464df8408b74679fb9944ea220146a4f14f18",
    "pilot/v1.1.0-rc/tasks/httpx/F-PILOT-HTTPX-002.yaml": "a1ae3b4505515e861e9727972aba2b5af0bb3dc38e0f14c8dcc869714da39d8b",
    "pilot/v1.1.0-rc/tasks/httpx/F-PILOT-HTTPX-003.yaml": "18e18ef141412ef6f7764db5ab13f13b90407b912b593fb3bd1a05d5ea7c7a13",
    "pilot/v1.1.0-rc/tasks/zod/F-PILOT-ZOD-001.yaml": "4ba93aa74390041a3fc53e846b8405e186bf65b379fb41c7afba4a59464c28bb",
    "pilot/v1.1.0-rc/tasks/zod/F-PILOT-ZOD-002.yaml": "4c97d73d38b5421522d23102310fe50f68551b838e8c5f8de88da51e561c7b20",
    "pilot/v1.1.0-rc/tasks/zod/F-PILOT-ZOD-003.yaml": "97620ff13f28aaf9b8c94d90e62234a4a85159ca401693bf4a64b4d5e50be0aa",
    "pilot/v1.1.0-rc/runtime/Dockerfile": "940f5c6ba24c6f198bc78bc0fda739d5facca5beddb9073dc8fd1d46e8b20a66",
}

for relative, digest in expected.items():
    actual = hashlib.sha256((root / relative).read_bytes()).hexdigest()
    assert actual == digest, f"frozen input drift: {relative}: {actual} != {digest}"
    assert digest in manifest

print("v1.1.0-rc pilot manifest valid: 3 profiles, 3 repositories, 9 frozen tasks")
