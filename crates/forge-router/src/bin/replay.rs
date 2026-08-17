//! Offline historical replay through the production Forge store/router path.
//!
//! This binary never opens the repository's operational ledger. It imports a
//! JSONL export into a temporary SQLite store, remaps ledger-local run ids
//! deterministically, and invokes the same evidence resolver and router used
//! by `forge run --agent auto`.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use forge_core::agent::AgentConfig;
use forge_core::config::RoutingConfig;
use forge_core::events::EvaluationSubject;
use forge_core::ids::{AgentId, RunId};
use forge_core::result::Evaluation;
use forge_core::routing::{
    CandidateAgent, CandidateAgentSet, ExplorationPolicy, MinimumRoutingEvidence,
    RoutingEvidencePolicy, RoutingRequest,
};
use forge_core::run::{AgentExecution, AgentExecutionStatus, AgentRun, Usage};
use forge_core::task::TaskRevision;
use forge_router::RoutingContract;
use forge_store::{ExportRecord, Store};

#[derive(Debug)]
struct Arguments {
    input: PathBuf,
    minimum_score_margin: f64,
    summary: bool,
}

impl Arguments {
    fn parse() -> Result<Self, Box<dyn Error>> {
        let mut values = std::env::args().skip(1);
        let mut input = None;
        let mut minimum_score_margin: f64 = 0.05;
        let mut summary = false;
        while let Some(argument) = values.next() {
            match argument.as_str() {
                "--input" => input = values.next().map(PathBuf::from),
                "--minimum-score-margin" => {
                    minimum_score_margin = values
                        .next()
                        .ok_or("--minimum-score-margin requires a value")?
                        .parse()?;
                }
                "--summary" => summary = true,
                "--help" | "-h" => {
                    println!(
                        "usage: forge-router-replay --input EXPORT.jsonl \
                         [--minimum-score-margin 0.05] [--summary]"
                    );
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument `{other}`").into()),
            }
        }
        let input = input.ok_or("--input is required")?;
        if !minimum_score_margin.is_finite() || minimum_score_margin < 0.0 {
            return Err("minimum score margin must be finite and non-negative".into());
        }
        Ok(Self {
            input,
            minimum_score_margin,
            summary,
        })
    }
}

#[derive(serde::Serialize)]
struct ReplayOutput<'a> {
    task_id: &'a str,
    cutoff: chrono::DateTime<chrono::Utc>,
    original_run_ids_are_ledger_local: bool,
    decision: &'a forge_core::RoutingDecisionRecord,
}

#[derive(serde::Serialize)]
struct ReplaySummary<'a> {
    task_id: &'a str,
    cutoff: chrono::DateTime<chrono::Utc>,
    decision: forge_core::routing::RoutingDecisionKind,
    selected_agent: Option<&'a str>,
    historical_runs_found: u64,
    eligible_runs: u64,
    resolved_runs: u64,
    ready: bool,
    scores: BTreeMap<&'a str, f64>,
    decision_margin: Option<f64>,
    evidence_fingerprint: &'a str,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let arguments = Arguments::parse()?;
    let records = read_records(&arguments.input)?;
    if records.is_empty() {
        return Err("the replay export is empty".into());
    }

    let mut grouped: BTreeMap<(String, String), Vec<&ExportRecord>> = BTreeMap::new();
    for record in &records {
        grouped
            .entry((
                record.task_revision_id.to_string(),
                record.base_commit.clone(),
            ))
            .or_default()
            .push(record);
    }
    let mut pairs = grouped.into_values().collect::<Vec<_>>();
    pairs.sort_by_key(|pair| pair.iter().map(|record| record.created_at).min());

    let temporary = tempfile::tempdir()?;
    let config = RoutingConfig {
        minimum_score_margin: arguments.minimum_score_margin,
        ..RoutingConfig::default()
    };
    for (pair_index, pair) in pairs.into_iter().enumerate() {
        let first = pair.first().ok_or("empty replay pair")?;
        let cutoff = pair
            .iter()
            .map(|record| record.created_at)
            .min()
            .ok_or("replay pair has no cutoff")?;

        // Reconstruct the database as it existed immediately before this
        // decision. Importing the complete campaign and relying only on SQL
        // cutoffs would expose records which did not exist yet as exclusions,
        // changing the snapshot fingerprint even though they were ineligible.
        let store =
            Store::open(temporary.path().join(format!("replay-{pair_index:04}.db"))).await?;
        import_records(
            &store,
            &records
                .iter()
                .filter(|record| record.created_at < cutoff)
                .collect::<Vec<_>>(),
        )
        .await?;

        let task_revision = TaskRevision::from_stored(
            first.task_revision_id.clone(),
            first.task.definition.clone(),
        )?;
        let stored_revision_id = store.upsert_task(&first.task.definition).await?;
        if stored_revision_id != first.task_revision_id {
            return Err(format!(
                "task {} recomputed as revision {}, expected {}",
                first.task.task_id, stored_revision_id, first.task_revision_id
            )
            .into());
        }
        let candidates = candidate_set(&pair)?;
        let request = RoutingRequest::new(
            task_revision,
            candidates,
            RoutingEvidencePolicy::default(),
            MinimumRoutingEvidence {
                total: config.minimum_total_evidence,
                per_agent: config.minimum_agent_evidence,
            },
            ExplorationPolicy::CompeteWhenUncertain,
            cutoff,
        );
        let decision = RoutingContract::new(store.clone())
            .route(&request, &config)
            .await?;
        if arguments.summary {
            println!(
                "{}",
                serde_json::to_string(&summary(first.task.task_id.as_str(), cutoff, &decision))?
            );
        } else {
            println!(
                "{}",
                serde_json::to_string(&ReplayOutput {
                    task_id: first.task.task_id.as_str(),
                    cutoff,
                    original_run_ids_are_ledger_local: true,
                    decision: &decision,
                })?
            );
        }
    }
    Ok(())
}

fn summary<'a>(
    task_id: &'a str,
    cutoff: chrono::DateTime<chrono::Utc>,
    record: &'a forge_core::RoutingDecisionRecord,
) -> ReplaySummary<'a> {
    let (evidence, decision_margin) = match &record.decision {
        forge_core::RoutingDecision::Selected {
            evidence_summary,
            decision_margin,
            ..
        }
        | forge_core::RoutingDecision::InsufficientEvidence {
            evidence_summary,
            decision_margin,
            ..
        }
        | forge_core::RoutingDecision::CompeteRecommended {
            evidence_summary,
            decision_margin,
            ..
        } => (evidence_summary, *decision_margin),
    };
    let minimum = record.decision.snapshot().minimum_evidence;
    let ready = evidence.eligible_runs > 0
        && evidence.similar_task_revisions > 0
        && evidence.resolved_runs >= minimum.total
        && evidence
            .per_agent
            .iter()
            .all(|agent| agent.resolved >= minimum.per_agent);
    ReplaySummary {
        task_id,
        cutoff,
        decision: record.decision.kind(),
        selected_agent: record
            .selected
            .as_ref()
            .map(|agent| agent.agent_id.as_str()),
        historical_runs_found: evidence.historical_runs_found,
        eligible_runs: evidence.eligible_runs,
        resolved_runs: evidence.resolved_runs,
        ready,
        scores: record
            .decision
            .scores()
            .iter()
            .map(|score| (score.agent.agent_id.as_str(), score.routing_score))
            .collect(),
        decision_margin,
        evidence_fingerprint: &record.evidence_fingerprint,
    }
}

fn read_records(path: &PathBuf) -> Result<Vec<ExportRecord>, Box<dyn Error>> {
    BufReader::new(File::open(path)?)
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let line = line?;
            serde_json::from_str(&line)
                .map_err(|error| format!("invalid JSONL record {}: {error}", index + 1).into())
        })
        .collect()
}

fn candidate_set(records: &[&ExportRecord]) -> Result<CandidateAgentSet, Box<dyn Error>> {
    let mut configs: BTreeMap<AgentId, AgentConfig> = BTreeMap::new();
    for record in records {
        if let Some(existing) = configs.insert(record.agent.agent_id.clone(), record.agent.clone())
            && existing != record.agent
        {
            return Err(format!(
                "task {} contains two configurations for agent {}",
                record.task.task_id, record.agent.agent_id
            )
            .into());
        }
    }
    CandidateAgentSet::new(
        configs
            .into_iter()
            .map(|(agent_id, config)| CandidateAgent::new(agent_id, config))
            .collect::<Result<Vec<_>, _>>()?,
    )
    .map_err(Into::into)
}

async fn import_records(store: &Store, records: &[&ExportRecord]) -> Result<(), Box<dyn Error>> {
    let mut ordered = records.to_vec();
    ordered.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.task.task_id.cmp(&right.task.task_id))
            .then_with(|| left.agent.agent_id.cmp(&right.agent.agent_id))
    });

    for (index, record) in ordered.into_iter().enumerate() {
        let revision_id = store.upsert_task(&record.task.definition).await?;
        if revision_id != record.task_revision_id {
            return Err(format!(
                "task {} recomputed as revision {}, expected {}",
                record.task.task_id, revision_id, record.task_revision_id
            )
            .into());
        }
        let run_id = RunId::sequential((index + 1) as u64);
        let mut run = AgentRun::new(
            run_id.clone(),
            record.task.task_id.clone(),
            record.agent.clone(),
            record.base_commit.clone(),
        );
        run.execution_provenance = record.execution_provenance;
        run.selection_source = record.selection_source.clone();
        run.status = record.status;
        run.created_at = record.created_at;
        run.started_at = record.started_at;
        run.finished_at = record.finished_at;
        run.failure_reason = record.failure_reason.clone();
        run.infrastructure_failures = record.infrastructure_failures.clone();
        run.outcome = record.outcome;
        run.integrity = record.integrity.clone();
        run.patch = record.patch.clone();
        run.warnings = record.warnings.clone();
        run.evaluation_verdict = record.evaluation.as_ref().map(|value| value.verdict);
        run.execution = execution(record);
        store
            .save_run_at_task_revision(&run, None, &record.task_revision_id)
            .await?;
        if let Some(patch) = &record.patch {
            store.record_patch(&run_id, patch).await?;
        }
        if let Some(evaluation) = &record.evaluation {
            let mut evaluation: Evaluation = evaluation.clone();
            evaluation.subject = EvaluationSubject::Run(run_id);
            store.record_evaluation(&evaluation).await?;
        }
    }
    Ok(())
}

fn execution(record: &ExportRecord) -> Option<AgentExecution> {
    let status = record.agent_status?;
    let started_at = record.started_at.unwrap_or(record.created_at);
    let finished_at = record.finished_at.unwrap_or(started_at);
    Some(AgentExecution {
        status,
        exit_code: None,
        timed_out: status == AgentExecutionStatus::TimedOut,
        started_at,
        finished_at,
        duration_ms: record.agent_runtime_ms.unwrap_or(0),
        stdout_path: None,
        stderr_path: None,
        usage: Usage {
            input_tokens: record.provider_reported_input_tokens,
            output_tokens: record.provider_reported_output_tokens,
            cost_usd: record.known_cost_usd,
        },
        self_report: None,
        harness_metadata: BTreeMap::new(),
        infrastructure_failures: Vec::new(),
    })
}
