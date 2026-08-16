#!/usr/bin/env python3
"""Deterministic post-campaign analysis for Forge Tier 1.

This tool reads the frozen schema-1 JSONL export and optional additive Codex
accounting records. It never opens a Forge ledger and never writes below the
private validation archive. All outputs are derived artifacts.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import os
import re
import statistics
import subprocess
import sys
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Iterable, Sequence


ANALYSIS_VERSION = "tier1-post-campaign-v1"
ROUTER_VERSION = "historical-baseline-v1"
RATE_CARD_ID = "openai-chatgpt-codex-credits-2026-08-15"
EXPECTED_AGENTS = ("claude", "codex")
EXPECTED_BASE = "781b32fab791d1d4f839bfb1e5988f4e56150048"
EXPECTED_TASK_COUNT = 20
EXPECTED_RECORD_COUNT = 40
CATEGORY_ORDER = (
    "debugging",
    "feature",
    "refactor",
    "testing",
    "performance",
    "persistence",
)
OUTCOME_LABELS = {
    "passed": "PASS",
    "failed": "FAIL",
    "inconclusive": "INCONCLUSIVE",
    "errored": "INFRASTRUCTURE_EXCLUDED",
    "no_change": "NO_CHANGE",
}


class AnalysisError(RuntimeError):
    """A fail-closed dataset or analysis-contract violation."""


def canonical_json(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    with path.open(encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, 1):
            if not line.strip():
                raise AnalysisError(f"blank JSONL record at {path}:{line_number}")
            try:
                value = json.loads(line)
            except json.JSONDecodeError as error:
                raise AnalysisError(f"malformed JSON at {path}:{line_number}: {error}") from error
            if not isinstance(value, dict):
                raise AnalysisError(f"non-object JSONL record at {path}:{line_number}")
            records.append(value)
    return records


def parse_time(value: str | None) -> datetime | None:
    if value is None:
        return None
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def median(values: Sequence[float | int]) -> float | None:
    return statistics.median(values) if values else None


def percentile(values: Sequence[float | int], fraction: float) -> float | None:
    """Linear percentile (R-7/NumPy default), documented in results metadata."""
    if not values:
        return None
    ordered = sorted(float(value) for value in values)
    if len(ordered) == 1:
        return ordered[0]
    position = (len(ordered) - 1) * fraction
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def describe(values: Sequence[float | int]) -> dict[str, float | int | None]:
    if not values:
        return {
            "n": 0,
            "min": None,
            "q1": None,
            "median": None,
            "mean": None,
            "q3": None,
            "max": None,
        }
    return {
        "n": len(values),
        "min": min(values),
        "q1": percentile(values, 0.25),
        "median": median(values),
        "mean": statistics.fmean(values),
        "q3": percentile(values, 0.75),
        "max": max(values),
    }


def fmt_number(value: float | int | None, digits: int = 2) -> str:
    if value is None:
        return "unknown"
    if isinstance(value, int):
        return f"{value:,}"
    if value.is_integer():
        return f"{int(value):,}"
    return f"{value:,.{digits}f}"


def fmt_rate(value: float | None) -> str:
    return "unknown" if value is None else f"{value * 100:.1f}%"


def campaign_scalar(text: str, name: str) -> str:
    match = re.search(rf"(?m)^{re.escape(name)}:\s*[\"']?([^\"'#\n]+)", text)
    if not match:
        raise AnalysisError(f"campaign manifest is missing `{name}`")
    return match.group(1).strip()


def git(repo: Path, *arguments: str) -> str:
    return subprocess.check_output(
        ["git", *arguments], cwd=repo, text=True, stderr=subprocess.DEVNULL
    ).strip()


def agent_config_fingerprint(config: dict[str, Any]) -> str:
    """Exact Python reproduction of forge_core::AgentConfig::fingerprint."""
    digest = hashlib.sha256()
    digest.update(config["agent_id"].encode())
    digest.update(b"\0")
    digest.update(config["harness"].encode())
    digest.update(b"\0")
    digest.update((config.get("model") or "").encode())
    digest.update(b"\0")
    for tool in config.get("tools", []):
        digest.update(tool.encode())
        digest.update(b"\x1f")
    digest.update(b"\0")
    for key, value in sorted(config.get("settings", {}).items()):
        digest.update(key.encode())
        digest.update(b"\x1e")
        digest.update(value.encode())
        digest.update(b"\x1f")
    return digest.digest()[:8].hex()


def outcome_label(record: dict[str, Any]) -> str:
    outcome = record.get("outcome")
    if outcome not in OUTCOME_LABELS:
        raise AnalysisError(f"unknown outcome `{outcome}`")
    return OUTCOME_LABELS[outcome]


def integrity_clean(record: dict[str, Any]) -> bool:
    return record.get("integrity", {}).get("status") == "clean"


def integrity_paths(record: dict[str, Any]) -> list[str]:
    integrity = record.get("integrity") or {}
    paths: list[str] = []
    for key in ("modified", "missing", "added"):
        paths.extend(integrity.get(key, []))
    return sorted(set(paths))


def patch_lines(record: dict[str, Any]) -> int | None:
    patch = record.get("patch")
    if not patch:
        return None
    return int(patch.get("insertions", 0)) + int(patch.get("deletions", 0))


def task_id(record: dict[str, Any]) -> str:
    return record["task"]["task_id"]


def category(record: dict[str, Any]) -> str:
    return record["task"]["classification"]["category"]


def evidence_key(record: dict[str, Any], campaign_id: str) -> tuple[str, ...]:
    return (
        campaign_id,
        task_id(record),
        record["task_revision_id"],
        record["base_commit"],
        record["agent"]["agent_id"],
        record["run_id"],
    )


def validate_dataset(
    records: list[dict[str, Any]],
    campaign: dict[str, Any],
    archive: Path,
) -> dict[str, Any]:
    errors: list[str] = []
    if len(records) != EXPECTED_RECORD_COUNT:
        errors.append(f"expected {EXPECTED_RECORD_COUNT} records, found {len(records)}")

    required = {
        "schema_version",
        "run_id",
        "task_revision_id",
        "task",
        "base_commit",
        "agent",
        "execution_provenance",
        "status",
        "outcome",
        "created_at",
    }
    for index, record in enumerate(records, 1):
        missing = sorted(required - record.keys())
        if missing:
            errors.append(f"record {index} missing {', '.join(missing)}")

    live = [record for record in records if record.get("execution_provenance") == "live"]
    tagged = [
        record
        for record in records
        if "validation-campaign" in record.get("task", {}).get("definition", {}).get("tags", [])
    ]
    if len(live) != EXPECTED_RECORD_COUNT:
        errors.append(f"expected 40 live records, found {len(live)}")
    if len(tagged) != EXPECTED_RECORD_COUNT:
        errors.append(f"expected 40 campaign-tagged records, found {len(tagged)}")

    unknown_outcomes = sorted(
        {record.get("outcome") for record in records} - set(OUTCOME_LABELS)
    )
    if unknown_outcomes:
        errors.append(f"unknown outcomes: {unknown_outcomes}")
    nonterminal = [record for record in records if record.get("status") not in {"completed", "failed", "cancelled"}]
    if nonterminal:
        errors.append(f"found {len(nonterminal)} non-terminal records")

    bases = sorted({record.get("base_commit") for record in records})
    if bases != [campaign["baseline_commit"]]:
        errors.append(f"base commits differ from frozen manifest: {bases}")

    by_task: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for record in records:
        by_task[task_id(record)].append(record)
    if len(by_task) != EXPECTED_TASK_COUNT:
        errors.append(f"expected 20 tasks, found {len(by_task)}")
    revisions: dict[str, str] = {}
    for identifier, task_records in sorted(by_task.items()):
        agents = sorted(record["agent"]["agent_id"] for record in task_records)
        task_revisions = {record["task_revision_id"] for record in task_records}
        if len(task_records) != 2 or agents != list(EXPECTED_AGENTS):
            errors.append(f"{identifier} is not a Claude/Codex pair: {agents}")
        if len(task_revisions) != 1:
            errors.append(f"{identifier} has {len(task_revisions)} revisions")
        elif next(iter(task_revisions)) in revisions:
            errors.append(
                f"{identifier} shares revision {next(iter(task_revisions))} with "
                f"{revisions[next(iter(task_revisions))]}"
            )
        else:
            revisions[next(iter(task_revisions))] = identifier

    global_keys = [evidence_key(record, campaign["campaign_id"]) for record in records]
    duplicates = [key for key, count in Counter(global_keys).items() if count > 1]
    if duplicates:
        errors.append(f"duplicate global evidence keys: {duplicates}")

    strata: dict[str, dict[str, Any]] = {}
    for agent in EXPECTED_AGENTS:
        configs = {
            canonical_json(record["agent"])
            for record in records
            if record["agent"]["agent_id"] == agent
        }
        if len(configs) != 1:
            errors.append(f"{agent} has {len(configs)} exported AgentConfig strata")
            continue
        config = json.loads(next(iter(configs)))
        strata[agent] = {
            "forge_config_fingerprint": agent_config_fingerprint(config),
            "agent_config": config,
        }

    point_exports = sorted(archive.glob("campaign-*/T-VAL-*.claude.export.jsonl")) + sorted(
        archive.glob("campaign-*/T-VAL-*.codex.export.jsonl")
    )
    point_records: list[dict[str, Any]] = []
    for path in point_exports:
        rows = read_jsonl(path)
        if len(rows) != 1:
            errors.append(f"point export {path} has {len(rows)} records")
        point_records.extend(rows)
    if len(point_exports) != EXPECTED_RECORD_COUNT:
        errors.append(f"expected 40 point exports, found {len(point_exports)}")
    if Counter(map(canonical_json, point_records)) != Counter(map(canonical_json, records)):
        errors.append("point exports do not match the master export as a multiset")

    environments = sorted(archive.glob("campaign-*/environment.json"))
    environment_values = [json.loads(path.read_text(encoding="utf-8")) for path in environments]
    if len(environment_values) != EXPECTED_TASK_COUNT:
        errors.append(f"expected 20 campaign environments, found {len(environment_values)}")
    for path, value in zip(environments, environment_values):
        if value.get("campaign_id") != campaign["campaign_id"]:
            errors.append(f"campaign id mismatch in {path}")
        if value.get("baseline_commit") != campaign["baseline_commit"]:
            errors.append(f"baseline mismatch in {path}")
        if value.get("tier") != "tier-1-paired":
            errors.append(f"tier mismatch in {path}")
        if value.get("isolation_strategy") != "independent-clone-v1":
            errors.append(f"isolation mismatch in {path}")

    config_files = sorted(archive.glob("campaign-*/participants/T-VAL-*-*/.forge/config.toml"))
    config_digests = sorted({sha256_file(path) for path in config_files})
    if len(config_files) != EXPECTED_RECORD_COUNT:
        errors.append(f"expected 40 participant configs, found {len(config_files)}")
    if len(config_digests) != 1:
        errors.append(f"participant repository configs have {len(config_digests)} digests")

    if errors:
        raise AnalysisError("dataset completeness failed:\n- " + "\n- ".join(errors))

    run_id_counts = Counter(record["run_id"] for record in records)
    infra = sum(record["outcome"] == "errored" for record in records)
    return {
        "valid": True,
        "records": len(records),
        "live_provenance": len(live),
        "campaign_tagged_attempts": len(tagged),
        "included_runs": len(records) - infra,
        "infrastructure_excluded": infra,
        "unique_tasks": len(by_task),
        "unique_task_revisions": len(revisions),
        "complete_pairs": len(by_task),
        "point_exports": len(point_exports),
        "point_exports_match_master": True,
        "missing_exports": 0,
        "duplicate_exports": 0,
        "malformed_records": 0,
        "unknown_outcomes": [],
        "base_commits": bases,
        "agent_counts": dict(sorted(Counter(record["agent"]["agent_id"] for record in records).items())),
        "run_id_counts": dict(sorted(run_id_counts.items())),
        "run_id_scope_note": "run_id is participant-ledger-local; the composite evidence key is globally unique",
        "configuration_strata": strata,
        "participant_config_sha256": config_digests[0],
        "environment_count": len(environment_values),
        "isolation_strategy": "independent-clone-v1",
        "forge_versions": sorted({value.get("forge_version") for value in environment_values}),
        "claude_versions": sorted({value.get("claude_version") for value in environment_values}),
        "codex_versions": sorted({value.get("codex_version") for value in environment_values}),
    }


def selected_records(records: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    return [
        record
        for record in records
        if record["execution_provenance"] == "live"
        and "validation-campaign" in record["task"]["definition"].get("tags", [])
        and record["outcome"] != "errored"
    ]


def outcome_counts(records: Sequence[dict[str, Any]]) -> dict[str, int]:
    counts = Counter(outcome_label(record) for record in records)
    return {label: counts.get(label, 0) for label in OUTCOME_LABELS.values()}


def agent_summary(records: Sequence[dict[str, Any]], agent: str) -> dict[str, Any]:
    cohort = [record for record in records if record["agent"]["agent_id"] == agent]
    counts = outcome_counts(cohort)
    costs = [record["known_cost_usd"] for record in cohort if record.get("known_cost_usd") is not None]
    passes = counts["PASS"]
    return {
        "agent": agent,
        "n": len(cohort),
        "outcomes": counts,
        "pass_rate": passes / len(cohort) if cohort else None,
        "integrity_events": sum(not integrity_clean(record) for record in cohort),
        "runtime_ms": describe([record["agent_runtime_ms"] for record in cohort if record.get("agent_runtime_ms") is not None]),
        "provider_reported_input_tokens": describe(
            [record["provider_reported_input_tokens"] for record in cohort if record.get("provider_reported_input_tokens") is not None]
        ),
        "provider_reported_output_tokens": describe(
            [record["provider_reported_output_tokens"] for record in cohort if record.get("provider_reported_output_tokens") is not None]
        ),
        "provider_reported_total_tokens": {
            **describe(
                [record["provider_reported_total_tokens"] for record in cohort if record.get("provider_reported_total_tokens") is not None]
            ),
            "sum": sum(
                record["provider_reported_total_tokens"]
                for record in cohort
                if record.get("provider_reported_total_tokens") is not None
            ),
        },
        "known_cost_usd": {
            **describe(costs),
            "sum": sum(costs) if costs else None,
            "coverage": len(costs),
            "cost_per_attempted_run": sum(costs) / len(cohort) if len(costs) == len(cohort) else None,
            "cost_per_trustworthy_pass": sum(costs) / passes if costs and passes else None,
        },
        "patch_files_changed": describe(
            [record["patch"]["files_changed"] for record in cohort if record.get("patch")]
        ),
        "patch_insertions": describe(
            [record["patch"]["insertions"] for record in cohort if record.get("patch")]
        ),
        "patch_deletions": describe(
            [record["patch"]["deletions"] for record in cohort if record.get("patch")]
        ),
        "patch_lines_changed": describe(
            [value for record in cohort if (value := patch_lines(record)) is not None]
        ),
    }


def category_summaries(records: Sequence[dict[str, Any]]) -> list[dict[str, Any]]:
    summaries: list[dict[str, Any]] = []
    for task_category in CATEGORY_ORDER:
        for agent in EXPECTED_AGENTS:
            cohort = [
                record
                for record in records
                if category(record) == task_category and record["agent"]["agent_id"] == agent
            ]
            counts = outcome_counts(cohort)
            costs = [record["known_cost_usd"] for record in cohort if record.get("known_cost_usd") is not None]
            runtime_values = [
                record["agent_runtime_ms"] for record in cohort if record.get("agent_runtime_ms") is not None
            ]
            token_values = [
                record["provider_reported_total_tokens"]
                for record in cohort
                if record.get("provider_reported_total_tokens") is not None
            ]
            patch_line_values = [
                value for record in cohort if (value := patch_lines(record)) is not None
            ]
            summaries.append(
                {
                    "category": task_category,
                    "agent": agent,
                    "n": len(cohort),
                    "outcomes": counts,
                    "pass_rate": counts["PASS"] / len(cohort) if cohort else None,
                    "integrity_events": sum(not integrity_clean(record) for record in cohort),
                    "runtime_ms": describe(runtime_values),
                    "median_runtime_ms": median(runtime_values),
                    "provider_reported_total_tokens": describe(token_values),
                    "median_provider_reported_total_tokens": median(token_values),
                    "median_patch_files_changed": median(
                        [record["patch"]["files_changed"] for record in cohort if record.get("patch")]
                    ),
                    "patch_lines_changed": describe(patch_line_values),
                    "median_patch_lines_changed": median(patch_line_values),
                    "known_cost_usd": {**describe(costs), "sum": sum(costs) if costs else None},
                    "known_cost_usd_sum": sum(costs) if costs else None,
                    "known_cost_usd_median": median(costs),
                }
            )
    return summaries


def paired(records: Sequence[dict[str, Any]]) -> list[dict[str, dict[str, Any]]]:
    grouped: dict[tuple[str, str], dict[str, dict[str, Any]]] = defaultdict(dict)
    for record in records:
        grouped[(record["task_revision_id"], record["base_commit"])][record["agent"]["agent_id"]] = record
    pairs = list(grouped.values())
    pairs.sort(
        key=lambda pair: min(parse_time(pair[agent]["created_at"]) for agent in EXPECTED_AGENTS)
    )
    return pairs


def declared_benchmark_metrics(record: dict[str, Any]) -> dict[str, dict[str, Any]]:
    metrics = {}
    for metric in (record.get("evaluation") or {}).get("metrics", []):
        if (
            metric.get("source") == "benchmark"
            and metric.get("direction") != "neutral"
            and metric.get("name") != "benchmark.duration_ms"
        ):
            metrics[metric["name"]] = metric
    return metrics


def benchmark_results(pairs: Sequence[dict[str, dict[str, Any]]]) -> list[dict[str, Any]]:
    results: list[dict[str, Any]] = []
    for pair in pairs:
        claude_metrics = declared_benchmark_metrics(pair["claude"])
        codex_metrics = declared_benchmark_metrics(pair["codex"])
        for name in sorted(set(claude_metrics) & set(codex_metrics)):
            left = claude_metrics[name]
            right = codex_metrics[name]
            if left.get("unit") != right.get("unit") or left.get("direction") != right.get("direction"):
                raise AnalysisError(f"incompatible benchmark metric {task_id(pair['claude'])}:{name}")
            claude_value = float(left["value"])
            codex_value = float(right["value"])
            direction = left["direction"]
            if math.isclose(claude_value, codex_value):
                winner = "tie"
            elif direction == "minimize":
                winner = "claude" if claude_value < codex_value else "codex"
            elif direction == "maximize":
                winner = "claude" if claude_value > codex_value else "codex"
            else:
                winner = "not_comparable"
            absolute_delta = abs(claude_value - codex_value)
            percentage_delta = absolute_delta / abs(claude_value) * 100 if claude_value else None
            results.append(
                {
                    "task_id": task_id(pair["claude"]),
                    "metric": name,
                    "unit": left.get("unit"),
                    "direction": direction,
                    "claude": claude_value,
                    "codex": codex_value,
                    "absolute_delta": absolute_delta,
                    "percentage_delta_vs_claude_baseline": percentage_delta,
                    "winner_by_direction": winner,
                    "preregistered_five_percent_threshold_met": percentage_delta is not None and percentage_delta >= 5.0,
                }
            )
    return results


def pair_winner(
    pair: dict[str, dict[str, Any]], benchmarks: Sequence[dict[str, Any]]
) -> tuple[str | None, str]:
    claude_pass = pair["claude"]["outcome"] == "passed"
    codex_pass = pair["codex"]["outcome"] == "passed"
    if claude_pass != codex_pass:
        return ("claude" if claude_pass else "codex"), "pass_beats_non_pass"
    if not claude_pass:
        return None, "tie_both_non_pass"

    task_benchmarks = [
        result
        for result in benchmarks
        if result["task_id"] == task_id(pair["claude"])
        and result["preregistered_five_percent_threshold_met"]
        and result["winner_by_direction"] in EXPECTED_AGENTS
    ]
    benchmark_winners = {result["winner_by_direction"] for result in task_benchmarks}
    if len(benchmark_winners) == 1:
        return next(iter(benchmark_winners)), "declared_benchmark_at_least_five_percent"
    if len(benchmark_winners) > 1:
        return None, "tie_conflicting_benchmark_metrics"

    claude_runtime = pair["claude"].get("agent_runtime_ms")
    codex_runtime = pair["codex"].get("agent_runtime_ms")
    if claude_runtime and codex_runtime:
        percentage = abs(claude_runtime - codex_runtime) / claude_runtime * 100
        if percentage >= 20.0:
            return ("claude" if claude_runtime < codex_runtime else "codex"), "runtime_at_least_twenty_percent"
    return None, "tie_equivalent"


def pair_rows(
    pairs: Sequence[dict[str, dict[str, Any]]], benchmarks: Sequence[dict[str, Any]]
) -> list[dict[str, Any]]:
    rows = []
    for pair in pairs:
        claude = pair["claude"]
        codex = pair["codex"]
        winner, reason = pair_winner(pair, benchmarks)
        task_benchmarks = [result for result in benchmarks if result["task_id"] == task_id(claude)]
        claude_runtime = claude.get("agent_runtime_ms")
        codex_runtime = codex.get("agent_runtime_ms")
        raw_runtime_winner = None
        if claude_runtime is not None and codex_runtime is not None and claude_runtime != codex_runtime:
            raw_runtime_winner = "claude" if claude_runtime < codex_runtime else "codex"
        rows.append(
            {
                "task_id": task_id(claude),
                "task_revision_id": claude["task_revision_id"],
                "category": category(claude),
                "cutoff": min(claude["created_at"], codex["created_at"]),
                "claude_outcome": outcome_label(claude),
                "codex_outcome": outcome_label(codex),
                "claude_integrity": claude["integrity"]["status"],
                "codex_integrity": codex["integrity"]["status"],
                "claude_integrity_paths": integrity_paths(claude),
                "codex_integrity_paths": integrity_paths(codex),
                "claude_runtime_ms": claude_runtime,
                "codex_runtime_ms": codex_runtime,
                "runtime_delta_ms_claude_minus_codex": (
                    claude_runtime - codex_runtime
                    if claude_runtime is not None and codex_runtime is not None
                    else None
                ),
                "raw_runtime_winner": raw_runtime_winner,
                "claude_input_tokens": claude.get("provider_reported_input_tokens"),
                "claude_output_tokens": claude.get("provider_reported_output_tokens"),
                "claude_total_tokens": claude.get("provider_reported_total_tokens"),
                "codex_input_tokens": codex.get("provider_reported_input_tokens"),
                "codex_output_tokens": codex.get("provider_reported_output_tokens"),
                "codex_total_tokens": codex.get("provider_reported_total_tokens"),
                "claude_patch_files": (claude.get("patch") or {}).get("files_changed"),
                "claude_patch_insertions": (claude.get("patch") or {}).get("insertions"),
                "claude_patch_deletions": (claude.get("patch") or {}).get("deletions"),
                "claude_patch_lines": patch_lines(claude),
                "codex_patch_files": (codex.get("patch") or {}).get("files_changed"),
                "codex_patch_insertions": (codex.get("patch") or {}).get("insertions"),
                "codex_patch_deletions": (codex.get("patch") or {}).get("deletions"),
                "codex_patch_lines": patch_lines(codex),
                "patch_lines_delta_claude_minus_codex": (
                    patch_lines(claude) - patch_lines(codex)
                    if patch_lines(claude) is not None and patch_lines(codex) is not None
                    else None
                ),
                "benchmark_results": task_benchmarks,
                "preregistered_pair_winner": winner,
                "preregistered_pair_reason": reason,
            }
        )
    return rows


def jaccard(left: Iterable[str], right: Iterable[str]) -> float:
    left_set = set(left)
    right_set = set(right)
    union = left_set | right_set
    return len(left_set & right_set) / len(union) if union else 0.0


def objective_tokens(value: str) -> set[str]:
    # Rust's char::is_alphanumeric treats underscore as a separator, unlike
    # Python's regex \w. Build terms character-by-character to keep the replay
    # byte-for-byte faithful to forge-store's semantic token boundary.
    terms: list[str] = []
    current: list[str] = []
    for character in value:
        if character.isalnum():
            current.append(character)
        elif current:
            terms.append("".join(current))
            current = []
    if current:
        terms.append("".join(current))
    return {term.lower() for term in terms if len(term) >= 3}


def similarity(left: dict[str, Any], right: dict[str, Any]) -> float:
    """Exact fixed-weight forge-store task similarity used by the v1 router."""
    score = 0.0
    if left["task"]["repository"] == right["task"]["repository"]:
        score += 0.20
    left_class = left["task"]["classification"]
    right_class = right["task"]["classification"]
    for field, weight in (("category", 0.20), ("language", 0.15), ("domain", 0.15), ("difficulty", 0.10)):
        if left_class.get(field) is not None and left_class.get(field) == right_class.get(field):
            score += weight
    score += 0.10 * jaccard(left["task"].get("components", []), right["task"].get("components", []))
    score += 0.05 * jaccard(left["task"].get("tags", []), right["task"].get("tags", []))
    score += 0.05 * jaccard(
        objective_tokens(left["task"]["objective"]), objective_tokens(right["task"]["objective"])
    )
    return min(score, 1.0)


def evaluator_has_infrastructure_error(record: dict[str, Any]) -> bool:
    evaluation = record.get("evaluation") or {}
    return any(check.get("execution_status") == "error" for check in evaluation.get("checks", []))


def router_eligible(record: dict[str, Any], target: dict[str, Any]) -> bool:
    if record.get("execution_provenance") != "live":
        return False
    if record.get("status") != "completed" or record.get("outcome") == "errored":
        return False
    if record.get("agent_status") in {"start_failed", "cancelled"}:
        return False
    if not integrity_clean(record):
        return False
    if record.get("outcome") != "no_change" and record.get("evaluation") is None:
        return False
    if evaluator_has_infrastructure_error(record):
        return False
    return similarity(target, record) >= 0.20


def prior_records(
    records: Sequence[dict[str, Any]], cutoff: datetime
) -> list[dict[str, Any]]:
    return [
        record
        for record in records
        if (parse_time(record.get("finished_at")) or parse_time(record["created_at"])) <= cutoff
    ]


def forge_route(
    all_records: Sequence[dict[str, Any]],
    pair: dict[str, dict[str, Any]],
) -> dict[str, Any]:
    target = pair["claude"]
    cutoff = min(parse_time(pair[agent]["created_at"]) for agent in EXPECTED_AGENTS)
    historical = prior_records(all_records, cutoff)
    eligible = [record for record in historical if router_eligible(record, target)]
    resolved = [record for record in eligible if record["outcome"] in {"passed", "failed"}]
    per_agent_resolved = {
        agent: sum(record["agent"]["agent_id"] == agent for record in resolved)
        for agent in EXPECTED_AGENTS
    }
    ready = len(resolved) >= 10 and all(per_agent_resolved[agent] >= 3 for agent in EXPECTED_AGENTS)
    scores = []
    for agent in EXPECTED_AGENTS:
        agent_records = [record for record in eligible if record["agent"]["agent_id"] == agent]
        positive_weight = sum(similarity(target, record) for record in agent_records if record["outcome"] == "passed")
        negative_weight = sum(similarity(target, record) for record in agent_records if record["outcome"] == "failed")
        weighted = positive_weight + negative_weight
        scores.append(
            {
                "agent": agent,
                "routing_score": (positive_weight + 1.0) / (weighted + 2.0),
                "positive_count": sum(record["outcome"] == "passed" for record in agent_records),
                "negative_count": sum(record["outcome"] == "failed" for record in agent_records),
                "unresolved_count": sum(record["outcome"] not in {"passed", "failed"} for record in agent_records),
                "weighted_similarity_evidence": weighted,
            }
        )
    scores.sort(key=lambda score: (-score["routing_score"], score["agent"]))
    margin = scores[0]["routing_score"] - scores[1]["routing_score"]
    if ready and margin >= 0.05:
        decision_kind = "selected"
        selected = scores[0]["agent"]
    else:
        decision_kind = "compete_recommended"
        selected = None
    return {
        "task_id": task_id(target),
        "cutoff": cutoff.isoformat().replace("+00:00", "Z"),
        "historical_records": len(historical),
        "eligible_records": len(eligible),
        "resolved_records": len(resolved),
        "per_agent_resolved": per_agent_resolved,
        "evidence_ready": ready,
        "scores": scores,
        "score_margin": margin,
        "minimum_score_margin": 0.05,
        "decision_kind": decision_kind,
        "selected_agent": selected,
    }


def score_selection(
    selector_name: str,
    selections: Sequence[tuple[dict[str, dict[str, Any]], str | None, dict[str, Any]]],
    benchmarks: Sequence[dict[str, Any]],
) -> dict[str, Any]:
    counts = Counter()
    selected_passes = 0
    regret = 0
    task_results = []
    for pair, selected, extra in selections:
        winner, winner_reason = pair_winner(pair, benchmarks)
        if selected is None:
            classification = "no-decision"
        elif winner is None:
            classification = "tie-not-scored"
        elif selected == winner:
            classification = "correct"
        else:
            classification = "incorrect"
        counts[classification] += 1
        if selected is not None:
            selected_passes += int(pair[selected]["outcome"] == "passed")
            exactly_one_pass = sum(pair[agent]["outcome"] == "passed" for agent in EXPECTED_AGENTS) == 1
            if exactly_one_pass and pair[selected]["outcome"] != "passed":
                regret += 1
        task_results.append(
            {
                "task_id": task_id(pair["claude"]),
                "selected_agent": selected,
                "actual_pair_winner": winner,
                "actual_pair_reason": winner_reason,
                "classification": classification,
                **extra,
            }
        )
    decisions = len(selections) - counts["no-decision"]
    comparable = counts["correct"] + counts["incorrect"]
    return {
        "name": selector_name,
        "tasks": len(selections),
        "decision_coverage": decisions / len(selections) if selections else None,
        "decisions": decisions,
        "comparable_predictions": comparable,
        "accuracy": counts["correct"] / comparable if comparable else None,
        "selected_agent_passes": selected_passes,
        "selected_agent_pass_rate": selected_passes / decisions if decisions else None,
        "regret": regret,
        "regret_rate": regret / comparable if comparable else None,
        "classification_counts": {
            label: counts[label]
            for label in ("correct", "incorrect", "tie-not-scored", "no-decision", "not-comparable")
        },
        "task_results": task_results,
    }


def historical_pass_selector(
    records: Sequence[dict[str, Any]],
    pair: dict[str, dict[str, Any]],
    category_only: bool,
) -> tuple[str | None, dict[str, Any]]:
    cutoff = min(parse_time(pair[agent]["created_at"]) for agent in EXPECTED_AGENTS)
    history = [record for record in selected_records(prior_records(records, cutoff))]
    if category_only:
        history = [record for record in history if category(record) == category(pair["claude"])]
    rates = {}
    for agent in EXPECTED_AGENTS:
        cohort = [record for record in history if record["agent"]["agent_id"] == agent]
        rates[agent] = (
            sum(record["outcome"] == "passed" for record in cohort) / len(cohort) if cohort else None
        )
    if rates["claude"] is None or rates["codex"] is None or math.isclose(rates["claude"], rates["codex"]):
        selected = None
    else:
        selected = "claude" if rates["claude"] > rates["codex"] else "codex"
    return selected, {"historical_pass_rates": rates, "historical_attempts": len(history)}


def routing_analysis(
    records: Sequence[dict[str, Any]],
    pairs: Sequence[dict[str, dict[str, Any]]],
    benchmarks: Sequence[dict[str, Any]],
    seed: str,
) -> dict[str, Any]:
    forge_decisions = [(pair, (route := forge_route(records, pair))["selected_agent"], {"router": route}) for pair in pairs]
    baselines: dict[str, Any] = {}
    baselines["forge_router"] = score_selection("forge_router", forge_decisions, benchmarks)
    for agent in EXPECTED_AGENTS:
        baselines[f"always_{agent}"] = score_selection(
            f"always_{agent}", [(pair, agent, {}) for pair in pairs], benchmarks
        )

    random_selections = []
    for pair in pairs:
        revision = pair["claude"]["task_revision_id"]
        digest = hashlib.sha256((seed + revision).encode()).digest()
        selected = EXPECTED_AGENTS[int.from_bytes(digest, "big") % len(EXPECTED_AGENTS)]
        random_selections.append(
            (pair, selected, {"sha256": digest.hex(), "mapping": "digest integer modulo lexicographic candidates"})
        )
    baselines["seeded_random"] = score_selection("seeded_random", random_selections, benchmarks)

    global_selections = []
    category_selections = []
    for pair in pairs:
        selected, details = historical_pass_selector(records, pair, False)
        global_selections.append((pair, selected, details))
        selected, details = historical_pass_selector(records, pair, True)
        category_selections.append((pair, selected, details))
    baselines["best_global_historical"] = score_selection(
        "best_global_historical", global_selections, benchmarks
    )
    baselines["category_aware_historical"] = score_selection(
        "category_aware_historical", category_selections, benchmarks
    )

    ready_tasks = [
        result
        for result in baselines["forge_router"]["task_results"]
        if result["router"]["evidence_ready"]
    ]
    return {
        "router_version": ROUTER_VERSION,
        "temporal_cutoff_rule": "COALESCE(finished_at, created_at) <= earliest pair created_at",
        "minimum_total_resolved_evidence": 10,
        "minimum_resolved_evidence_per_agent": 3,
        "minimum_score_margin": 0.05,
        "tasks_before_evidence_ready": len(pairs) - len(ready_tasks),
        "first_evidence_ready_task": ready_tasks[0]["task_id"] if ready_tasks else None,
        "evidence_ready_tasks": len(ready_tasks),
        "seed": seed,
        "seeded_random_mapping_note": (
            "The plan froze SHA256(seed || task_revision_id) but did not spell out bit-to-agent mapping; "
            "this analysis uses the digest as a big-endian integer modulo lexicographically sorted candidates."
        ),
        "baselines": baselines,
    }


def load_accounting(path: Path | None) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    if path is None:
        return [], {"runs": 0}
    records = read_jsonl(path)
    coverage = {
        "runs": len(records),
        "model_known": sum(record.get("provider_usage", {}).get("model") is not None for record in records),
        "input_output_tokens_known": sum(
            record.get("provider_usage", {}).get("input_tokens") is not None
            and record.get("provider_usage", {}).get("output_tokens") is not None
            for record in records
        ),
        "cached_input_known": sum(record.get("provider_usage", {}).get("cached_input_tokens") is not None for record in records),
        "provider_credits_known": sum(record.get("provider_usage", {}).get("provider_reported_credits") is not None for record in records),
        "derived_credits_known": sum(record.get("derived") is not None for record in records),
        "credit_equivalent_usd_known": sum(
            (record.get("derived") or {}).get("credit_equivalent_usd") is not None for record in records
        ),
        "known_billed_usd": sum(
            record.get("provider_usage", {}).get("provider_reported_cost_usd") is not None for record in records
        ),
    }
    if len(records) != 20:
        raise AnalysisError(f"expected 20 Codex accounting records, found {len(records)}")
    keys = [canonical_json(record["evidence_key"]) for record in records]
    if len(keys) != len(set(keys)):
        raise AnalysisError("duplicate Codex accounting evidence keys")

    credits = [record["derived"]["derived_credits"] for record in records if record.get("derived")]
    cache_ratios = []
    pooled_cached = 0
    pooled_input = 0
    models = Counter()
    by_category: dict[str, list[float]] = defaultdict(list)
    for record in records:
        usage = record["provider_usage"]
        model = usage.get("model")
        if model:
            models[model["model_id"]] += 1
        if usage.get("input_tokens"):
            if usage.get("cached_input_tokens") is not None:
                cache_ratios.append(usage["cached_input_tokens"] / usage["input_tokens"])
                pooled_cached += usage["cached_input_tokens"]
                pooled_input += usage["input_tokens"]
    return records, {
        "coverage": coverage,
        "models": dict(sorted(models.items())),
        "rate_card_ids": sorted(
            {record["derived"]["credit_rate_card_id"] for record in records if record.get("derived")}
        ),
        "derived_credits": {**describe(credits), "sum": sum(credits) if credits else None},
        "cache_hit_ratio_per_run": describe(cache_ratios),
        "cache_hit_ratio_pooled": pooled_cached / pooled_input if pooled_input else None,
        "raw_usage": {
            "input_tokens": describe(
                [record["provider_usage"]["input_tokens"] for record in records if record["provider_usage"].get("input_tokens") is not None]
            ),
            "cached_input_tokens": describe(
                [record["provider_usage"]["cached_input_tokens"] for record in records if record["provider_usage"].get("cached_input_tokens") is not None]
            ),
            "uncached_input_tokens": describe(
                [
                    record["provider_usage"]["input_tokens"] - record["provider_usage"]["cached_input_tokens"]
                    for record in records
                    if record["provider_usage"].get("input_tokens") is not None
                    and record["provider_usage"].get("cached_input_tokens") is not None
                ]
            ),
            "cache_write_input_tokens": describe(
                [record["provider_usage"]["cache_write_input_tokens"] for record in records if record["provider_usage"].get("cache_write_input_tokens") is not None]
            ),
            "output_tokens": describe(
                [record["provider_usage"]["output_tokens"] for record in records if record["provider_usage"].get("output_tokens") is not None]
            ),
            "reasoning_output_tokens": describe(
                [record["provider_usage"]["reasoning_output_tokens"] for record in records if record["provider_usage"].get("reasoning_output_tokens") is not None]
            ),
            "provider_total_tokens": describe(
                [record["provider_usage"]["total_tokens"] for record in records if record["provider_usage"].get("total_tokens") is not None]
            ),
        },
        "credit_equivalent_usd": None,
        "known_billed_usd": None,
        "derivation_blockers": dict(
            sorted(Counter(blocker for record in records for blocker in record.get("derivation_blockers", [])).items())
        ),
    }


def add_accounting_categories(
    accounting_records: Sequence[dict[str, Any]],
    codex_summary: dict[str, Any],
    records: Sequence[dict[str, Any]],
) -> None:
    categories = {record["task_revision_id"]: category(record) for record in records}
    values: dict[str, list[float]] = defaultdict(list)
    for record in accounting_records:
        if record.get("derived"):
            values[categories[record["evidence_key"]["task_revision_id"]]].append(
                record["derived"]["derived_credits"]
            )
    codex_summary["credits_by_category"] = {
        task_category: {**describe(values[task_category]), "sum": sum(values[task_category])}
        for task_category in CATEGORY_ORDER
    }
    pass_count = sum(
        record["agent"]["agent_id"] == "codex" and record["outcome"] == "passed"
        for record in records
    )
    total_credits = codex_summary["derived_credits"]["sum"]
    codex_summary["credits_per_trustworthy_pass"] = (
        total_credits / pass_count if total_credits is not None and pass_count else None
    )


def integrity_analysis(records: Sequence[dict[str, Any]]) -> dict[str, Any]:
    events = []
    for record in records:
        if integrity_clean(record):
            continue
        checks = (record.get("evaluation") or {}).get("checks", [])
        events.append(
            {
                "task_id": task_id(record),
                "agent": record["agent"]["agent_id"],
                "protected_paths": integrity_paths(record),
                "formal_outcome": outcome_label(record),
                "evaluation_verdict": (record.get("evaluation") or {}).get("verdict"),
                "tests_passed": any(check.get("kind") == "test" and check.get("verdict") == "pass" for check in checks),
                "lint_passed": any(check.get("kind") == "lint" and check.get("verdict") == "pass" for check in checks),
                "custom_evaluators": [
                    {"name": check["name"], "verdict": check["verdict"]}
                    for check in checks
                    if check.get("kind") == "custom"
                ],
            }
        )
    green_nonpass = [
        record
        for record in records
        if (record.get("evaluation") or {}).get("verdict") == "pass" and record["outcome"] != "passed"
    ]
    return {
        "events": events,
        "event_count": len(events),
        "green_evaluator_but_non_pass_count": len(green_nonpass),
        "green_evaluator_but_non_pass": [
            {"task_id": task_id(record), "agent": record["agent"]["agent_id"], "outcome": outcome_label(record)}
            for record in green_nonpass
        ],
        "post_hoc_qualitative_classifications": {
            "T-VAL-021": "likely benchmark-design collision",
            "T-VAL-022": "likely benchmark-design collision",
        },
    }


def follow_on_readiness(
    records: Sequence[dict[str, Any]], pair_results: Sequence[dict[str, Any]]
) -> dict[str, Any]:
    by_task = {row["task_id"]: row for row in pair_results}
    task_records = {task_id(record): record for record in records}
    context = sorted(
        identifier
        for identifier, record in task_records.items()
        if "context-experiment" in record["task"]["tags"]
    )
    teams = sorted(
        identifier for identifier, record in task_records.items() if "team-candidate" in record["task"]["tags"]
    )
    team_baselines = []
    for identifier in teams:
        row = by_task[identifier]
        strongest = row["preregistered_pair_winner"]
        if strongest is None:
            passed = [agent for agent in EXPECTED_AGENTS if row[f"{agent}_outcome"] == "PASS"]
            strongest = "/".join(passed) if passed else "no trustworthy PASS"
        team_baselines.append(
            {
                "task_id": identifier,
                "strongest_single_agent": strongest,
                "claude_outcome": row["claude_outcome"],
                "codex_outcome": row["codex_outcome"],
            }
        )
    policy_evidence = [
        record for record in records if "policy-evidence" in record["task"]["tags"]
    ]
    return {
        "context_experiment": {
            "tasks": context,
            "estimated_additional_runs": len(context) * 2,
            "trustworthy_passes_in_tier1": sum(
                by_task[identifier][f"{agent}_outcome"] == "PASS"
                for identifier in context
                for agent in EXPECTED_AGENTS
            ),
            "tier1_attempts": len(context) * 2,
            "assessment": "registered and executable, but Tier 1 has a high PASS ceiling; resource deltas may be more informative than PASS deltas",
        },
        "team_experiment": {
            "tasks": teams,
            "estimated_additional_team_runs": len(teams),
            "strongest_single_agent_baselines": team_baselines,
            "assessment": "ready as a resource/benchmark comparison; the strongest observed single agent passed every candidate task",
        },
        "longitudinal_health": {
            "baseline_snapshot_exists": False,
            "recorded_health_snapshots": 0,
            "accepted_dogfood_hardening_merged_in_v1_0_1": True,
            "accepted_tier1_candidate_changes_merged": 0,
            "meaningful_trend_available": False,
            "assessment": "pending; no baseline snapshot or three-point accepted commit sequence exists",
        },
        "policy_optimization": {
            "tier1_policy_evidence_tagged_runs": len(policy_evidence),
            "eligible_policy_controlled_observations": 0,
            "excluded_manual_or_missing_policy_identity": len(records),
            "comparable_observations_per_arm_required": 8,
            "health_snapshots_required": 3,
            "likely_recommendation_now": "insufficient_evidence",
            "health_observation_pending_after_arm_readiness": True,
            "assessment": "Tier 1 predates policy control; it cannot populate a control/candidate policy comparison",
        },
        "tier2": {
            "recommendation": "RUN A REDUCED TIER 2",
            "tasks": ["T-VAL-016"],
            "estimated_additional_runs": 6,
            "reason": (
                "Repeat only the preregistered performance task whose 10.2% benchmark separation could plausibly move under run-to-run noise. "
                "Do not repeat T-VAL-021 unchanged because its protected-path collision is a design issue, and the other preregistered tasks were paired PASSes without a conclusion-changing ambiguity."
            ),
        },
        "deepseek": {
            "recommendation": "register a separate 20-run extension after adapter/evaluator-layout review",
            "runs": 20,
            "baseline_commit": EXPECTED_BASE,
            "separate_from_tier1": True,
            "tests": [
                "provider neutrality",
                "evaluator portability",
                "new-agent routing cold start",
                "evidence accumulation",
                "Claude/Codex-specific assumptions",
            ],
        },
    }


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n", encoding="utf-8")


def write_category_csv(
    path: Path, summaries: Sequence[dict[str, Any]], metadata: dict[str, Any]
) -> None:
    fields = [
        "campaign_id",
        "baseline_commit",
        "master_export_sha256",
        "analysis_tool_version",
        "generated_at",
        "rate_card_ids",
        "category",
        "agent",
        "n",
        "pass",
        "fail",
        "inconclusive",
        "infrastructure_excluded",
        "pass_rate",
        "integrity_events",
        "median_runtime_ms",
        "median_provider_reported_total_tokens",
        "median_patch_files_changed",
        "median_patch_lines_changed",
        "known_cost_usd_sum",
    ]
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields)
        writer.writeheader()
        for summary in summaries:
            writer.writerow(
                {
                    "campaign_id": metadata["campaign_id"],
                    "baseline_commit": metadata["baseline_commit"],
                    "master_export_sha256": metadata["master_export_sha256"],
                    "analysis_tool_version": metadata["analysis_tool_version"],
                    "generated_at": metadata["generated_at"],
                    "rate_card_ids": ";".join(metadata["rate_card_ids"]),
                    "category": summary["category"],
                    "agent": summary["agent"],
                    "n": summary["n"],
                    "pass": summary["outcomes"]["PASS"],
                    "fail": summary["outcomes"]["FAIL"],
                    "inconclusive": summary["outcomes"]["INCONCLUSIVE"],
                    "infrastructure_excluded": summary["outcomes"]["INFRASTRUCTURE_EXCLUDED"],
                    "pass_rate": summary["pass_rate"],
                    "integrity_events": summary["integrity_events"],
                    "median_runtime_ms": summary["median_runtime_ms"],
                    "median_provider_reported_total_tokens": summary["median_provider_reported_total_tokens"],
                    "median_patch_files_changed": summary["median_patch_files_changed"],
                    "median_patch_lines_changed": summary["median_patch_lines_changed"],
                    "known_cost_usd_sum": summary["known_cost_usd_sum"],
                }
            )


def write_pair_csv(
    path: Path, rows: Sequence[dict[str, Any]], metadata: dict[str, Any]
) -> None:
    fields = [
        "campaign_id",
        "baseline_commit",
        "master_export_sha256",
        "analysis_tool_version",
        "generated_at",
        "rate_card_ids",
        "task_id",
        "task_revision_id",
        "category",
        "cutoff",
        "claude_outcome",
        "codex_outcome",
        "claude_integrity",
        "codex_integrity",
        "claude_integrity_paths",
        "codex_integrity_paths",
        "claude_runtime_ms",
        "codex_runtime_ms",
        "runtime_delta_ms_claude_minus_codex",
        "raw_runtime_winner",
        "claude_input_tokens",
        "claude_output_tokens",
        "claude_total_tokens",
        "codex_input_tokens",
        "codex_output_tokens",
        "codex_total_tokens",
        "claude_patch_files",
        "claude_patch_insertions",
        "claude_patch_deletions",
        "claude_patch_lines",
        "codex_patch_files",
        "codex_patch_insertions",
        "codex_patch_deletions",
        "codex_patch_lines",
        "patch_lines_delta_claude_minus_codex",
        "benchmark_results_json",
        "preregistered_pair_winner",
        "preregistered_pair_reason",
    ]
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields)
        writer.writeheader()
        for row in rows:
            output = {field: row.get(field) for field in fields}
            output.update(
                {
                    "campaign_id": metadata["campaign_id"],
                    "baseline_commit": metadata["baseline_commit"],
                    "master_export_sha256": metadata["master_export_sha256"],
                    "analysis_tool_version": metadata["analysis_tool_version"],
                    "generated_at": metadata["generated_at"],
                    "rate_card_ids": ";".join(metadata["rate_card_ids"]),
                }
            )
            output["claude_integrity_paths"] = ";".join(row["claude_integrity_paths"])
            output["codex_integrity_paths"] = ";".join(row["codex_integrity_paths"])
            output["benchmark_results_json"] = canonical_json(row["benchmark_results"])
            writer.writerow(output)


def markdown_table(headers: Sequence[str], rows: Sequence[Sequence[Any]]) -> str:
    lines = ["| " + " | ".join(headers) + " |", "| " + " | ".join("---" for _ in headers) + " |"]
    lines.extend("| " + " | ".join(str(value) for value in row) + " |" for row in rows)
    return "\n".join(lines)


def write_integrity_review(path: Path, results: dict[str, Any]) -> None:
    integrity = results["integrity"]
    event_rows = []
    for event in integrity["events"]:
        custom = ", ".join(f"{check['name']}={check['verdict']}" for check in event["custom_evaluators"]) or "none"
        event_rows.append(
            [
                event["task_id"],
                event["agent"],
                ", ".join(event["protected_paths"]),
                event["formal_outcome"],
                "yes" if event["tests_passed"] else "no",
                custom,
            ]
        )
    text = f"""# Tier 1 integrity review

Campaign: `{results['metadata']['campaign_id']}`<br>
Master export SHA-256: `{results['metadata']['master_export_sha256']}`<br>
Generated: `{results['metadata']['generated_at']}`

This is additive post-campaign analysis. It does not revise any formal outcome.

## Formal integrity events

{markdown_table(['Task', 'Agent', 'Protected path', 'Outcome', 'Tests passed', 'Custom evaluators'], event_rows)}

Forge refused PASS in **{integrity['green_evaluator_but_non_pass_count']}** runs whose ordinary evaluator verdict was green: T-VAL-012 Claude and both agents on T-VAL-021 and T-VAL-022. This is direct campaign evidence that independent integrity checking changes the trusted result.

## T-VAL-006 Codex evidence note

Trustworthy evidence: the original agent log and patch show a completed Codex execution, the patch modified `crates/forge-cli/tests/run.rs`, Forge recorded the protected-path warning, and the preserved SQLite/WAL contains the terminal run recovered into the non-empty export. The original zero-byte export remains preserved separately. The ledger contains one Codex attempt; no agent rerun occurred.

Contaminated evidence: test, lint, and custom evaluator failures occurred while the host reported `ENOSPC`. They are not independent evidence that the implementation was incorrect. The formal result remains **FAIL** because that is the immutable recorded outcome, and the protected-path modification independently prevents a trustworthy PASS. Under the preregistered rule, `failed` is included as a non-PASS engineering attempt; only `errored` is infrastructure-excluded. The run therefore remains included, with the ENOSPC caveat attached.

## T-VAL-012 Claude note

Tests and lint passed, but Claude modified protected `crates/forge-cli/tests/policy.rs`. Forge correctly recorded **INCONCLUSIVE**. A clean route existed through production code and unprotected unit/store integration tests, so the protected edit was not forced by the task. This remains a genuine integrity event and is not recategorized.

## T-VAL-021 — post hoc qualitative analysis

Classification: **likely benchmark-design collision**.

The new `experiments show` behavior naturally calls for an end-to-end CLI test, and Forge's established CLI integration surface is `crates/forge-cli/tests/run.rs`. Both agents independently added assertions to the existing competition fixture there. A clean implementation path existed—production changes plus unit tests inside editable command modules, relying on the external custom evaluator for reachability—but it was materially less natural and less complete than adding the repository's normal integration test. Both agents choosing the same protected location is evidence of benchmark pressure, not proof of collusion or leakage.

Future campaigns should keep independent evaluator assets protected while separating them from the repository's ordinary editable integration-test surface, or protect existing assertions while explicitly allowing task-authored additions. This post hoc diagnosis does not change either **INCONCLUSIVE** result.

## T-VAL-022 — post hoc qualitative analysis

Classification: **likely benchmark-design collision**.

The task requires proof that preview performs no agent execution, workspace provisioning, routing/policy persistence, or run allocation. Those properties cross CLI, runner, router, policy, store, and filesystem boundaries. The repository's existing `run.rs` fixture is the obvious place to prove them, and both agents independently used it. Unit-only tests were possible after refactoring resolution into pure functions, but they would not cover the full no-side-effect contract as directly. The task therefore put unusually strong pressure on a path the benchmark declared protected.

Future versions should move the secret/independent evaluator outside the editable project tests and permit ordinary integration coverage. Both Tier 1 outcomes remain **INCONCLUSIVE**.
"""
    path.write_text(text, encoding="utf-8")


def write_next_experiments(path: Path, results: dict[str, Any]) -> None:
    readiness = results["follow_on_readiness"]
    team_rows = [
        [row["task_id"], row["strongest_single_agent"], row["claude_outcome"], row["codex_outcome"]]
        for row in readiness["team_experiment"]["strongest_single_agent_baselines"]
    ]
    text = f"""# Tier 1 follow-on experiment readiness

Campaign: `{results['metadata']['campaign_id']}`<br>
Master export SHA-256: `{results['metadata']['master_export_sha256']}`<br>
Generated: `{results['metadata']['generated_at']}`

No experiment described here was executed.

## Context A/B

Frozen subset: {', '.join(f'`{task}`' for task in readiness['context_experiment']['tasks'])}. A full two-arm run with one fixed agent/configuration is **{readiness['context_experiment']['estimated_additional_runs']} additional runs**. Tier 1 produced {readiness['context_experiment']['trustworthy_passes_in_tier1']}/{readiness['context_experiment']['tier1_attempts']} trustworthy PASSes on this subset, so PASS has limited headroom; runtime, tokens, patch behavior, and supplied fact counts are likely the more informative endpoints.

The preregistered rationale remains sound: T-VAL-004 depends on evaluator-contract knowledge beyond the edited file; T-VAL-010 spans a shared command preamble; T-VAL-012 depends on the policy resolver/persistence boundary; and T-VAL-014 depends on understanding what Phase 0–7 history the ledger actually contains. These are repository-context tasks selected before outcomes were known.

## Team vs single

{markdown_table(['Task', 'Strongest/representative single', 'Claude', 'Codex'], team_rows)}

The strongest observed single-agent baseline passed all five tasks. Team runs are ready only as a test of benchmark/resource improvement against that ceiling, at **5 additional team executions**.

## Longitudinal health

The dogfood-driven validation hardening was accepted into the v1.0.1 baseline, but no baseline health snapshot was built. No Tier 1 candidate change was accepted into `main`, and no three-snapshot comparable sequence exists. Longitudinal validation remains pending; isolated candidate branches cannot support a trend.

## Phase 8 policy optimization

Tier 1 contains {readiness['policy_optimization']['tier1_policy_evidence_tagged_runs']} runs tagged `policy-evidence`, but **0 policy-controlled control/candidate observations**. All 40 formal runs predate policy control and are ineligible for a per-arm policy comparison. The default objective needs 8 comparable observations per arm and 3 health snapshots. A proposal now would be `InsufficientEvidence`; `HealthObservationPending` becomes the expected conservative state only after short-term arm evidence exists without the required health window. No proposal or promotion was executed.

## Tier 2

Recommendation: **{readiness['tier2']['recommendation']}** on `{readiness['tier2']['tasks'][0]}` only ({readiness['tier2']['estimated_additional_runs']} runs). {readiness['tier2']['reason']}

## DeepSeek extension

Register, but do not mix, a separate cohort of 20 DeepSeek runs over the same frozen corpus and `v1.0.1` baseline after verifying a DeepSeek adapter/config and the evaluator layout. Give every task its own `independent-clone-v1` execution, preserve a new environment/ledger/export stratum, and analyze it as an extension rather than adding rows to the original Claude-vs-Codex Tier 1 estimate. It would test provider neutrality, evaluator portability, cold-start abstention, evidence accumulation for a new agent, and hidden Claude/Codex assumptions.
"""
    path.write_text(text, encoding="utf-8")


def write_summary(path: Path, results: dict[str, Any]) -> None:
    metadata = results["metadata"]
    completeness = results["completeness"]
    agents = results["agents"]
    categories = results["categories"]
    matrix = results["paired_outcome_matrix"]
    routing = results["routing"]
    accounting = results["codex_accounting"]
    benchmarks = results["benchmarks"]
    integrity = results["integrity"]

    outcome_rows = []
    for agent in EXPECTED_AGENTS:
        summary = agents[agent]
        outcome_rows.append(
            [
                agent,
                summary["outcomes"]["PASS"],
                summary["outcomes"]["FAIL"],
                summary["outcomes"]["INCONCLUSIVE"],
                summary["outcomes"]["INFRASTRUCTURE_EXCLUDED"],
                summary["n"],
                fmt_rate(summary["pass_rate"]),
            ]
        )
    category_rows = [
        [
            row["category"],
            row["agent"],
            row["n"],
            row["outcomes"]["PASS"],
            row["outcomes"]["FAIL"],
            row["outcomes"]["INCONCLUSIVE"],
            fmt_rate(row["pass_rate"]),
            row["integrity_events"],
            fmt_number(row["median_runtime_ms"]),
            fmt_number(row["median_provider_reported_total_tokens"]),
            fmt_number(row["median_patch_lines_changed"]),
        ]
        for row in categories
    ]
    benchmark_rows = [
        [
            row["task_id"],
            row["metric"],
            fmt_number(row["claude"]),
            fmt_number(row["codex"]),
            fmt_number(row["absolute_delta"]),
            f"{row['percentage_delta_vs_claude_baseline']:.2f}%",
            row["winner_by_direction"],
        ]
        for row in benchmarks["metrics"]
    ]
    baseline_rows = []
    for name in (
        "forge_router",
        "always_claude",
        "always_codex",
        "seeded_random",
        "best_global_historical",
        "category_aware_historical",
    ):
        baseline = routing["baselines"][name]
        baseline_rows.append(
            [
                name,
                f"{baseline['decisions']}/20",
                fmt_rate(baseline["accuracy"]),
                fmt_rate(baseline["selected_agent_pass_rate"]),
                baseline["selected_agent_passes"],
                baseline["regret"],
            ]
        )

    claude_cost = agents["claude"]["known_cost_usd"]
    raw_runtime_wins = results["runtime"]["raw_wins"]
    text = f"""# Forge Tier 1 post-campaign analysis

Campaign: `{metadata['campaign_id']}` v{metadata['campaign_version']}<br>
Campaign specification: `{metadata['campaign_specification_tag']}` / `{metadata['campaign_specification_commit']}`<br>
Frozen execution baseline: `{metadata['baseline_tag']}` / `{metadata['baseline_commit']}`<br>
Master export: `{metadata['master_export_path']}`<br>
Master export SHA-256: `{metadata['master_export_sha256']}`<br>
Generated: `{metadata['generated_at']}` by `{metadata['analysis_tool_version']}` (`{metadata['analysis_tool_sha256']}`)

Analysis repository HEAD: `{metadata['analysis_tool_working_tree_base_commit']}` (`main`, derived worktree changes uncommitted as required).

## Completeness and scope

The master contains **{completeness['records']} attempted**, **{completeness['included_runs']} included**, and **{completeness['complete_pairs']} complete paired** runs: 20 Claude and 20 Codex. All are live, campaign-tagged, terminal, and based on `{metadata['baseline_commit']}`. The 40 individual point exports match the master exactly as a multiset. There are no malformed records, unknown outcomes, duplicate composite evidence keys, missing exports, duplicate exports, or infrastructure-excluded runs. Every participant-local ledger allocated `R-0001`, which is why `run_id` alone is deliberately not used as a global key.

This is descriptive evidence from one Rust-heavy repository, 20 maintainer-selected non-random tasks, specific Claude Code/Codex CLI harness versions, the 2026 campaign window, and frozen Forge v1.0.1. It does not establish universal provider superiority.

## Outcomes

{markdown_table(['Agent', 'PASS', 'FAIL', 'INCONCLUSIVE', 'INFRA_EXCLUDED', 'Total', 'PASS rate'], outcome_rows)}

The simple historical analyzer is reproduced exactly: **17/20 PASS (85.0%) for each agent**. The equal headline rate masks different non-PASS types and tasks.

### By category

{markdown_table(['Category', 'Agent', 'N', 'PASS', 'FAIL', 'INCONCLUSIVE', 'PASS rate', 'Integrity', 'Median ms', 'Median tokens', 'Median patch lines'], category_rows)}

These are campaign-specific cells as small as n=2 or n=3, not model rankings.

## Paired outcomes

- Both PASS: **{matrix['both_pass']}**
- Claude PASS / Codex non-PASS: **{matrix['claude_pass_codex_non_pass']}**
- Codex PASS / Claude non-PASS: **{matrix['codex_pass_claude_non_pass']}**
- Both non-PASS: **{matrix['both_non_pass']}**

Non-PASS subtypes are preserved in `paired-results.csv`: T-VAL-006 is PASS/FAIL, T-VAL-012 is INCONCLUSIVE/PASS, and T-VAL-021/T-VAL-022 are INCONCLUSIVE/INCONCLUSIVE.

## Runtime and patch size

Runtime delta is defined as `Claude - Codex`; positive means Codex was faster. Claude median runtime was **{fmt_number(agents['claude']['runtime_ms']['median'])} ms** and Codex median was **{fmt_number(agents['codex']['runtime_ms']['median'])} ms**. Raw per-task speed: Claude faster on **{raw_runtime_wins['claude']}**, Codex faster on **{raw_runtime_wins['codex']}**, exact ties **{raw_runtime_wins['tie']}**. The preregistered 20% threshold is used only for pair winner classification; no new near-tie threshold was invented.

Claude median patch size was **{fmt_number(agents['claude']['patch_lines_changed']['median'])} lines across {fmt_number(agents['claude']['patch_files_changed']['median'])} files**; Codex median was **{fmt_number(agents['codex']['patch_lines_changed']['median'])} lines across {fmt_number(agents['codex']['patch_files_changed']['median'])} files**. Patch size is descriptive, never a quality tiebreaker.

## Provider-reported tokens and accounting

Claude provider-reported medians: **{fmt_number(agents['claude']['provider_reported_input_tokens']['median'])} input**, **{fmt_number(agents['claude']['provider_reported_output_tokens']['median'])} output**, **{fmt_number(agents['claude']['provider_reported_total_tokens']['median'])} total**. Codex export medians: **{fmt_number(agents['codex']['provider_reported_input_tokens']['median'])} input**, **{fmt_number(agents['codex']['provider_reported_output_tokens']['median'])} output**, **{fmt_number(agents['codex']['provider_reported_total_tokens']['median'])} total**. Provider accounting semantics may differ, so those totals are not treated as a direct efficiency contest.

Claude reported **${claude_cost['sum']:.2f} known total USD**, median **${claude_cost['median']:.4f}**, mean/cost per attempted run **${claude_cost['cost_per_attempted_run']:.4f}**, and **${claude_cost['cost_per_trustworthy_pass']:.4f} per trustworthy PASS**. Spending on non-PASS attempts remains in every denominator.

Codex accounting coverage is {accounting['coverage']['runs']} runs; model {accounting['coverage']['model_known']}/20, input/output {accounting['coverage']['input_output_tokens_known']}/20, cached input {accounting['coverage']['cached_input_known']}/20, derived credits {accounting['coverage']['derived_credits_known']}/20, provider credits {accounting['coverage']['provider_credits_known']}/20, billed USD {accounting['coverage']['known_billed_usd']}/20. All recovered models are `{next(iter(accounting['models'])) if accounting['models'] else 'unknown'}`. Total derived credits: **{fmt_number(accounting['derived_credits']['sum'], 4)}**; median: **{fmt_number(accounting['derived_credits']['median'], 4)}**; mean: **{fmt_number(accounting['derived_credits']['mean'], 4)}**; credits per trustworthy PASS: **{fmt_number(accounting['credits_per_trustworthy_pass'], 4)}**; pooled cache-hit ratio: **{fmt_rate(accounting['cache_hit_ratio_pooled'])}**; median per-run ratio: **{fmt_rate(accounting['cache_hit_ratio_per_run']['median'])}**. Codex provider credits, billed USD, and credit-equivalent USD remain unknown—not zero. No dollar-cost winner is claimed.

## Benchmarks

{markdown_table(['Task', 'Metric', 'Claude', 'Codex', 'Abs delta', '% vs Claude', 'Direction winner'], benchmark_rows)}

Directional wins: Claude **{benchmarks['winner_counts'].get('claude', 0)}**, Codex **{benchmarks['winner_counts'].get('codex', 0)}**. T-VAL-017's 3.08% difference is visible but below the preregistered 5% pair-decision threshold.

## Integrity

There were **{integrity['event_count']} integrity-compromised runs** and **{integrity['green_evaluator_but_non_pass_count']} green-evaluator runs that Forge refused to call PASS**. The task-level evidence notes and clearly labeled post hoc T-VAL-021/T-VAL-022 benchmark-design classifications are in `integrity-review.md`; no historical outcome was changed.

## Retrospective routing

The production v1 similarity weights, Beta(1,1) scoring, 10-total/3-per-agent readiness, 0.05 margin, and `compete_when_uncertain` policy were replayed with `COALESCE(finished_at, created_at) <= earliest pair created_at`. Routing first became evidence-ready at **{routing['first_evidence_ready_task'] or 'never'}**; {routing['tasks_before_evidence_ready']} tasks occurred before readiness and {routing['evidence_ready_tasks']} were evidence-ready.

{markdown_table(['Selector', 'Coverage', 'Accuracy', 'Selected PASS rate', 'Routed PASSes', 'Regret'], baseline_rows)}

Accuracy excludes tied pairs; abstention is not scored as wrong. Regret is only a selection of the non-PASS agent where exactly one agent passed. The seeded-random mapping caveat is recorded in `results.json`. Learned routing added value only if it improved accuracy at comparable coverage; on this campaign it **{results['learned_routing_value']}**.

The counterfactual replay uses paired observed outcomes, not unobserved claims: always-Claude and always-Codex each yield 17 trustworthy PASSes; Forge routing yields {routing['baselines']['forge_router']['selected_agent_passes']} PASSes over {routing['baselines']['forge_router']['decisions']} covered tasks and abstains elsewhere.

## Readiness and recommendation

Context A/B is registered for four tasks (8 additional runs) but has a 7/8 Tier 1 PASS ceiling. Team comparison is registered for five tasks (5 team executions), and the best observed single agent passed all five. Longitudinal health is pending: no baseline snapshot, no accepted campaign candidate sequence, and no three comparable points. Phase 8 has 0 eligible policy-controlled observations and would return `InsufficientEvidence` before health could become `HealthObservationPending`.

Recommendation: **RUN A REDUCED TIER 2** on T-VAL-016 only (6 runs), then consider a separately registered 20-run DeepSeek cohort after evaluator-layout review. Do not repeat T-VAL-021 unchanged.

## Reproducibility and boundaries

Run `validation/scripts/run-tier1-analysis.sh` with the frozen archive and Codex session directory. The artifacts record campaign/tag/baseline, export digest, tool version/base commit/tool digest, generation timestamp, and rate card. Raw archives are never opened for writing. No Tier 1 task, record, evaluation, patch, log, candidate, execution semantic, or outcome is modified; no agent, Tier 2, context, team, policy, DeepSeek, commit, tag, or Phase 9 action is performed.
"""
    path.write_text(text, encoding="utf-8")


def analyze(args: argparse.Namespace) -> dict[str, Any]:
    repo = args.repo.resolve()
    campaign_path = args.campaign.resolve()
    master = args.master.resolve()
    archive = args.archive.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)

    campaign_text = campaign_path.read_text(encoding="utf-8")
    campaign = {
        "campaign_id": campaign_scalar(campaign_text, "campaign_id"),
        "campaign_version": int(campaign_scalar(campaign_text, "campaign_version")),
        "baseline_commit": campaign_scalar(campaign_text, "baseline_commit"),
        "baseline_tag": campaign_scalar(campaign_text, "baseline_tag"),
        "routing_seed": campaign_scalar(campaign_text, "routing_baseline_seed"),
    }
    if campaign["baseline_commit"] != EXPECTED_BASE:
        raise AnalysisError(f"frozen baseline changed: {campaign['baseline_commit']}")

    records = read_jsonl(master)
    completeness = validate_dataset(records, campaign, archive)
    included = selected_records(records)
    pairs = paired(included)
    benchmarks = benchmark_results(pairs)
    paired_rows = pair_rows(pairs, benchmarks)
    accounting_records, codex_accounting = load_accounting(args.accounting.resolve() if args.accounting else None)
    add_accounting_categories(accounting_records, codex_accounting, included)

    agent_summaries = {agent: agent_summary(included, agent) for agent in EXPECTED_AGENTS}
    categories = category_summaries(included)
    routing = routing_analysis(included, pairs, benchmarks, campaign["routing_seed"])
    integrity = integrity_analysis(included)

    matrix = {
        "both_pass": sum(pair["claude"]["outcome"] == "passed" and pair["codex"]["outcome"] == "passed" for pair in pairs),
        "claude_pass_codex_non_pass": sum(pair["claude"]["outcome"] == "passed" and pair["codex"]["outcome"] != "passed" for pair in pairs),
        "codex_pass_claude_non_pass": sum(pair["codex"]["outcome"] == "passed" and pair["claude"]["outcome"] != "passed" for pair in pairs),
        "both_non_pass": sum(pair["claude"]["outcome"] != "passed" and pair["codex"]["outcome"] != "passed" for pair in pairs),
        "subtypes": dict(
            sorted(
                Counter((outcome_label(pair["claude"]), outcome_label(pair["codex"])) for pair in pairs).items(),
                key=lambda item: str(item[0]),
            )
        ),
    }
    # JSON object keys cannot be tuples.
    matrix["subtypes"] = {
        f"claude={left},codex={right}": count
        for (left, right), count in Counter(
            (outcome_label(pair["claude"]), outcome_label(pair["codex"])) for pair in pairs
        ).items()
    }

    runtime_deltas = [row["runtime_delta_ms_claude_minus_codex"] for row in paired_rows]
    raw_winners = Counter(row["raw_runtime_winner"] or "tie" for row in paired_rows)
    patch_deltas = [row["patch_lines_delta_claude_minus_codex"] for row in paired_rows]
    benchmark_counts = Counter(row["winner_by_direction"] for row in benchmarks)

    tool_path = Path(__file__).resolve()
    generated_at = args.generated_at or datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    head = git(repo, "rev-parse", "HEAD")
    dirty = bool(git(repo, "status", "--short"))
    campaign_commit = git(repo, "rev-list", "-n", "1", "validation-2026-08")
    metadata = {
        "campaign_id": campaign["campaign_id"],
        "campaign_version": campaign["campaign_version"],
        "campaign_specification_tag": "validation-2026-08",
        "campaign_specification_commit": campaign_commit,
        "baseline_tag": campaign["baseline_tag"],
        "baseline_commit": campaign["baseline_commit"],
        "master_export_path": os.path.relpath(master, repo),
        "master_export_sha256": sha256_file(master),
        "generated_at": generated_at,
        "analysis_tool_version": ANALYSIS_VERSION,
        "analysis_tool_commit": None,
        "analysis_tool_working_tree_base_commit": head,
        "analysis_tool_working_tree_dirty": dirty,
        "analysis_tool_sha256": sha256_file(tool_path),
        "rate_card_ids": codex_accounting.get("rate_card_ids", []),
        "campaign_date_range": {
            "first_created_at": min(record["created_at"] for record in included),
            "last_finished_at": max(record["finished_at"] for record in included),
        },
        "percentile_method": "linear R-7",
    }

    forge_router = routing["baselines"]["forge_router"]
    always_best_accuracy = max(
        routing["baselines"]["always_claude"]["accuracy"] or 0,
        routing["baselines"]["always_codex"]["accuracy"] or 0,
    )
    if forge_router["decisions"] == 0:
        learned_value = "did not demonstrate added selection value because it abstained on every task"
    elif forge_router["decision_coverage"] < 1.0:
        learned_value = "did not beat full-coverage baselines at comparable coverage"
    elif (forge_router["accuracy"] or 0) > always_best_accuracy:
        learned_value = "improved accuracy at comparable coverage"
    else:
        learned_value = "did not improve accuracy over the always-agent baselines"

    results = {
        "metadata": metadata,
        "completeness": completeness,
        "agents": agent_summaries,
        "categories": categories,
        "paired_outcome_matrix": matrix,
        "paired_results": paired_rows,
        "runtime": {
            "delta_sign_convention": "Claude - Codex; positive means Codex faster",
            "paired_delta_ms": describe(runtime_deltas),
            "raw_wins": {agent: raw_winners[agent] for agent in (*EXPECTED_AGENTS, "tie")},
        },
        "patch_size": {
            "paired_lines_delta_claude_minus_codex": describe(patch_deltas),
            "interpretation": "descriptive only; smaller is not automatically better",
        },
        "benchmarks": {
            "metrics": benchmarks,
            "winner_counts": dict(sorted(benchmark_counts.items())),
        },
        "integrity": integrity,
        "routing": routing,
        "codex_accounting": codex_accounting,
        "follow_on_readiness": follow_on_readiness(included, paired_rows),
        "learned_routing_value": learned_value,
        "statistical_limitations": [
            "one repository",
            "Rust-heavy codebase",
            "20 maintainer-selected non-random tasks",
            "one attempt per task and agent",
            "specific Claude Code and Codex CLI harness versions",
            "specific 2026 campaign period",
            "frozen Forge v1.0.1 baseline",
            "provider token-accounting semantics may differ",
        ],
    }

    write_json(output / "results.json", results)
    write_category_csv(output / "category-results.csv", categories, metadata)
    write_pair_csv(output / "paired-results.csv", paired_rows, metadata)
    write_integrity_review(output / "integrity-review.md", results)
    write_next_experiments(output / "next-experiments.md", results)
    write_summary(output / "summary.md", results)
    return results


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--repo", type=Path, default=Path.cwd())
    result.add_argument("--campaign", type=Path, required=True)
    result.add_argument("--master", type=Path, required=True)
    result.add_argument("--archive", type=Path, required=True)
    result.add_argument("--accounting", type=Path)
    result.add_argument("--output", type=Path, required=True)
    result.add_argument("--generated-at", help="fixed RFC3339 timestamp for byte-for-byte reproduction")
    return result


def main() -> int:
    try:
        results = analyze(parser().parse_args())
    except (AnalysisError, OSError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(
        f"analyzed {results['completeness']['records']} records / "
        f"{results['completeness']['complete_pairs']} pairs"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
