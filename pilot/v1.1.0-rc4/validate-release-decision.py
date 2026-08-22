#!/usr/bin/env python3
"""Validate the post-pilot RC4 human release decision without rewriting RC4."""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import sqlite3
import subprocess


ROOT = pathlib.Path(__file__).resolve().parents[2]
PILOT = ROOT / "pilot/v1.1.0-rc4"
IMAGE = (
    "localhost:5000/forge/pilot-runtime@"
    "sha256:5624e2d6abe5fb52282963dbd41e1c9e7c1f3a18653bef2726b4c17e42fecde2"
)
SOURCE_IDENTITY = "8742c698919b3810d26d0795620e11f35eaaf55924d91d008adc7fa9966de839"

REPOSITORIES = {
    "fd": {
        "baseline": "ee20f426ddf338ac7ead5c5f00ea49258005caaf",
        "profile": "fd-claude.toml",
        "agent": "claude",
        "harness": "2.1.223",
        "runs": [
            (
                "R-0001",
                "F-PILOT-FD-001",
                "TR-2d1e63c8825b91e08b2f8479c3d46bc0c4b8baa283cf28c8e8b2b70ae91d342f",
                "46691b3741b7c9da891331dbc39770c54d054a74",
                2,
            ),
            (
                "R-0002",
                "F-PILOT-FD-002",
                "TR-3cc8991fed709191dfe463f3920798a0222688d5db87505869d7f4510ddd9fea",
                "71f030a8a02f75311034919763d029a30fd67c8f",
                3,
            ),
            (
                "R-0003",
                "F-PILOT-FD-003",
                "TR-eaf613b687fe2f904b71edb800d449159f3facdde69e98b06d3dc3655a86047e",
                "4a7666620f0cd5732b82ffcb6f4f29cedf1e0478",
                2,
            ),
        ],
    },
    "httpx": {
        "baseline": "b5addb64f0161ff6bfe94c124ef76f6a1fba5254",
        "profile": "httpx-codex.toml",
        "agent": "codex",
        "harness": "0.147.0",
        "runs": [
            (
                "R-0001",
                "F-PILOT-HTTPX-001",
                "TR-230d21c55edf89278f0d34e860d778a3020e04ddef308f4a7f9ff6d83a2580c7",
                "5802b2555cdc4a7d17ffa50b837d08bc58798437",
                2,
            ),
            (
                "R-0002",
                "F-PILOT-HTTPX-002",
                "TR-12e3b85f36b8fa0ba8d2422498f50ffafe9c796dec75d474dbdb710ba9b6dc85",
                "b84f6115cac8bcf7326811204c6269fbed8d58f7",
                2,
            ),
            (
                "R-0003",
                "F-PILOT-HTTPX-003",
                "TR-1a26be110d6c014c675cc16728701776b9809eaec8daf6d2687325658ac68e35",
                "0c7aa3c43e3d3fd4d9e12707532bb57a7d5172b0",
                2,
            ),
        ],
    },
    "zod": {
        "baseline": "9f0a3d81221e3ab7c09ca4911ef35b54817869a4",
        "profile": "zod-claude.toml",
        "agent": "claude",
        "harness": "2.1.223",
        "runs": [
            (
                "R-0001",
                "F-PILOT-ZOD-001",
                "TR-b55f0198dc2f593492bf118163223d6e6ba37b0694ebc44d86358d113bc2800f",
                "4e2bf713f50bb18c1ffb6dbc5f9b9242f07ee645",
                2,
            ),
        ],
    },
}

ALL_TASKS = {
    "fd": ["F-PILOT-FD-001", "F-PILOT-FD-002", "F-PILOT-FD-003"],
    "httpx": [
        "F-PILOT-HTTPX-001",
        "F-PILOT-HTTPX-002",
        "F-PILOT-HTTPX-003",
    ],
    "zod": ["F-PILOT-ZOD-001", "F-PILOT-ZOD-002", "F-PILOT-ZOD-003"],
}


def sha256(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def git(repo: pathlib.Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout.strip()


def product_source_identity() -> str:
    paths = [ROOT / "Cargo.toml", ROOT / "Cargo.lock", ROOT / "rust-toolchain.toml"]
    paths.extend(path for path in (ROOT / "crates").rglob("*") if path.is_file())
    digest = hashlib.sha256()
    for path in sorted(set(paths), key=lambda item: item.relative_to(ROOT).as_posix()):
        relative = path.relative_to(ROOT).as_posix().encode()
        contents = path.read_bytes()
        digest.update(len(relative).to_bytes(8, "big"))
        digest.update(relative)
        digest.update(len(contents).to_bytes(8, "big"))
        digest.update(contents)
    return digest.hexdigest()


def readonly_database(path: pathlib.Path) -> sqlite3.Connection:
    return sqlite3.connect(f"{path.resolve().as_uri()}?mode=ro", uri=True)


def validate_frozen_inputs() -> None:
    assert sha256(PILOT / "manifest.yaml") == (
        "314564830961aefad910ec8288875852f52f9bca578e0ac9fb4be1bc3a53776b"
    )
    assert sha256(PILOT / "amendment.md") == (
        "c8743709faf765991b8a20228ea41042e4892ae9ca58555bf8bb9e1c0114bac1"
    )
    assert sha256(PILOT / "validate.py") == (
        "7c74f8a56320acc1b490ab16f9da521eac7a7e28ed906972cc5807fbd98da87b"
    )
    assert product_source_identity() == SOURCE_IDENTITY
    assert sha256(ROOT / "dist/v1.1.0-rc4/forge-1.1.0-darwin-arm64.tar.gz") == (
        "13f887a5886456642ddee6e5a73389af19df843f1a783a0457cd5db585aae097"
    )

    inspection = subprocess.run(
        ["docker", "image", "inspect", IMAGE],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    )
    image = json.loads(inspection.stdout)[0]
    assert image["Id"] == (
        "sha256:44deba43a2ed6c8f3d40d551164c7ccc128ab9b04b5f3fdeea525f86fee8b1c3"
    )
    assert (image["Os"], image["Architecture"]) == ("linux", "arm64")
    assert image["Config"]["Labels"][
        "org.opencontainers.image.qualification.source-tree"
    ] == SOURCE_IDENTITY


def validate_decision_language() -> None:
    decision = (PILOT / "release-decision.md").read_text()
    required = [
        "FROZEN RC4 GATE: not fully satisfied due to 2 human-waived tasks",
        "HUMAN RELEASE DECISION: residual qualification risk accepted",
        "AUTONOMOUS PRODUCTION: NOT AUTHORIZED",
        "ZOD-002",
        "ZOD-003",
        "NOT ATTEMPTED — HUMAN WAIVER",
        "Provider API budget constraint; release owner accepts the",
        "7/9 executed; 7/7 executed tasks PASS; 2/9 human-waived",
        "0 observed integrity failures",
        "0 observed production-class infrastructure failures",
        "GitHub CI/CD and publication",
    ]
    for phrase in required:
        assert phrase in decision, f"release decision omits: {phrase}"
    assert decision.count("NOT ATTEMPTED — HUMAN WAIVER") == 2


def validate_repository(evidence_root: pathlib.Path, name: str, expected: dict) -> tuple[int, int]:
    repo = evidence_root / name
    assert repo.is_dir(), f"missing prepared evidence repository: {repo}"
    assert git(repo, "rev-parse", "HEAD") == expected["baseline"]
    subprocess.run(["git", "-C", str(repo), "diff", "--quiet"], check=True)
    subprocess.run(["git", "-C", str(repo), "diff", "--cached", "--quiet"], check=True)
    assert sha256(repo / ".forge/config.toml") == sha256(
        PILOT / "profiles" / expected["profile"]
    )

    for task_id in ALL_TASKS[name]:
        prepared = repo / ".forge/tasks" / f"{task_id}.yaml"
        frozen = ROOT / "pilot/v1.1.0-rc3/tasks" / name / f"{task_id}.yaml"
        assert sha256(prepared) == sha256(frozen)

    expected_runs = expected["runs"]
    expected_by_id = {row[0]: row for row in expected_runs}
    with readonly_database(repo / ".forge/forge.db") as database:
        task_rows = database.execute("SELECT task_id FROM tasks ORDER BY task_id").fetchall()
        assert [row[0] for row in task_rows] == sorted(ALL_TASKS[name])
        runs = database.execute(
            "SELECT run_id, task_id, agent_id, status, agent_status, outcome, "
            "execution_provenance, base_commit, task_revision_id, branch, "
            "failure_reason, workspace_path, record_json FROM runs ORDER BY run_id"
        ).fetchall()
        assert len(runs) == len(expected_runs)
        assert [row[0] for row in runs] == [row[0] for row in expected_runs]

        evaluator_total = 0
        for row in runs:
            (
                run_id,
                task_id,
                agent_id,
                status,
                agent_status,
                outcome,
                provenance,
                base_commit,
                revision,
                branch,
                failure_reason,
                workspace_path,
                record_json,
            ) = row
            expected_run, expected_task, expected_revision, expected_head, expected_checks = (
                expected_by_id[run_id]
            )
            assert run_id == expected_run
            assert task_id == expected_task
            assert revision == expected_revision
            assert agent_id == expected["agent"]
            assert (status, agent_status, outcome, provenance) == (
                "completed",
                "completed",
                "passed",
                "live",
            )
            assert base_commit == expected["baseline"]
            assert failure_reason is None

            record = json.loads(record_json)
            assert record["integrity"]["status"] == "clean"
            assert record["evaluation_verdict"] == "pass"
            assert record.get("infrastructure_failures", []) == []
            assert record.get("warnings", []) == []
            assert record["agent"]["harness_version"] == expected["harness"]
            assert record["agent"]["sandbox"]["image"] == IMAGE

            checks = database.execute(
                "SELECT required, verdict, execution_status, exit_code, execution_error "
                "FROM evaluator_results WHERE run_id=? ORDER BY evaluator_id",
                (run_id,),
            ).fetchall()
            assert len(checks) == expected_checks
            assert all(check == (1, "pass", "completed", 0, None) for check in checks)
            evaluator_total += len(checks)

            patch = database.execute(
                "SELECT head_commit, diff_path FROM patches WHERE run_id=?", (run_id,)
            ).fetchone()
            assert patch is not None
            head_commit, diff_path = patch
            assert head_commit == expected_head
            assert pathlib.Path(diff_path).is_file()
            assert git(repo, "rev-parse", f"refs/heads/{branch}") == expected_head

            dispositions = database.execute(
                "SELECT data_json FROM events "
                "WHERE run_id=? AND event_type='WorkspaceDispositionRecorded'",
                (run_id,),
            ).fetchall()
            assert len(dispositions) == 1
            disposition = json.loads(dispositions[0][0])["data"]
            assert disposition["disposition"] == "removed"
            assert pathlib.Path(disposition["path"]) == pathlib.Path(workspace_path)
            assert not pathlib.Path(workspace_path).exists()

    return len(expected_runs), evaluator_total


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--evidence-root",
        type=pathlib.Path,
        default=ROOT.parent / "forge-pilot-v1.1.0-rc4",
        help="directory containing the prepared fd/httpx/zod evidence repositories",
    )
    args = parser.parse_args()

    validate_frozen_inputs()
    validate_decision_language()
    run_total = 0
    evaluator_total = 0
    for name, expected in REPOSITORIES.items():
        runs, evaluators = validate_repository(args.evidence_root, name, expected)
        run_total += runs
        evaluator_total += evaluators

    assert run_total == 7
    assert evaluator_total == 15
    print(
        "RC4 human release decision valid: frozen source/image/manifest preserved; "
        "7/9 executed, 7/7 PASS, 2/9 human-waived, 0 observed integrity failures, "
        "0 observed production-class infrastructure failures, 15/15 evaluator "
        "results PASS; frozen nine-outcome gate remains not fully satisfied"
    )


if __name__ == "__main__":
    main()
