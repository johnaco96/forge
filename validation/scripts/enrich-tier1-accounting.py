#!/usr/bin/env python3
"""Run forge-accounting over all preserved Tier 1 Codex runs."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path


class EnrichmentError(RuntimeError):
    pass


def read_thread_id(agent_log: Path) -> str:
    with agent_log.open(encoding="utf-8") as handle:
        for line in handle:
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if event.get("type") == "thread.started" and event.get("thread_id"):
                return str(event["thread_id"])
    raise EnrichmentError(f"no Codex thread id in {agent_log}")


def locate_session(sessions: Path, thread_id: str) -> Path:
    matches = sorted(sessions.rglob(f"*{thread_id}.jsonl"))
    if len(matches) != 1:
        raise EnrichmentError(
            f"expected one session for Codex thread {thread_id}, found {len(matches)}"
        )
    return matches[0]


def codex_exports(archive: Path) -> list[Path]:
    exports = sorted(archive.glob("campaign-*/T-VAL-*.codex.export.jsonl"))
    if len(exports) != 20:
        raise EnrichmentError(f"expected 20 Codex exports, found {len(exports)}")
    return exports


def enrich(binary: Path, archive: Path, sessions: Path, output: Path) -> None:
    combined = []
    with tempfile.TemporaryDirectory(prefix="forge-tier1-accounting-") as temporary:
        temporary_path = Path(temporary)
        for index, export in enumerate(codex_exports(archive), 1):
            task = export.name.split(".codex.export.jsonl", 1)[0]
            campaign_dir = export.parent
            environment = campaign_dir / "environment.json"
            agent_log = (
                campaign_dir
                / "participants"
                / f"{task}-codex"
                / ".forge"
                / "runs"
                / "R-0001"
                / "agent.stdout.log"
            )
            thread_id = read_thread_id(agent_log)
            session = locate_session(sessions, thread_id)
            individual = temporary_path / f"{index:02d}-{task}.jsonl"
            subprocess.run(
                [
                    str(binary),
                    "enrich-codex",
                    "--environment",
                    str(environment),
                    "--export",
                    str(export),
                    "--agent-log",
                    str(agent_log),
                    "--session-log",
                    str(session),
                    "--output",
                    str(individual),
                ],
                check=True,
            )
            rows = [json.loads(line) for line in individual.read_text(encoding="utf-8").splitlines()]
            if len(rows) != 1:
                raise EnrichmentError(f"{individual} contains {len(rows)} records")
            combined.extend(rows)

    combined.sort(
        key=lambda record: (
            record["source_artifacts"][0]["path"],
            record["evidence_key"]["task_id"],
        )
    )
    keys = {
        (
            record["evidence_key"]["campaign_id"],
            record["evidence_key"]["task_revision_id"],
            record["evidence_key"]["base_commit"],
            record["evidence_key"]["agent_id"],
            record["evidence_key"]["run_id"],
        )
        for record in combined
    }
    if len(keys) != 20:
        raise EnrichmentError(f"expected 20 global accounting keys, found {len(keys)}")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        "".join(json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n" for record in combined),
        encoding="utf-8",
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--sessions", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        enrich(args.binary.resolve(), args.archive.resolve(), args.sessions.resolve(), args.output.resolve())
    except (EnrichmentError, OSError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"wrote {args.output} (20 records)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
