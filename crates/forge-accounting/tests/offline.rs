use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use forge_accounting::{
    AccountingCoverage, AccountingError, DerivationBlocker, ENRICHMENT_SCHEMA_VERSION,
    ModelIdentitySource, enrich_codex_evidence, read_enrichment_jsonl, write_enrichment_jsonl,
};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
    environment: PathBuf,
    export: PathBuf,
    agent_log: PathBuf,
    session_log: PathBuf,
}

impl Fixture {
    fn new(campaign: &str, task: &str, revision: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let environment = root.path().join("environment.json");
        let export = root.path().join("run.export.jsonl");
        let agent_log = root.path().join("agent.stdout.log");
        let session_log = root.path().join("rollout.jsonl");
        write(
            &environment,
            &format!(r#"{{"campaign_id":"{campaign}","codex_version":"codex-cli 0.147.0"}}"#),
        );
        write(&export, &export_record(task, revision, "codex", None, None));
        write(
            &agent_log,
            r#"{"type":"thread.started","thread_id":"thread-1"}
{"type":"turn.completed","usage":{"input_tokens":1000000,"cached_input_tokens":800000,"cache_write_input_tokens":0,"output_tokens":100000,"reasoning_output_tokens":25000}}"#,
        );
        write(
            &session_log,
            r#"{"type":"session_meta","payload":{"id":"thread-1"}}
{"type":"turn_context","payload":{"model":"gpt-5.6-sol"}}
{"type":"event_msg","payload":{"info":{"total_token_usage":{"total_tokens":1100000}}}}"#,
        );
        Self {
            root,
            environment,
            export,
            agent_log,
            session_log,
        }
    }

    fn output(&self) -> PathBuf {
        self.root.path().join("enriched.jsonl")
    }

    fn enrich(&self) -> forge_accounting::AccountingEnrichmentRecord {
        enrich_codex_evidence(
            &self.environment,
            &self.export,
            &self.agent_log,
            Some(&self.session_log),
        )
        .unwrap()
    }
}

fn export_record(
    task: &str,
    revision: &str,
    agent: &str,
    model: Option<&str>,
    cost: Option<f64>,
) -> String {
    serde_json::json!({
        "schema_version": 1,
        "run_id": "R-0001",
        "task_revision_id": revision,
        "task": { "task_id": task, "ignored_legacy_field": true },
        "base_commit": "781b32fab791d1d4f839bfb1e5988f4e56150048",
        "agent": {
            "agent_id": agent,
            "harness": if agent == "codex" { "codex-cli" } else { "claude-code" },
            "model": model,
            "timeout_secs": 3600
        },
        "outcome": "passed",
        "known_cost_usd": cost,
        "future_additive_field": { "safe_to_ignore": true }
    })
    .to_string()
}

fn write(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
}

#[test]
fn old_export_deserializes_and_new_enrichment_round_trips() {
    let fixture = Fixture::new("forge-v1", "T-VAL-001", "TR-one");
    let record = fixture.enrich();
    assert_eq!(record.schema_version, ENRICHMENT_SCHEMA_VERSION);
    assert_eq!(record.original_export_schema_version, 1);
    assert_eq!(record.original_outcome.as_deref(), Some("passed"));
    assert_eq!(record.evidence_key.task_id, "T-VAL-001");
    assert_eq!(
        record.provider_usage.model.as_ref().unwrap().source,
        ModelIdentitySource::ProviderSessionLog
    );
    assert!(record.derived.is_some());
    assert_eq!(record.provider_usage.total_tokens, Some(1_100_000));
    assert_eq!(record.provider_usage.supplemental_sources.len(), 1);

    let output = fixture.output();
    write_enrichment_jsonl(&output, &record).unwrap();
    assert_eq!(read_enrichment_jsonl(&output).unwrap(), vec![record]);
}

#[test]
fn enrichment_never_mutates_raw_evidence() {
    let fixture = Fixture::new("forge-v1", "T-VAL-001", "TR-one");
    let paths = [
        &fixture.environment,
        &fixture.export,
        &fixture.agent_log,
        &fixture.session_log,
    ];
    let before = paths
        .iter()
        .map(|path| fs::read(path).unwrap())
        .collect::<Vec<_>>();
    let record = fixture.enrich();
    write_enrichment_jsonl(&fixture.output(), &record).unwrap();
    let after = paths
        .iter()
        .map(|path| fs::read(path).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(before, after);
}

#[test]
fn ledger_local_duplicate_run_ids_cannot_collide() {
    let first = Fixture::new("campaign-a", "T-VAL-001", "TR-one").enrich();
    let second = Fixture::new("campaign-a", "T-VAL-008", "TR-eight").enrich();
    assert_eq!(first.evidence_key.run_id, second.evidence_key.run_id);
    assert_ne!(first.evidence_key, second.evidence_key);
    let keys = [first.evidence_key, second.evidence_key]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(keys.len(), 2);
}

#[test]
fn omitted_session_keeps_model_and_derived_credits_unknown() {
    let fixture = Fixture::new("forge-v1", "T-VAL-001", "TR-one");
    let record = enrich_codex_evidence(
        &fixture.environment,
        &fixture.export,
        &fixture.agent_log,
        None,
    )
    .unwrap();
    assert_eq!(record.provider_usage.model, None);
    assert_eq!(record.derived, None);
    assert_eq!(
        record.derivation_blockers,
        vec![DerivationBlocker::ModelUnknown]
    );
}

#[test]
fn explicit_immutable_forge_model_is_the_fallback_source() {
    let fixture = Fixture::new("forge-v1", "T-VAL-001", "TR-one");
    write(
        &fixture.export,
        &export_record("T-VAL-001", "TR-one", "codex", Some("gpt-5.6-sol"), None),
    );
    let record = enrich_codex_evidence(
        &fixture.environment,
        &fixture.export,
        &fixture.agent_log,
        None,
    )
    .unwrap();
    assert_eq!(
        record.provider_usage.model.unwrap().source,
        ModelIdentitySource::ExplicitForgeConfiguration
    );
    assert!(record.derived.is_some());
}

#[test]
fn claude_exports_are_rejected_and_their_cost_is_untouched() {
    let fixture = Fixture::new("forge-v1", "T-VAL-001", "TR-one");
    let claude = export_record("T-VAL-001", "TR-one", "claude", None, Some(0.42));
    write(&fixture.export, &claude);
    let before = fs::read(&fixture.export).unwrap();
    assert!(matches!(
        enrich_codex_evidence(
            &fixture.environment,
            &fixture.export,
            &fixture.agent_log,
            Some(&fixture.session_log)
        ),
        Err(AccountingError::NotCodex(agent)) if agent == "claude"
    ));
    assert_eq!(fs::read(&fixture.export).unwrap(), before);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&before).unwrap()["known_cost_usd"],
        0.42
    );
}

#[test]
fn command_line_enrichment_and_coverage_need_no_network() {
    let fixture = Fixture::new("forge-v1", "T-VAL-001", "TR-one");
    let output = fixture.output();
    let binary = env!("CARGO_BIN_EXE_forge-accounting");
    let status = Command::new(binary)
        .args([
            "enrich-codex",
            "--environment",
            fixture.environment.to_str().unwrap(),
            "--export",
            fixture.export.to_str().unwrap(),
            "--agent-log",
            fixture.agent_log.to_str().unwrap(),
            "--session-log",
            fixture.session_log.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .status()
        .unwrap();
    assert!(status.success());

    let coverage = Command::new(binary)
        .args(["coverage", output.to_str().unwrap()])
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .output()
        .unwrap();
    assert!(coverage.status.success());
    let stdout = String::from_utf8(coverage.stdout).unwrap();
    let compact = stdout
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>();
    assert!(compact.iter().any(|line| line == "runs 1"), "{stdout}");
    assert!(
        compact.iter().any(|line| line == "model known 1"),
        "{stdout}"
    );
    assert!(
        compact.iter().any(|line| line == "known billed USD 0"),
        "{stdout}"
    );
}

#[test]
fn coverage_counts_provider_and_derived_bases_independently() {
    let fixture = Fixture::new("forge-v1", "T-VAL-001", "TR-one");
    let record = fixture.enrich();
    assert_eq!(
        AccountingCoverage::from_records(&[record]),
        AccountingCoverage {
            runs: 1,
            model_known: 1,
            input_output_tokens_known: 1,
            cached_input_known: 1,
            provider_credits_known: 0,
            derived_credits_known: 1,
            credit_equivalent_usd_known: 0,
            provider_reported_cost_usd_known: 0,
        }
    );
}
