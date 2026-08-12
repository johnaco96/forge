//! `forge policy` — durable, evidence-backed engineering policy lifecycle.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use forge_core::ids::{PolicyExperimentId, PolicyId, PolicyProposalId};
use forge_core::optimization::{
    ExperimentBudget, PolicyEvent, PolicyEventPayload, PolicyEventSubject, PolicyExperimentStatus,
    ProposalRecommendation,
};
use forge_core::policy::{PolicyBounds, PolicyProvenance, PolicyStatus};
use forge_policy::{
    BaselineOptimizer, OptimizationRequest, PolicyEvidenceResolver, PolicyOptimizer,
    create_policy_experiment, ensure_bootstrap_policy, promote_proposal,
    rollback_policy as execute_rollback,
};
use forge_store::Store;

use crate::commands::run::resolve_repository;

pub struct ProposeArgs {
    pub candidate: Option<PolicyId>,
    pub max_world_facts: Option<u32>,
    pub timeout_secs: Option<u64>,
    pub minimum_score_margin: Option<f64>,
    pub learned_routing: Option<bool>,
    pub cutoff: Option<DateTime<Utc>>,
}

async fn context(repo: Option<PathBuf>) -> Result<(forge_core::ForgeConfig, Store)> {
    let (_, layout, config) = resolve_repository(repo.as_deref())?;
    let store = Store::open(layout.store_path(&config))
        .await
        .with_context(|| format!("opening the ledger at {}", config.store.path))?;
    Ok((config, store))
}

pub async fn show(repo: Option<PathBuf>) -> Result<()> {
    let (config, store) = context(repo).await?;
    let policy = ensure_bootstrap_policy(&store, &config).await?;
    println!("Forge engineering policy {}\n", policy.policy_id);
    println!("Status\n  {}", policy.status);
    println!(
        "Lineage\n  parent {}\n  fingerprint {}\n  provenance {}",
        policy
            .parent_policy_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "none (bootstrap root)".into()),
        policy.fingerprint(),
        policy.provenance.as_str()
    );
    println!("\nSettings");
    for (name, value) in policy.display_rows() {
        println!("  {name:<12} {value}");
    }
    println!("\nObjective\n  version {}", policy.objective.version);
    for term in &policy.objective.terms {
        println!(
            "  {} · {} · {}",
            term.metric,
            term.direction,
            if term.kind.is_hard() { "HARD" } else { "soft" }
        );
    }
    let bounds = PolicyBounds::for_config(&config);
    println!(
        "\nBounds\n  max facts {} · timeout {}s · retries {} · parallel team nodes {} · reviewers {}",
        bounds.max_world_facts,
        bounds.max_timeout_secs,
        bounds.max_retries,
        bounds.max_parallel_team_nodes,
        bounds.max_review_nodes
    );
    println!("\nFixed guardrails");
    for guardrail in policy.guardrails.iter() {
        println!("  {guardrail}: {}", guardrail.rationale());
    }
    Ok(())
}

pub async fn history(repo: Option<PathBuf>, limit: u32) -> Result<()> {
    let (config, store) = context(repo).await?;
    ensure_bootstrap_policy(&store, &config).await?;
    println!("Forge engineering policy history\n");
    for entry in store.policy_history(&config.repository.name, limit).await? {
        println!(
            "{}{}  {:<11} parent={} provenance={} fingerprint={}",
            entry.policy_id,
            if entry.is_active { " *" } else { "  " },
            entry.status,
            entry
                .parent_policy_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "none".into()),
            entry.provenance,
            entry.fingerprint
        );
    }
    Ok(())
}

pub async fn propose(repo: Option<PathBuf>, args: ProposeArgs) -> Result<()> {
    let (config, store) = context(repo).await?;
    let active = ensure_bootstrap_policy(&store, &config).await?;
    let proposal_id = store.next_policy_proposal_id().await?;
    let mut created = false;
    let candidate = if let Some(candidate_id) = args.candidate {
        if args.max_world_facts.is_some()
            || args.timeout_secs.is_some()
            || args.minimum_score_margin.is_some()
            || args.learned_routing.is_some()
        {
            bail!("candidate id cannot be combined with candidate-setting flags");
        }
        store
            .policy_by_id(&candidate_id)
            .await?
            .ok_or_else(|| anyhow!("policy `{candidate_id}` does not exist"))?
    } else {
        if args.max_world_facts.is_none()
            && args.timeout_secs.is_none()
            && args.minimum_score_margin.is_none()
            && args.learned_routing.is_none()
        {
            bail!(
                "name one bounded change: --max-world-facts, --timeout-secs, \
                 --minimum-score-margin, or --learned-routing"
            );
        }
        let mut candidate = active.clone();
        candidate.policy_id = store.next_policy_id().await?;
        candidate.parent_policy_id = Some(active.policy_id.clone());
        candidate.created_at = Utc::now();
        candidate.status = PolicyStatus::Draft;
        candidate.provenance = PolicyProvenance::OptimizerProposed;
        candidate.optimizer_version = Some(forge_policy::OPTIMIZER_VERSION.into());
        candidate.proposal_id = Some(proposal_id.clone());
        if let Some(value) = args.max_world_facts {
            candidate.context.max_world_facts = value;
        }
        if let Some(value) = args.timeout_secs {
            candidate.resources.timeout_secs = value;
        }
        if let Some(value) = args.minimum_score_margin {
            candidate.routing.minimum_score_margin = value;
        }
        if let Some(value) = args.learned_routing {
            candidate.routing.use_learned_routing = value;
        }
        if candidate.changed_dimensions(&active).is_empty() {
            bail!("the candidate is identical to the active policy");
        }
        candidate
            .validate(&PolicyBounds::for_config(&config))
            .map_err(|error| anyhow!(error))?;
        store.insert_policy(&candidate).await?;
        let subject = PolicyEventSubject::Policy(candidate.policy_id.clone());
        store
            .append_policy_events(&[PolicyEvent {
                seq: store.next_policy_event_seq(&subject).await?,
                subject,
                timestamp: candidate.created_at,
                payload: PolicyEventPayload::PolicyCreated {
                    provenance: candidate.provenance.as_str().into(),
                    fingerprint: candidate.fingerprint(),
                },
            }])
            .await?;
        created = true;
        candidate
    };
    if candidate.repository != active.repository
        || candidate.parent_policy_id.as_ref() != Some(&active.policy_id)
        || candidate.objective != active.objective
    {
        bail!("candidate is not a direct, same-objective successor of the active policy");
    }

    let cutoff = args.cutoff.unwrap_or_else(Utc::now);
    let resolved = PolicyEvidenceResolver::new(store.clone())
        .resolve(&active, &candidate, cutoff)
        .await?;
    let proposal = BaselineOptimizer::new().propose(OptimizationRequest {
        proposal_id: proposal_id.clone(),
        active: &active,
        candidate: &candidate,
        evidence: &resolved.snapshot,
        objective: &active.objective,
        bounds: &PolicyBounds::for_config(&config),
        health: resolved.health,
    })?;
    store
        .insert_policy_proposal(&proposal, &resolved.snapshot)
        .await?;
    let subject = PolicyEventSubject::Proposal(proposal_id.clone());
    store
        .append_policy_events(&[PolicyEvent {
            seq: store.next_policy_event_seq(&subject).await?,
            subject,
            timestamp: proposal.created_at,
            payload: PolicyEventPayload::PolicyProposalCreated {
                candidate_policy_id: candidate.policy_id.clone(),
                recommendation: proposal.recommendation,
                evidence_fingerprint: proposal.evidence_fingerprint.clone(),
            },
        }])
        .await?;
    if created && proposal.recommendation == ProposalRecommendation::ShadowTest {
        store
            .set_policy_status(&candidate.policy_id, PolicyStatus::Shadow)
            .await?;
    }

    println!("Forge policy proposal {}\n", proposal.proposal_id);
    println!(
        "Candidate\n  {}  {}",
        candidate.policy_id,
        candidate.describe_changes(&active).join("; ")
    );
    println!(
        "\nEvidence\n  cutoff {}\n  {} eligible · {} excluded\n  fingerprint {}",
        proposal.cutoff.to_rfc3339(),
        proposal.eligible_observations,
        proposal.excluded_observations,
        proposal.evidence_fingerprint
    );
    println!(
        "\nConclusion\n  {} · {} · {} evidence",
        proposal.recommendation, proposal.comparison, proposal.evidence_strength
    );
    for explanation in &proposal.explanation {
        println!("  - {explanation}");
    }
    Ok(())
}

pub async fn compare(repo: Option<PathBuf>, proposal_id: PolicyProposalId) -> Result<()> {
    let (_, store) = context(repo).await?;
    let proposal = store
        .policy_proposal_by_id(&proposal_id)
        .await?
        .ok_or_else(|| anyhow!("proposal `{proposal_id}` does not exist"))?;
    let evidence = store
        .policy_proposal_evidence(&proposal_id)
        .await?
        .ok_or_else(|| anyhow!("proposal `{proposal_id}` has no evidence snapshot"))?;
    println!("Forge policy comparison {}\n", proposal_id);
    println!(
        "Evidence\n  {} eligible · {} excluded · {} health snapshots",
        evidence.eligible.len(),
        evidence.excluded.len(),
        evidence.health.len()
    );
    for (reason, count) in evidence.exclusion_breakdown() {
        println!("  excluded {reason}: {count}");
    }
    println!("\nControl\n{}", summary(&proposal.control_summary));
    println!("\nCandidate\n{}", summary(&proposal.candidate_summary));
    println!("\nHard constraints");
    for result in &proposal.constraint_results {
        println!(
            "  {}  {}  {}",
            if result.satisfied { "PASS" } else { "FAIL" },
            result.metric,
            result.detail
        );
    }
    println!("\nObjectives");
    for outcome in &proposal.objective_outcomes {
        println!("  {}  {}", outcome.metric, outcome.describe());
    }
    println!(
        "\nConclusion\n  comparison {} · recommendation {} · evidence {}",
        proposal.comparison, proposal.recommendation, proposal.evidence_strength
    );
    Ok(())
}

fn summary(summary: &forge_core::PolicyOutcomeSummary) -> String {
    format!(
        "  observations {} · pass {} · fail {} · inconclusive {} · no-change {}\n  \
         integrity {}/{} · runtime {} · tokens {} · cost {}",
        summary.observations,
        summary.passed,
        summary.failed,
        summary.inconclusive,
        summary.no_change,
        summary.integrity_clean,
        summary.observations,
        summary
            .mean_runtime_ms()
            .map(|value| format!("{value:.1}ms"))
            .unwrap_or_else(|| "not measured".into()),
        summary
            .mean_tokens()
            .map(|value| format!("{value:.1}"))
            .unwrap_or_else(|| "not measured".into()),
        summary
            .mean_cost_usd()
            .map(|value| format!("${value:.4}"))
            .unwrap_or_else(|| "not measured".into())
    )
}

pub async fn experiment_create(
    repo: Option<PathBuf>,
    proposal: PolicyProposalId,
    candidate_share_percent: u32,
    budget: ExperimentBudget,
) -> Result<()> {
    let (config, store) = context(repo).await?;
    ensure_bootstrap_policy(&store, &config).await?;
    let experiment = create_policy_experiment(
        &store,
        &config.repository.name,
        &proposal,
        candidate_share_percent,
        budget,
    )
    .await?;
    print_experiment(&experiment);
    Ok(())
}

pub async fn experiment_show(repo: Option<PathBuf>, experiment: PolicyExperimentId) -> Result<()> {
    let (_, store) = context(repo).await?;
    let experiment = store
        .policy_experiment_by_id(&experiment)
        .await?
        .ok_or_else(|| anyhow!("policy experiment `{experiment}` does not exist"))?;
    print_experiment(&experiment);
    let assignments = store
        .experiment_assignment_count(&experiment.experiment_id)
        .await?;
    let observations = store
        .experiment_observations(&experiment.experiment_id)
        .await?;
    println!(
        "  assignments {assignments} · observations {}",
        observations.len()
    );
    Ok(())
}

fn print_experiment(experiment: &forge_core::PolicyExperiment) {
    println!("Forge policy experiment {}\n", experiment.experiment_id);
    println!(
        "  {} → {} · {}% candidate · status {}",
        experiment.control_policy_id,
        experiment.candidate_policy_id,
        experiment.assignment.candidate_share_percent,
        experiment.status
    );
    println!(
        "  budget {} tasks · {} extra runs · cost {}",
        experiment.budget.max_tasks,
        experiment.budget.max_extra_runs,
        experiment
            .budget
            .max_extra_cost_usd
            .map(|cost| format!("${cost:.2}"))
            .unwrap_or_else(|| "not bounded because pre-execution cost is unavailable".into())
    );
}

pub async fn experiment_status(
    repo: Option<PathBuf>,
    experiment: PolicyExperimentId,
    status: PolicyExperimentStatus,
) -> Result<()> {
    let (_, store) = context(repo).await?;
    let concluded = (status != PolicyExperimentStatus::Running).then(Utc::now);
    store
        .set_policy_experiment_status(&experiment, status, concluded)
        .await?;
    println!("Policy experiment {experiment} is now {status}.");
    Ok(())
}

pub async fn promote(
    repo: Option<PathBuf>,
    proposal: PolicyProposalId,
    actor: String,
) -> Result<()> {
    let (config, store) = context(repo).await?;
    ensure_bootstrap_policy(&store, &config).await?;
    let gate = promote_proposal(
        &store,
        &config.repository.name,
        &proposal,
        &PolicyBounds::for_config(&config),
        &actor,
    )
    .await?;
    println!(
        "Promoted proposal {proposal}; approval {} was supplied explicitly by {actor}.",
        gate.approval
    );
    Ok(())
}

pub async fn rollback(
    repo: Option<PathBuf>,
    policy: PolicyId,
    reason: String,
    actor: String,
) -> Result<()> {
    let (config, store) = context(repo).await?;
    ensure_bootstrap_policy(&store, &config).await?;
    execute_rollback(&store, &config.repository.name, &policy, &reason, &actor).await?;
    println!("Rolled back to {policy}: {reason}");
    Ok(())
}
