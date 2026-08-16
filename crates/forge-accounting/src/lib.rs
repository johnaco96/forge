//! Offline, provenance-preserving accounting for Forge agent evidence.
//!
//! This crate deliberately does not participate in `forge run`. During the
//! frozen Tier 1 campaign it reads copies of already-recorded exports and raw
//! harness logs, then emits a separate additive artifact. It never opens a
//! Forge ledger and cannot change an outcome, evaluation, prompt, or agent
//! invocation.

#![deny(rust_2018_idioms)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Schema for the separate accounting-enrichment artifact.
pub const ENRICHMENT_SCHEMA_VERSION: u32 = 1;
/// The observed Codex JSONL contract parsed by this release.
pub const CODEX_USAGE_SOURCE_VERSION: &str = "codex-exec-jsonl-observed-0.147.0";
/// The observed local Codex rollout contract used as supplemental evidence.
pub const CODEX_SESSION_SOURCE_VERSION: &str = "codex-rollout-jsonl-observed-0.147.0";
/// Pinned official ChatGPT/Codex credit rate card.
pub const CODEX_RATE_CARD_ID: &str = "openai-chatgpt-codex-credits-2026-08-15";
/// Official source used to transcribe the checked-in rate card.
pub const CODEX_RATE_CARD_SOURCE: &str = "https://learn.chatgpt.com/docs/pricing";
/// Date the official source was retrieved and checked.
pub const CODEX_RATE_CARD_ACCESSED_ON: &str = "2026-08-15";

#[derive(Debug, Error)]
pub enum AccountingError {
    #[error("could not read `{path}`: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not write `{path}`: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid {kind} JSON in `{path}`: {source}")]
    Json {
        kind: &'static str,
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("the export contains no run record")]
    EmptyExport,
    #[error("offline Codex enrichment requires a Codex export, found agent `{0}`")]
    NotCodex(String),
    #[error("the Codex log contains no structured events")]
    EmptyCodexLog,
    #[error("the Codex session log contains conflicting model identities: {0}")]
    ConflictingModels(String),
    #[error("Codex thread `{expected}` does not match session `{actual}`")]
    SessionMismatch { expected: String, actual: String },
    #[error("the export contains multiple run records; enrich one run at a time")]
    MultipleExportRecords,
}

pub type AccountingResult<T> = Result<T, AccountingError>;

/// Globally unique context for an enrichment record.
///
/// Forge run IDs are only ledger-local. The campaign, task revision, agent,
/// and base commit therefore travel with the run ID in every artifact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EvidenceKey {
    pub campaign_id: String,
    pub task_id: String,
    pub task_revision_id: String,
    pub agent_id: String,
    pub base_commit: String,
    pub run_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelIdentitySource {
    /// Actual model recorded in the Codex provider session log.
    ProviderSessionLog,
    /// Model explicitly present in the immutable Forge run configuration.
    ExplicitForgeConfiguration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelIdentity {
    pub model_id: String,
    pub source: ModelIdentitySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputTokenSemantics {
    /// `input_tokens` is the total input volume and cached input is a subset.
    IncludesCachedInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSource {
    pub format: String,
    pub format_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_version: Option<String>,
}

/// Raw values reported by Codex/OpenAI tooling.
///
/// Absent values stay absent. In particular, no cached-token, credit, or cost
/// field is defaulted to zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_reported_credits: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_reported_cost_usd: Option<f64>,
    pub input_token_semantics: InputTokenSemantics,
    pub source: UsageSource,
    /// Additional provider artifacts used for fields absent from exec stdout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supplemental_sources: Vec<UsageSource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum CacheWriteBillingPolicy {
    /// The rate card explicitly excludes cache writes from credit charges.
    NotCharged,
    /// Cache writes are billed at their own token rate.
    CreditsPerMillion { credits_per_million: f64 },
    /// The applicable rate card does not establish how cache writes are billed.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodexCreditRate {
    pub model_id: String,
    pub input_credits_per_million: f64,
    pub cached_input_credits_per_million: f64,
    pub output_credits_per_million: f64,
    /// Versioned billing semantics for raw provider cache-write evidence.
    pub cache_write_billing: CacheWriteBillingPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodexRateCard {
    pub id: String,
    pub source_url: String,
    pub accessed_on: String,
    pub rates: Vec<CodexCreditRate>,
}

impl CodexRateCard {
    /// The smallest checked rate card needed by the current formal campaign.
    pub fn official_2026_08_15() -> Self {
        Self {
            id: CODEX_RATE_CARD_ID.to_string(),
            source_url: CODEX_RATE_CARD_SOURCE.to_string(),
            accessed_on: CODEX_RATE_CARD_ACCESSED_ON.to_string(),
            rates: vec![CodexCreditRate {
                model_id: "gpt-5.6-sol".to_string(),
                input_credits_per_million: 125.0,
                cached_input_credits_per_million: 12.5,
                output_credits_per_million: 750.0,
                cache_write_billing: CacheWriteBillingPolicy::NotCharged,
            }],
        }
    }

    pub fn rate_for(&self, model_id: &str) -> Option<&CodexCreditRate> {
        self.rates.iter().find(|rate| rate.model_id == model_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreditDerivation {
    TokenRateCard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreditEquivalentBasis {
    /// Versioned conversion identifier, independent of the token rate card.
    pub id: String,
    /// Official source establishing that this conversion applies.
    pub source_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivedCreditAccounting {
    pub derived_credits: f64,
    pub credit_rate_card_id: String,
    pub credit_derivation: CreditDerivation,
    /// A standardized equivalent, not billed cost. It remains absent until an
    /// official, applicable, versioned credit-to-USD conversion is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_equivalent_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_equivalent_basis: Option<CreditEquivalentBasis>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivationBlocker {
    ModelUnknown,
    ModelRateUnavailable,
    InputTokensUnknown,
    CachedInputTokensUnknown,
    CacheWriteInputTokensUnknown,
    CacheWriteRateUnavailable,
    OutputTokensUnknown,
    CachedInputExceedsInput,
    TotalTokenMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceArtifact {
    pub role: String,
    pub path: PathBuf,
    pub sha256: String,
}

/// Separate additive output. No Forge run record is rewritten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountingEnrichmentRecord {
    pub schema_version: u32,
    pub evidence_key: EvidenceKey,
    pub original_export_schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_outcome: Option<String>,
    pub provider_usage: ProviderUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived: Option<DerivedCreditAccounting>,
    #[serde(default)]
    pub derivation_blockers: Vec<DerivationBlocker>,
    pub source_artifacts: Vec<SourceArtifact>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingCoverage {
    pub runs: u64,
    pub model_known: u64,
    pub input_output_tokens_known: u64,
    pub cached_input_known: u64,
    pub provider_credits_known: u64,
    pub derived_credits_known: u64,
    pub credit_equivalent_usd_known: u64,
    pub provider_reported_cost_usd_known: u64,
}

impl AccountingCoverage {
    pub fn from_records(records: &[AccountingEnrichmentRecord]) -> Self {
        let mut coverage = Self::default();
        for record in records {
            coverage.runs += 1;
            let usage = &record.provider_usage;
            coverage.model_known += u64::from(usage.model.is_some());
            coverage.input_output_tokens_known +=
                u64::from(usage.input_tokens.is_some() && usage.output_tokens.is_some());
            coverage.cached_input_known += u64::from(usage.cached_input_tokens.is_some());
            coverage.provider_credits_known += u64::from(usage.provider_reported_credits.is_some());
            coverage.derived_credits_known += u64::from(record.derived.is_some());
            coverage.credit_equivalent_usd_known += u64::from(
                record
                    .derived
                    .as_ref()
                    .and_then(|derived| derived.credit_equivalent_usd)
                    .is_some(),
            );
            coverage.provider_reported_cost_usd_known +=
                u64::from(usage.provider_reported_cost_usd.is_some());
        }
        coverage
    }
}

#[derive(Debug, Deserialize)]
struct CampaignEnvironment {
    campaign_id: String,
    #[serde(default)]
    codex_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExportIdentity {
    schema_version: u32,
    run_id: String,
    task_revision_id: String,
    task: ExportTask,
    base_commit: String,
    agent: ExportAgent,
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    known_cost_usd: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ExportTask {
    task_id: String,
}

#[derive(Debug, Deserialize)]
struct ExportAgent {
    agent_id: String,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct WireUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    cached_input_tokens: Option<u64>,
    #[serde(default)]
    cache_write_input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    reasoning_output_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
    #[serde(default, alias = "credits")]
    provider_reported_credits: Option<f64>,
    #[serde(default, alias = "cost_usd")]
    provider_reported_cost_usd: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct CodexEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    usage: Option<WireUsage>,
    #[serde(default)]
    credits: Option<f64>,
    #[serde(default)]
    cost_usd: Option<f64>,
}

#[derive(Debug)]
struct ParsedCodexLog {
    thread_id: Option<String>,
    usage: WireUsage,
}

/// Parse provider usage from preserved `codex exec --json` stdout.
pub fn parse_codex_exec_jsonl(
    raw: &str,
    cli_version: Option<String>,
) -> AccountingResult<ProviderUsage> {
    let parsed = parse_codex_log(raw)?;
    Ok(provider_usage(parsed, None, cli_version, None))
}

fn parse_codex_log(raw: &str) -> AccountingResult<ParsedCodexLog> {
    let mut events = 0_u64;
    let mut thread_id = None;
    let mut usage = WireUsage::default();
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<CodexEvent>(line) else {
            continue;
        };
        events += 1;
        if event.event_type == "thread.started" {
            thread_id = event.thread_id;
        }
        if event.event_type == "turn.completed"
            && let Some(mut completed) = event.usage
        {
            if completed.provider_reported_credits.is_none() {
                completed.provider_reported_credits = event.credits;
            }
            if completed.provider_reported_cost_usd.is_none() {
                completed.provider_reported_cost_usd = event.cost_usd;
            }
            usage = completed;
        }
    }
    if events == 0 {
        return Err(AccountingError::EmptyCodexLog);
    }
    Ok(ParsedCodexLog { thread_id, usage })
}

fn provider_usage(
    parsed: ParsedCodexLog,
    model: Option<ModelIdentity>,
    cli_version: Option<String>,
    export_cost_usd: Option<f64>,
) -> ProviderUsage {
    ProviderUsage {
        model,
        input_tokens: parsed.usage.input_tokens,
        cached_input_tokens: parsed.usage.cached_input_tokens,
        cache_write_input_tokens: parsed.usage.cache_write_input_tokens,
        output_tokens: parsed.usage.output_tokens,
        reasoning_output_tokens: parsed.usage.reasoning_output_tokens,
        // Do not relabel Forge arithmetic as a provider-reported total. CLI
        // 0.147 stdout omits this field, so it remains absent there.
        total_tokens: parsed.usage.total_tokens,
        provider_reported_credits: parsed.usage.provider_reported_credits,
        provider_reported_cost_usd: parsed.usage.provider_reported_cost_usd.or(export_cost_usd),
        input_token_semantics: InputTokenSemantics::IncludesCachedInput,
        source: UsageSource {
            format: "codex-exec-jsonl".to_string(),
            format_version: CODEX_USAGE_SOURCE_VERSION.to_string(),
            cli_version,
        },
        supplemental_sources: Vec::new(),
    }
}

#[derive(Debug)]
struct ParsedCodexSession {
    model: Option<ModelIdentity>,
    total_tokens: Option<u64>,
}

/// Recover the actual model from a preserved Codex session log.
pub fn parse_codex_session_model(
    raw: &str,
    expected_thread_id: Option<&str>,
) -> AccountingResult<Option<ModelIdentity>> {
    Ok(parse_codex_session(raw, expected_thread_id)?.model)
}

fn parse_codex_session(
    raw: &str,
    expected_thread_id: Option<&str>,
) -> AccountingResult<ParsedCodexSession> {
    let mut session_id = None;
    let mut models = BTreeSet::new();
    let mut total_tokens = None;
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let payload = &value["payload"];
        if value["type"] == "session_meta" {
            session_id = payload["id"].as_str().map(str::to_string);
        }
        if let Some(model) = payload["model"].as_str() {
            models.insert(model.to_string());
        }
        if let Some(total) = payload["info"]["total_token_usage"]["total_tokens"].as_u64() {
            total_tokens = Some(total);
        }
    }
    if let (Some(expected), Some(actual)) = (expected_thread_id, session_id.as_deref())
        && expected != actual
    {
        return Err(AccountingError::SessionMismatch {
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
    if models.len() > 1 {
        return Err(AccountingError::ConflictingModels(
            models.into_iter().collect::<Vec<_>>().join(", "),
        ));
    }
    Ok(ParsedCodexSession {
        model: models.into_iter().next().map(|model_id| ModelIdentity {
            model_id,
            source: ModelIdentitySource::ProviderSessionLog,
        }),
        total_tokens,
    })
}

/// Derive exact credits only when every required raw fact and rate is known.
pub fn derive_codex_credits(
    usage: &ProviderUsage,
    rate_card: &CodexRateCard,
) -> Result<DerivedCreditAccounting, Vec<DerivationBlocker>> {
    let mut blockers = Vec::new();
    let model_id = match usage.model.as_ref() {
        Some(model) => Some(model.model_id.as_str()),
        None => {
            blockers.push(DerivationBlocker::ModelUnknown);
            None
        }
    };
    let rate = model_id.and_then(|model| rate_card.rate_for(model));
    if model_id.is_some() && rate.is_none() {
        blockers.push(DerivationBlocker::ModelRateUnavailable);
    }
    if usage.input_tokens.is_none() {
        blockers.push(DerivationBlocker::InputTokensUnknown);
    }
    if usage.cached_input_tokens.is_none() {
        blockers.push(DerivationBlocker::CachedInputTokensUnknown);
    }
    let cache_write_credits = rate.and_then(|rate| match rate.cache_write_billing {
        CacheWriteBillingPolicy::NotCharged => Some(0.0),
        CacheWriteBillingPolicy::CreditsPerMillion {
            credits_per_million,
        } => match usage.cache_write_input_tokens {
            Some(tokens) => Some(tokens as f64 / 1_000_000.0 * credits_per_million),
            None => {
                blockers.push(DerivationBlocker::CacheWriteInputTokensUnknown);
                None
            }
        },
        CacheWriteBillingPolicy::Unknown => match usage.cache_write_input_tokens {
            Some(0) => Some(0.0),
            Some(_) | None => {
                blockers.push(DerivationBlocker::CacheWriteRateUnavailable);
                None
            }
        },
    });
    if usage.output_tokens.is_none() {
        blockers.push(DerivationBlocker::OutputTokensUnknown);
    }
    if let (Some(input), Some(cached)) = (usage.input_tokens, usage.cached_input_tokens)
        && cached > input
    {
        blockers.push(DerivationBlocker::CachedInputExceedsInput);
    }
    if let (Some(input), Some(output), Some(total)) =
        (usage.input_tokens, usage.output_tokens, usage.total_tokens)
        && input.checked_add(output) != Some(total)
    {
        blockers.push(DerivationBlocker::TotalTokenMismatch);
    }
    blockers.sort_unstable();
    blockers.dedup();
    if !blockers.is_empty() {
        return Err(blockers);
    }

    let rate = rate.expect("rate checked above");
    let input = usage.input_tokens.expect("input checked above");
    let cached = usage
        .cached_input_tokens
        .expect("cached input checked above");
    let output = usage.output_tokens.expect("output checked above");
    let cache_write_credits = cache_write_credits.expect("cache-write policy checked above");
    let uncached = input - cached;
    let credits = uncached as f64 / 1_000_000.0 * rate.input_credits_per_million
        + cached as f64 / 1_000_000.0 * rate.cached_input_credits_per_million
        + output as f64 / 1_000_000.0 * rate.output_credits_per_million
        + cache_write_credits;
    // Forge's existing accounting convention uses f64. Normalize beyond the
    // precision of the published per-million rates so serialized historical
    // values do not retain binary floating-point display noise.
    let credits = (credits * 1_000_000_000.0).round() / 1_000_000_000.0;

    Ok(DerivedCreditAccounting {
        derived_credits: credits,
        credit_rate_card_id: rate_card.id.clone(),
        credit_derivation: CreditDerivation::TokenRateCard,
        credit_equivalent_usd: None,
        credit_equivalent_basis: None,
    })
}

/// Build one separate enrichment record from already-preserved evidence.
pub fn enrich_codex_evidence(
    environment_path: &Path,
    export_path: &Path,
    agent_log_path: &Path,
    session_log_path: Option<&Path>,
) -> AccountingResult<AccountingEnrichmentRecord> {
    let environment_raw = read(environment_path)?;
    let export_raw = read(export_path)?;
    let agent_raw = read(agent_log_path)?;
    let session_raw = session_log_path.map(read).transpose()?;

    let environment: CampaignEnvironment =
        serde_json::from_str(&environment_raw).map_err(|source| AccountingError::Json {
            kind: "campaign environment",
            path: environment_path.to_path_buf(),
            source,
        })?;
    let export = parse_single_export(&export_raw, export_path)?;
    if export.agent.agent_id != "codex" {
        return Err(AccountingError::NotCodex(export.agent.agent_id));
    }
    let parsed_log = parse_codex_log(&agent_raw)?;
    let session_evidence = match session_raw.as_deref() {
        Some(raw) => Some(parse_codex_session(raw, parsed_log.thread_id.as_deref())?),
        None => None,
    };
    let provider_model = session_evidence
        .as_ref()
        .and_then(|evidence| evidence.model.clone());
    let model = provider_model.or_else(|| {
        export.agent.model.clone().map(|model_id| ModelIdentity {
            model_id,
            source: ModelIdentitySource::ExplicitForgeConfiguration,
        })
    });
    let mut provider_usage = provider_usage(
        parsed_log,
        model,
        environment.codex_version,
        export.known_cost_usd,
    );
    if let Some(session_evidence) = session_evidence {
        if provider_usage.total_tokens.is_none() {
            provider_usage.total_tokens = session_evidence.total_tokens;
        }
        provider_usage.supplemental_sources.push(UsageSource {
            format: "codex-rollout-jsonl".to_string(),
            format_version: CODEX_SESSION_SOURCE_VERSION.to_string(),
            cli_version: provider_usage.source.cli_version.clone(),
        });
    }
    let (derived, derivation_blockers) =
        match derive_codex_credits(&provider_usage, &CodexRateCard::official_2026_08_15()) {
            Ok(derived) => (Some(derived), Vec::new()),
            Err(blockers) => (None, blockers),
        };

    let mut source_artifacts = vec![
        artifact("campaign_environment", environment_path, &environment_raw),
        artifact("original_export", export_path, &export_raw),
        artifact("codex_exec_stdout", agent_log_path, &agent_raw),
    ];
    if let (Some(path), Some(raw)) = (session_log_path, session_raw.as_deref()) {
        source_artifacts.push(artifact("codex_session_log", path, raw));
    }

    Ok(AccountingEnrichmentRecord {
        schema_version: ENRICHMENT_SCHEMA_VERSION,
        evidence_key: EvidenceKey {
            campaign_id: environment.campaign_id,
            task_id: export.task.task_id,
            task_revision_id: export.task_revision_id,
            agent_id: export.agent.agent_id,
            base_commit: export.base_commit,
            run_id: export.run_id,
        },
        original_export_schema_version: export.schema_version,
        original_outcome: export.outcome,
        provider_usage,
        derived,
        derivation_blockers,
        source_artifacts,
    })
}

pub fn read_enrichment_jsonl(path: &Path) -> AccountingResult<Vec<AccountingEnrichmentRecord>> {
    let raw = read(path)?;
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|source| AccountingError::Json {
                kind: "accounting enrichment",
                path: path.to_path_buf(),
                source,
            })
        })
        .collect()
}

pub fn write_enrichment_jsonl(
    path: &Path,
    record: &AccountingEnrichmentRecord,
) -> AccountingResult<()> {
    let mut serialized = serde_json::to_string(record).expect("serializable enrichment record");
    serialized.push('\n');
    fs::write(path, serialized).map_err(|source| AccountingError::Write {
        path: path.to_path_buf(),
        source,
    })
}

fn parse_single_export(raw: &str, path: &Path) -> AccountingResult<ExportIdentity> {
    let mut records = raw.lines().filter(|line| !line.trim().is_empty());
    let first = records.next().ok_or(AccountingError::EmptyExport)?;
    if records.next().is_some() {
        return Err(AccountingError::MultipleExportRecords);
    }
    serde_json::from_str(first).map_err(|source| AccountingError::Json {
        kind: "Forge export",
        path: path.to_path_buf(),
        source,
    })
}

fn artifact(role: &str, path: &Path, raw: &str) -> SourceArtifact {
    SourceArtifact {
        role: role.to_string(),
        path: path.to_path_buf(),
        sha256: hex_digest(raw.as_bytes()),
    }
}

fn read(path: &Path) -> AccountingResult<String> {
    fs::read_to_string(path).map_err(|source| AccountingError::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const USAGE: &str = r#"{"type":"thread.started","thread_id":"thread-1"}
{"type":"turn.completed","usage":{"input_tokens":1000000,"cached_input_tokens":800000,"cache_write_input_tokens":0,"output_tokens":100000,"reasoning_output_tokens":25000}}"#;

    fn known_usage() -> ProviderUsage {
        let mut usage = parse_codex_exec_jsonl(USAGE, Some("codex-cli 0.147.0".into())).unwrap();
        usage.model = Some(ModelIdentity {
            model_id: "gpt-5.6-sol".into(),
            source: ModelIdentitySource::ProviderSessionLog,
        });
        usage
    }

    #[test]
    fn codex_parser_captures_input_and_output_without_inventing_total() {
        let usage = parse_codex_exec_jsonl(USAGE, Some("codex-cli 0.147.0".into())).unwrap();
        assert_eq!(usage.input_tokens, Some(1_000_000));
        assert_eq!(usage.output_tokens, Some(100_000));
        assert_eq!(usage.total_tokens, None);
        assert_eq!(
            usage.input_token_semantics,
            InputTokenSemantics::IncludesCachedInput
        );
    }

    #[test]
    fn provider_total_is_retained_only_when_the_provider_emits_it() {
        let raw = r#"{"type":"turn.completed","usage":{"input_tokens":7,"output_tokens":3,"total_tokens":10}}"#;
        let usage = parse_codex_exec_jsonl(raw, None).unwrap();
        assert_eq!(usage.total_tokens, Some(10));
    }

    #[test]
    fn cached_and_reasoning_tokens_are_captured() {
        let usage = parse_codex_exec_jsonl(USAGE, None).unwrap();
        assert_eq!(usage.cached_input_tokens, Some(800_000));
        assert_eq!(usage.cache_write_input_tokens, Some(0));
        assert_eq!(usage.reasoning_output_tokens, Some(25_000));
    }

    #[test]
    fn nonzero_cache_write_usage_is_preserved_as_provider_evidence() {
        let raw = r#"{"type":"turn.completed","usage":{"input_tokens":7,"cached_input_tokens":2,"cache_write_input_tokens":123456,"output_tokens":3}}"#;
        let usage = parse_codex_exec_jsonl(raw, None).unwrap();
        assert_eq!(usage.cache_write_input_tokens, Some(123_456));
    }

    #[test]
    fn missing_cached_input_remains_unknown() {
        let raw = r#"{"type":"turn.completed","usage":{"input_tokens":7,"output_tokens":3}}"#;
        let usage = parse_codex_exec_jsonl(raw, None).unwrap();
        assert_eq!(usage.cached_input_tokens, None);
        assert_eq!(usage.cache_write_input_tokens, None);
    }

    #[test]
    fn model_identity_comes_from_matching_provider_session() {
        let raw = r#"{"type":"session_meta","payload":{"id":"thread-1"}}
{"type":"turn_context","payload":{"model":"gpt-5.6-sol"}}"#;
        let model = parse_codex_session_model(raw, Some("thread-1"))
            .unwrap()
            .unwrap();
        assert_eq!(model.model_id, "gpt-5.6-sol");
        assert_eq!(model.source, ModelIdentitySource::ProviderSessionLog);
    }

    #[test]
    fn provider_session_total_is_recovered_without_recalculation() {
        let raw = r#"{"type":"session_meta","payload":{"id":"thread-1"}}
{"type":"event_msg","payload":{"info":{"total_token_usage":{"total_tokens":42}}}}"#;
        let session = parse_codex_session(raw, Some("thread-1")).unwrap();
        assert_eq!(session.total_tokens, Some(42));
    }

    #[test]
    fn mismatched_session_is_rejected() {
        let raw = r#"{"type":"session_meta","payload":{"id":"other"}}
{"type":"turn_context","payload":{"model":"gpt-5.6-sol"}}"#;
        assert!(matches!(
            parse_codex_session_model(raw, Some("thread-1")),
            Err(AccountingError::SessionMismatch { .. })
        ));
    }

    #[test]
    fn unknown_model_prevents_derivation() {
        let mut usage = known_usage();
        usage.model = None;
        assert_eq!(
            derive_codex_credits(&usage, &CodexRateCard::official_2026_08_15()),
            Err(vec![DerivationBlocker::ModelUnknown])
        );
    }

    #[test]
    fn unknown_cached_split_prevents_exact_derivation() {
        let mut usage = known_usage();
        usage.cached_input_tokens = None;
        assert!(
            derive_codex_credits(&usage, &CodexRateCard::official_2026_08_15())
                .unwrap_err()
                .contains(&DerivationBlocker::CachedInputTokensUnknown)
        );
    }

    #[test]
    fn known_fixture_uses_uncached_cached_and_output_without_double_counting() {
        let derived =
            derive_codex_credits(&known_usage(), &CodexRateCard::official_2026_08_15()).unwrap();
        // 0.2M * 125 + 0.8M * 12.5 + 0.1M * 750 = 110 credits.
        assert!((derived.derived_credits - 110.0).abs() < f64::EPSILON);
    }

    #[test]
    fn reasoning_tokens_are_not_added_to_provider_output_twice() {
        let mut a = known_usage();
        let mut b = a.clone();
        a.reasoning_output_tokens = Some(1);
        b.reasoning_output_tokens = Some(99_999);
        let card = CodexRateCard::official_2026_08_15();
        assert_eq!(
            derive_codex_credits(&a, &card),
            derive_codex_credits(&b, &card)
        );
    }

    #[test]
    fn rate_card_version_is_retained_in_the_derived_value() {
        let derived =
            derive_codex_credits(&known_usage(), &CodexRateCard::official_2026_08_15()).unwrap();
        assert_eq!(derived.credit_rate_card_id, CODEX_RATE_CARD_ID);
    }

    #[test]
    fn a_new_rate_card_does_not_mutate_a_serialized_historical_calculation() {
        let original =
            derive_codex_credits(&known_usage(), &CodexRateCard::official_2026_08_15()).unwrap();
        let serialized = serde_json::to_string(&original).unwrap();
        let mut newer = CodexRateCard::official_2026_08_15();
        newer.id = "future-card".into();
        newer.rates[0].input_credits_per_million = 999.0;
        let historical: DerivedCreditAccounting = serde_json::from_str(&serialized).unwrap();
        assert_eq!(historical, original);
        assert_ne!(
            derive_codex_credits(&known_usage(), &newer).unwrap(),
            historical
        );
    }

    #[test]
    fn provider_credits_and_derived_credits_remain_separate() {
        let raw = r#"{"type":"turn.completed","usage":{"input_tokens":1000000,"cached_input_tokens":800000,"cache_write_input_tokens":0,"output_tokens":100000,"credits":109.5}}"#;
        let mut usage = parse_codex_exec_jsonl(raw, None).unwrap();
        usage.model = known_usage().model;
        let derived = derive_codex_credits(&usage, &CodexRateCard::official_2026_08_15()).unwrap();
        assert_eq!(usage.provider_reported_credits, Some(109.5));
        assert_eq!(derived.derived_credits, 110.0);
    }

    #[test]
    fn provider_usd_and_credit_equivalent_usd_remain_separate() {
        let raw = r#"{"type":"turn.completed","usage":{"input_tokens":1000000,"cached_input_tokens":800000,"cache_write_input_tokens":0,"output_tokens":100000,"cost_usd":1.25}}"#;
        let mut usage = parse_codex_exec_jsonl(raw, None).unwrap();
        usage.model = known_usage().model;
        let derived = derive_codex_credits(&usage, &CodexRateCard::official_2026_08_15()).unwrap();
        assert_eq!(usage.provider_reported_cost_usd, Some(1.25));
        assert_eq!(derived.credit_equivalent_usd, None);
        assert_eq!(derived.credit_equivalent_basis, None);
    }

    #[test]
    fn nonzero_cache_writes_are_not_charged_by_the_official_card() {
        let card = CodexRateCard::official_2026_08_15();
        assert_eq!(
            card.rates[0].cache_write_billing,
            CacheWriteBillingPolicy::NotCharged
        );
        let without_writes = known_usage();
        let mut with_writes = without_writes.clone();
        with_writes.cache_write_input_tokens = Some(900_000);
        assert_eq!(
            derive_codex_credits(&without_writes, &card),
            derive_codex_credits(&with_writes, &card)
        );
    }

    #[test]
    fn unknown_future_cache_write_policy_blocks_nonzero_usage() {
        let mut future = CodexRateCard::official_2026_08_15();
        future.id = "future-card-with-unknown-cache-write-policy".into();
        future.rates[0].cache_write_billing = CacheWriteBillingPolicy::Unknown;
        let mut usage = known_usage();
        usage.cache_write_input_tokens = Some(1);
        assert!(
            derive_codex_credits(&usage, &future)
                .unwrap_err()
                .contains(&DerivationBlocker::CacheWriteRateUnavailable)
        );
    }

    #[test]
    fn coverage_reports_missingness_instead_of_zero_filling() {
        let record = AccountingEnrichmentRecord {
            schema_version: 1,
            evidence_key: EvidenceKey {
                campaign_id: "c".into(),
                task_id: "t".into(),
                task_revision_id: "tr".into(),
                agent_id: "codex".into(),
                base_commit: "b".into(),
                run_id: "R-0001".into(),
            },
            original_export_schema_version: 1,
            original_outcome: Some("passed".into()),
            provider_usage: parse_codex_exec_jsonl(
                r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":2}}"#,
                None,
            )
            .unwrap(),
            derived: None,
            derivation_blockers: vec![DerivationBlocker::ModelUnknown],
            source_artifacts: vec![],
        };
        assert_eq!(
            AccountingCoverage::from_records(&[record]),
            AccountingCoverage {
                runs: 1,
                input_output_tokens_known: 1,
                ..AccountingCoverage::default()
            }
        );
    }
}
