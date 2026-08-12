//! Immutable, commit-bound repository world models.
//!
//! A snapshot is evidence about one exact Git commit. Facts remain typed and
//! carry their own provenance and certainty; a later build creates a new
//! snapshot rather than rewriting history.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component as PathComponent, Path};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ids::{RunId, TaskId, WorldModelFactId, WorldModelSnapshotId};
use crate::result::{MetricName, MetricValue};

pub const WORLD_MODEL_SCHEMA_VERSION: &str = "world-model-v1";

/// A validated repository-relative path used as durable repository identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RepositoryPath(String);

impl RepositoryPath {
    pub fn new(raw: impl Into<String>) -> Result<Self, WorldModelError> {
        let raw = raw.into().replace('\\', "/");
        let path = Path::new(&raw);
        if raw.is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, PathComponent::Normal(_)))
        {
            return Err(WorldModelError::UnsafePath(raw));
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RepositoryPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RepositoryPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLocation {
    pub path: RepositoryPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u64>,
    pub commit: String,
}

impl SourceLocation {
    pub fn new(path: RepositoryPath, commit: impl Into<String>) -> Self {
        Self {
            path,
            symbol: None,
            line_start: None,
            line_end: None,
            commit: commit.into(),
        }
    }

    fn validate(&self, snapshot_commit: &str) -> Result<(), WorldModelError> {
        if self.commit != snapshot_commit {
            return Err(WorldModelError::LocationCommitMismatch {
                path: self.path.to_string(),
                expected: snapshot_commit.into(),
                found: self.commit.clone(),
            });
        }
        if let (Some(start), Some(end)) = (self.line_start, self.line_end)
            && (start == 0 || end < start)
        {
            return Err(WorldModelError::InvalidLineRange {
                path: self.path.to_string(),
                start,
                end,
            });
        }
        Ok(())
    }

    fn normalize_commit(&mut self) {
        self.commit = "<snapshot>".into();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractorIdentity {
    pub name: String,
    pub version: String,
}

impl ExtractorIdentity {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorldModelProvenanceSource {
    SourceCode { location: SourceLocation },
    RepositoryDocument { location: SourceLocation },
    Configuration { location: SourceLocation },
    Test { location: SourceLocation },
    Evaluator { evaluator_id: String },
    HistoricalRun { run_id: RunId },
    CommitHistory { commit: String },
    UserDeclared { location: SourceLocation },
    Imported { reference: String },
    AgentInferred { agent_id: String, reference: String },
}

impl WorldModelProvenanceSource {
    fn validate(&self, snapshot_commit: &str) -> Result<(), WorldModelError> {
        match self {
            Self::SourceCode { location }
            | Self::RepositoryDocument { location }
            | Self::Configuration { location }
            | Self::Test { location }
            | Self::UserDeclared { location } => location.validate(snapshot_commit),
            Self::Evaluator { evaluator_id } if evaluator_id.trim().is_empty() => {
                Err(WorldModelError::EmptyProvenanceReference)
            }
            Self::CommitHistory { commit } if commit.trim().is_empty() => {
                Err(WorldModelError::EmptyProvenanceReference)
            }
            Self::Imported { reference } | Self::AgentInferred { reference, .. }
                if reference.trim().is_empty() =>
            {
                Err(WorldModelError::EmptyProvenanceReference)
            }
            _ => Ok(()),
        }
    }

    fn normalize_commit(&mut self) {
        match self {
            Self::SourceCode { location }
            | Self::RepositoryDocument { location }
            | Self::Configuration { location }
            | Self::Test { location }
            | Self::UserDeclared { location } => location.normalize_commit(),
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldModelProvenance {
    pub extractor: ExtractorIdentity,
    pub source: WorldModelProvenanceSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceConfidence {
    Declared,
    Observed,
    Inferred,
    Unknown,
}

impl EvidenceConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Observed => "observed",
            Self::Inferred => "inferred",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactMetadata {
    pub id: WorldModelFactId,
    pub snapshot_id: WorldModelSnapshotId,
    pub confidence: EvidenceConfidence,
    pub provenance: Vec<WorldModelProvenance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contradicts: Vec<WorldModelFactId>,
}

impl FactMetadata {
    pub fn new(
        id: WorldModelFactId,
        snapshot_id: WorldModelSnapshotId,
        confidence: EvidenceConfidence,
        provenance: WorldModelProvenance,
    ) -> Self {
        Self {
            id,
            snapshot_id,
            confidence,
            provenance: vec![provenance],
            contradicts: Vec::new(),
        }
    }

    fn normalize_snapshot(&mut self) {
        self.snapshot_id = WorldModelSnapshotId::new("WM-normalized").expect("static id");
        for provenance in &mut self.provenance {
            provenance.source.normalize_commit();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldEntityKind {
    Component,
    Module,
    Interface,
    Contract,
    Invariant,
    Dependency,
    Ownership,
    PerformanceConstraint,
    HistoricalDecision,
    KnownFailureMode,
}

impl WorldEntityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Component => "component",
            Self::Module => "module",
            Self::Interface => "interface",
            Self::Contract => "contract",
            Self::Invariant => "invariant",
            Self::Dependency => "dependency",
            Self::Ownership => "ownership",
            Self::PerformanceConstraint => "performance_constraint",
            Self::HistoricalDecision => "historical_decision",
            Self::KnownFailureMode => "known_failure_mode",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldEntityRef {
    pub kind: WorldEntityKind,
    pub id: WorldModelFactId,
}

impl WorldEntityRef {
    pub fn new(kind: WorldEntityKind, id: WorldModelFactId) -> Self {
        Self { kind, id }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub metadata: FactMetadata,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub paths: Vec<RepositoryPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<WorldModelFactId>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub related_tasks: Vec<TaskId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Module {
    pub metadata: FactMetadata,
    pub name: String,
    pub path: RepositoryPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<WorldModelFactId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceKind {
    LibraryApi,
    Trait,
    HttpApi,
    Rpc,
    Storage,
    MessageSchema,
    Cli,
    Database,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceVisibility {
    Public,
    Internal,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interface {
    pub metadata: FactMetadata,
    pub name: String,
    pub interface_kind: InterfaceKind,
    pub owner: WorldEntityRef,
    pub location: SourceLocation,
    pub visibility: InterfaceVisibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractStrength {
    Explicit,
    Inferred,
    Historical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub metadata: FactMetadata,
    pub subject: WorldEntityRef,
    pub statement: String,
    pub strength: ContractStrength,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_location: Option<SourceLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invariant {
    pub metadata: FactMetadata,
    pub subject: WorldEntityRef,
    pub statement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement: Option<String>,
    #[serde(default)]
    pub related_evaluators: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Imports,
    Calls,
    Implements,
    Reads,
    Writes,
    PublishesTo,
    SubscribesTo,
    DependsOn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub metadata: FactMetadata,
    pub source: WorldEntityRef,
    pub target: WorldEntityRef,
    pub dependency_kind: DependencyKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipRecord {
    pub metadata: FactMetadata,
    pub subject: WorldEntityRef,
    pub owner: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintComparison {
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
    Equal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceConstraint {
    pub metadata: FactMetadata,
    pub subject: WorldEntityRef,
    pub metric: MetricName,
    pub comparison: ConstraintComparison,
    pub threshold: MetricValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub statement: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Proposed,
    Accepted,
    Superseded,
    Deprecated,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalDecision {
    pub metadata: FactMetadata,
    pub title: String,
    pub statement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(default)]
    pub affected: Vec<WorldEntityRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_commit: Option<String>,
    pub status: DecisionStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureModeStatus {
    Open,
    Mitigated,
    Resolved,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnownFailureMode {
    pub metadata: FactMetadata,
    #[serde(default)]
    pub components: Vec<WorldModelFactId>,
    pub description: String,
    #[serde(default)]
    pub symptoms: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_trigger: Option<String>,
    #[serde(default)]
    pub related_runs: Vec<RunId>,
    #[serde(default)]
    pub related_evaluators: Vec<String>,
    #[serde(default)]
    pub related_commits: Vec<String>,
    pub status: FailureModeStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldModelFacts {
    #[serde(default)]
    pub components: Vec<Component>,
    #[serde(default)]
    pub modules: Vec<Module>,
    #[serde(default)]
    pub interfaces: Vec<Interface>,
    #[serde(default)]
    pub contracts: Vec<Contract>,
    #[serde(default)]
    pub invariants: Vec<Invariant>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub ownership: Vec<OwnershipRecord>,
    #[serde(default)]
    pub performance_constraints: Vec<PerformanceConstraint>,
    #[serde(default)]
    pub historical_decisions: Vec<HistoricalDecision>,
    #[serde(default)]
    pub known_failure_modes: Vec<KnownFailureMode>,
}

impl WorldModelFacts {
    pub fn extend(&mut self, other: Self) {
        self.components.extend(other.components);
        self.modules.extend(other.modules);
        self.interfaces.extend(other.interfaces);
        self.contracts.extend(other.contracts);
        self.invariants.extend(other.invariants);
        self.dependencies.extend(other.dependencies);
        self.ownership.extend(other.ownership);
        self.performance_constraints
            .extend(other.performance_constraints);
        self.historical_decisions.extend(other.historical_decisions);
        self.known_failure_modes.extend(other.known_failure_modes);
    }

    pub fn canonicalize(&mut self) {
        macro_rules! sort_facts {
            ($field:ident) => {
                self.$field
                    .sort_by(|left, right| left.metadata.id.cmp(&right.metadata.id));
            };
        }
        sort_facts!(components);
        sort_facts!(modules);
        sort_facts!(interfaces);
        sort_facts!(contracts);
        sort_facts!(invariants);
        sort_facts!(dependencies);
        sort_facts!(ownership);
        sort_facts!(performance_constraints);
        sort_facts!(historical_decisions);
        sort_facts!(known_failure_modes);
    }

    pub fn records(&self) -> Vec<WorldFactRecord> {
        let mut records = Vec::new();
        records.extend(
            self.components
                .iter()
                .cloned()
                .map(WorldFactRecord::Component),
        );
        records.extend(self.modules.iter().cloned().map(WorldFactRecord::Module));
        records.extend(
            self.interfaces
                .iter()
                .cloned()
                .map(WorldFactRecord::Interface),
        );
        records.extend(
            self.contracts
                .iter()
                .cloned()
                .map(WorldFactRecord::Contract),
        );
        records.extend(
            self.invariants
                .iter()
                .cloned()
                .map(WorldFactRecord::Invariant),
        );
        records.extend(
            self.dependencies
                .iter()
                .cloned()
                .map(WorldFactRecord::Dependency),
        );
        records.extend(
            self.ownership
                .iter()
                .cloned()
                .map(WorldFactRecord::Ownership),
        );
        records.extend(
            self.performance_constraints
                .iter()
                .cloned()
                .map(WorldFactRecord::PerformanceConstraint),
        );
        records.extend(
            self.historical_decisions
                .iter()
                .cloned()
                .map(WorldFactRecord::HistoricalDecision),
        );
        records.extend(
            self.known_failure_modes
                .iter()
                .cloned()
                .map(WorldFactRecord::KnownFailureMode),
        );
        records.sort_by(|left, right| left.id().cmp(right.id()));
        records
    }

    pub fn summary(&self) -> WorldModelSummary {
        WorldModelSummary {
            components: self.components.len() as u64,
            modules: self.modules.len() as u64,
            interfaces: self.interfaces.len() as u64,
            contracts: self.contracts.len() as u64,
            invariants: self.invariants.len() as u64,
            dependencies: self.dependencies.len() as u64,
            ownership: self.ownership.len() as u64,
            performance_constraints: self.performance_constraints.len() as u64,
            historical_decisions: self.historical_decisions.len() as u64,
            known_failure_modes: self.known_failure_modes.len() as u64,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "fact", rename_all = "snake_case")]
pub enum WorldFactRecord {
    Component(Component),
    Module(Module),
    Interface(Interface),
    Contract(Contract),
    Invariant(Invariant),
    Dependency(Dependency),
    Ownership(OwnershipRecord),
    PerformanceConstraint(PerformanceConstraint),
    HistoricalDecision(HistoricalDecision),
    KnownFailureMode(KnownFailureMode),
}

impl WorldFactRecord {
    pub fn kind(&self) -> WorldEntityKind {
        match self {
            Self::Component(_) => WorldEntityKind::Component,
            Self::Module(_) => WorldEntityKind::Module,
            Self::Interface(_) => WorldEntityKind::Interface,
            Self::Contract(_) => WorldEntityKind::Contract,
            Self::Invariant(_) => WorldEntityKind::Invariant,
            Self::Dependency(_) => WorldEntityKind::Dependency,
            Self::Ownership(_) => WorldEntityKind::Ownership,
            Self::PerformanceConstraint(_) => WorldEntityKind::PerformanceConstraint,
            Self::HistoricalDecision(_) => WorldEntityKind::HistoricalDecision,
            Self::KnownFailureMode(_) => WorldEntityKind::KnownFailureMode,
        }
    }

    pub fn metadata(&self) -> &FactMetadata {
        match self {
            Self::Component(fact) => &fact.metadata,
            Self::Module(fact) => &fact.metadata,
            Self::Interface(fact) => &fact.metadata,
            Self::Contract(fact) => &fact.metadata,
            Self::Invariant(fact) => &fact.metadata,
            Self::Dependency(fact) => &fact.metadata,
            Self::Ownership(fact) => &fact.metadata,
            Self::PerformanceConstraint(fact) => &fact.metadata,
            Self::HistoricalDecision(fact) => &fact.metadata,
            Self::KnownFailureMode(fact) => &fact.metadata,
        }
    }

    fn metadata_mut(&mut self) -> &mut FactMetadata {
        match self {
            Self::Component(fact) => &mut fact.metadata,
            Self::Module(fact) => &mut fact.metadata,
            Self::Interface(fact) => &mut fact.metadata,
            Self::Contract(fact) => &mut fact.metadata,
            Self::Invariant(fact) => &mut fact.metadata,
            Self::Dependency(fact) => &mut fact.metadata,
            Self::Ownership(fact) => &mut fact.metadata,
            Self::PerformanceConstraint(fact) => &mut fact.metadata,
            Self::HistoricalDecision(fact) => &mut fact.metadata,
            Self::KnownFailureMode(fact) => &mut fact.metadata,
        }
    }

    pub fn id(&self) -> &WorldModelFactId {
        &self.metadata().id
    }

    pub fn display_name(&self) -> String {
        match self {
            Self::Component(fact) => fact.name.clone(),
            Self::Module(fact) => fact.name.clone(),
            Self::Interface(fact) => fact.name.clone(),
            Self::Contract(fact) => fact.statement.clone(),
            Self::Invariant(fact) => fact.statement.clone(),
            Self::Dependency(fact) => format!("{} -> {}", fact.source.id, fact.target.id),
            Self::Ownership(fact) => format!("{} owned by {}", fact.subject.id, fact.owner),
            Self::PerformanceConstraint(fact) => fact.statement.clone(),
            Self::HistoricalDecision(fact) => fact.title.clone(),
            Self::KnownFailureMode(fact) => fact.description.clone(),
        }
    }

    pub fn search_text(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_default()
            .to_ascii_lowercase()
    }

    fn referenced_entities(&self) -> Vec<&WorldEntityRef> {
        match self {
            Self::Interface(fact) => vec![&fact.owner],
            Self::Contract(fact) => vec![&fact.subject],
            Self::Invariant(fact) => vec![&fact.subject],
            Self::Dependency(fact) => vec![&fact.source, &fact.target],
            Self::Ownership(fact) => vec![&fact.subject],
            Self::PerformanceConstraint(fact) => vec![&fact.subject],
            Self::HistoricalDecision(fact) => fact.affected.iter().collect(),
            _ => Vec::new(),
        }
    }

    fn direct_ids(&self) -> Vec<(WorldModelFactId, WorldEntityKind)> {
        match self {
            Self::Component(fact) => fact
                .parent
                .iter()
                .cloned()
                .map(|id| (id, WorldEntityKind::Component))
                .collect(),
            Self::Module(fact) => fact
                .component
                .iter()
                .cloned()
                .map(|id| (id, WorldEntityKind::Component))
                .collect(),
            Self::KnownFailureMode(fact) => fact
                .components
                .iter()
                .cloned()
                .map(|id| (id, WorldEntityKind::Component))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn source_locations(&self) -> Vec<&SourceLocation> {
        match self {
            Self::Interface(fact) => vec![&fact.location],
            Self::Contract(fact) => fact.source_location.iter().collect(),
            _ => Vec::new(),
        }
    }

    fn semantic_normalized(mut self) -> Self {
        self.metadata_mut().normalize_snapshot();
        match &mut self {
            Self::Interface(fact) => fact.location.normalize_commit(),
            Self::Contract(fact) => {
                if let Some(location) = &mut fact.source_location {
                    location.normalize_commit();
                }
            }
            _ => {}
        }
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldModelSummary {
    pub components: u64,
    pub modules: u64,
    pub interfaces: u64,
    pub contracts: u64,
    pub invariants: u64,
    pub dependencies: u64,
    pub ownership: u64,
    pub performance_constraints: u64,
    pub historical_decisions: u64,
    pub known_failure_modes: u64,
}

impl WorldModelSummary {
    pub fn total(&self) -> u64 {
        self.components
            + self.modules
            + self.interfaces
            + self.contracts
            + self.invariants
            + self.dependencies
            + self.ownership
            + self.performance_constraints
            + self.historical_decisions
            + self.known_failure_modes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldModelSnapshotStatus {
    Complete,
    Partial,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldModelSnapshotSource {
    Deterministic,
    Imported,
    UserDeclared,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractorStatus {
    Completed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractorRecord {
    pub identity: ExtractorIdentity,
    pub required: bool,
    pub status: ExtractorStatus,
    pub facts_produced: u64,
    pub configuration_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldModelSnapshot {
    pub snapshot_id: WorldModelSnapshotId,
    pub repository: String,
    pub commit: String,
    pub created_at: DateTime<Utc>,
    pub source: WorldModelSnapshotSource,
    pub schema_version: String,
    pub status: WorldModelSnapshotStatus,
    pub extractors: Vec<ExtractorRecord>,
    pub facts: WorldModelFacts,
}

impl WorldModelSnapshot {
    pub fn validate(&self) -> Result<(), WorldModelError> {
        if self.repository.trim().is_empty() {
            return Err(WorldModelError::EmptyRepository);
        }
        if self.commit.len() != 40 || !self.commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(WorldModelError::InvalidCommit(self.commit.clone()));
        }
        if self.schema_version != WORLD_MODEL_SCHEMA_VERSION {
            return Err(WorldModelError::UnsupportedSchema(
                self.schema_version.clone(),
            ));
        }
        let mut extractor_names = BTreeSet::new();
        for extractor in &self.extractors {
            if extractor.identity.name.trim().is_empty()
                || extractor.identity.version.trim().is_empty()
                || !extractor_names.insert(extractor.identity.name.clone())
            {
                return Err(WorldModelError::InvalidExtractor(
                    extractor.identity.name.clone(),
                ));
            }
        }
        let required_failed = self
            .extractors
            .iter()
            .any(|extractor| extractor.required && extractor.status == ExtractorStatus::Failed);
        let optional_failed = self
            .extractors
            .iter()
            .any(|extractor| !extractor.required && extractor.status == ExtractorStatus::Failed);
        match self.status {
            WorldModelSnapshotStatus::Complete if required_failed || optional_failed => {
                return Err(WorldModelError::StatusContradictsExtractors);
            }
            WorldModelSnapshotStatus::Partial if required_failed || !optional_failed => {
                return Err(WorldModelError::StatusContradictsExtractors);
            }
            WorldModelSnapshotStatus::Failed if !required_failed => {
                return Err(WorldModelError::StatusContradictsExtractors);
            }
            _ => {}
        }

        let records = self.facts.records();
        let mut identities = BTreeMap::new();
        for record in &records {
            let metadata = record.metadata();
            if metadata.snapshot_id != self.snapshot_id {
                return Err(WorldModelError::SnapshotMismatch {
                    fact: metadata.id.clone(),
                    expected: self.snapshot_id.clone(),
                    found: metadata.snapshot_id.clone(),
                });
            }
            if metadata.provenance.is_empty() {
                return Err(WorldModelError::MissingProvenance(metadata.id.clone()));
            }
            for provenance in &metadata.provenance {
                if provenance.extractor.name.trim().is_empty()
                    || provenance.extractor.version.trim().is_empty()
                {
                    return Err(WorldModelError::MissingProvenance(metadata.id.clone()));
                }
                provenance.source.validate(&self.commit)?;
            }
            if identities
                .insert(metadata.id.clone(), record.kind())
                .is_some()
            {
                return Err(WorldModelError::DuplicateFact(metadata.id.clone()));
            }
            if record.display_name().trim().is_empty() {
                return Err(WorldModelError::EmptyFact(metadata.id.clone()));
            }
            for location in record.source_locations() {
                location.validate(&self.commit)?;
            }
        }
        for record in &records {
            for reference in record.referenced_entities() {
                validate_reference(&identities, reference)?;
            }
            for (id, kind) in record.direct_ids() {
                validate_reference(&identities, &WorldEntityRef { kind, id })?;
            }
            for contradiction in &record.metadata().contradicts {
                if !identities.contains_key(contradiction) {
                    return Err(WorldModelError::MissingReference(contradiction.clone()));
                }
            }
        }
        Ok(())
    }

    pub fn summary(&self) -> WorldModelSummary {
        self.facts.summary()
    }

    pub fn diff(&self, newer: &Self) -> WorldModelDiff {
        let old = self
            .facts
            .records()
            .into_iter()
            .map(|record| (record.id().clone(), record))
            .collect::<BTreeMap<_, _>>();
        let new = newer
            .facts
            .records()
            .into_iter()
            .map(|record| (record.id().clone(), record))
            .collect::<BTreeMap<_, _>>();
        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut changed = Vec::new();
        for (id, record) in &new {
            match old.get(id) {
                None => added.push(WorldFactChange::from(record)),
                Some(previous)
                    if previous.clone().semantic_normalized()
                        != record.clone().semantic_normalized() =>
                {
                    changed.push(WorldFactChange::from(record));
                }
                _ => {}
            }
        }
        for (id, record) in &old {
            if !new.contains_key(id) {
                removed.push(WorldFactChange::from(record));
            }
        }
        WorldModelDiff {
            from_snapshot_id: self.snapshot_id.clone(),
            to_snapshot_id: newer.snapshot_id.clone(),
            added,
            removed,
            changed,
            unresolved_identity_changes: Vec::new(),
        }
    }
}

fn validate_reference(
    identities: &BTreeMap<WorldModelFactId, WorldEntityKind>,
    reference: &WorldEntityRef,
) -> Result<(), WorldModelError> {
    let Some(actual) = identities.get(&reference.id) else {
        return Err(WorldModelError::MissingReference(reference.id.clone()));
    };
    if *actual != reference.kind {
        return Err(WorldModelError::ReferenceKindMismatch {
            id: reference.id.clone(),
            expected: reference.kind,
            found: *actual,
        });
    }
    Ok(())
}

impl WorldModelFactId {
    pub fn stable(kind: WorldEntityKind, logical_key: &str) -> Self {
        let mut digest = Sha256::new();
        digest.update(kind.as_str().as_bytes());
        digest.update([0]);
        digest.update(logical_key.as_bytes());
        let hash = format!("{:x}", digest.finalize());
        Self::new(format!("WF-{}", &hash[..24])).expect("hash-derived id is safe")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotRelation {
    Exact,
    Ancestor,
    Stale,
    UnknownRelation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldFactChange {
    pub id: WorldModelFactId,
    pub kind: WorldEntityKind,
    pub display_name: String,
}

impl From<&WorldFactRecord> for WorldFactChange {
    fn from(record: &WorldFactRecord) -> Self {
        Self {
            id: record.id().clone(),
            kind: record.kind(),
            display_name: record.display_name(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldModelDiff {
    pub from_snapshot_id: WorldModelSnapshotId,
    pub to_snapshot_id: WorldModelSnapshotId,
    pub added: Vec<WorldFactChange>,
    pub removed: Vec<WorldFactChange>,
    pub changed: Vec<WorldFactChange>,
    pub unresolved_identity_changes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldQueryKind {
    All,
    Component,
    Module,
    Interface,
    Contract,
    Invariant,
    Dependency,
    Ownership,
    PerformanceConstraint,
    HistoricalDecision,
    KnownFailureMode,
}

impl WorldQueryKind {
    pub fn matches(self, kind: WorldEntityKind) -> bool {
        self == Self::All
            || matches!(
                (self, kind),
                (Self::Component, WorldEntityKind::Component)
                    | (Self::Module, WorldEntityKind::Module)
                    | (Self::Interface, WorldEntityKind::Interface)
                    | (Self::Contract, WorldEntityKind::Contract)
                    | (Self::Invariant, WorldEntityKind::Invariant)
                    | (Self::Dependency, WorldEntityKind::Dependency)
                    | (Self::Ownership, WorldEntityKind::Ownership)
                    | (
                        Self::PerformanceConstraint,
                        WorldEntityKind::PerformanceConstraint
                    )
                    | (
                        Self::HistoricalDecision,
                        WorldEntityKind::HistoricalDecision
                    )
                    | (Self::KnownFailureMode, WorldEntityKind::KnownFailureMode)
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldContextFact {
    pub id: WorldModelFactId,
    pub kind: WorldEntityKind,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldModelContext {
    pub snapshot_id: WorldModelSnapshotId,
    pub commit: String,
    pub relation: SnapshotRelation,
    pub facts: Vec<WorldContextFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldModelContextReference {
    pub snapshot_id: WorldModelSnapshotId,
    pub fact_ids: Vec<WorldModelFactId>,
}

impl From<&WorldModelContext> for WorldModelContextReference {
    fn from(context: &WorldModelContext) -> Self {
        Self {
            snapshot_id: context.snapshot_id.clone(),
            fact_ids: context.facts.iter().map(|fact| fact.id.clone()).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldModelEvent {
    pub snapshot_id: WorldModelSnapshotId,
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub payload: WorldModelEventPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorldModelEventPayload {
    WorldModelBuildStarted {
        repository: String,
        commit: String,
    },
    ExtractorStarted {
        extractor: ExtractorIdentity,
    },
    ExtractorCompleted {
        extractor: ExtractorIdentity,
        fact_count: u64,
    },
    ExtractorFailed {
        extractor: ExtractorIdentity,
        required: bool,
        error: String,
    },
    WorldModelValidated {
        fact_count: u64,
    },
    WorldModelSnapshotCreated {
        status: WorldModelSnapshotStatus,
    },
    WorldModelBuildFailed {
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorldModelError {
    #[error("repository-relative path `{0}` is unsafe")]
    UnsafePath(String),
    #[error("world-model repository must not be empty")]
    EmptyRepository,
    #[error("`{0}` is not a full Git commit hash")]
    InvalidCommit(String),
    #[error("unsupported world-model schema `{0}`")]
    UnsupportedSchema(String),
    #[error("extractor identity `{0}` is empty or duplicated")]
    InvalidExtractor(String),
    #[error("snapshot status contradicts extractor results")]
    StatusContradictsExtractors,
    #[error("fact `{fact}` belongs to `{found}`, expected `{expected}`")]
    SnapshotMismatch {
        fact: WorldModelFactId,
        expected: WorldModelSnapshotId,
        found: WorldModelSnapshotId,
    },
    #[error("fact `{0}` has no provenance")]
    MissingProvenance(WorldModelFactId),
    #[error("provenance reference must not be empty")]
    EmptyProvenanceReference,
    #[error("world-model fact `{0}` is duplicated")]
    DuplicateFact(WorldModelFactId),
    #[error("world-model fact `{0}` has no description or name")]
    EmptyFact(WorldModelFactId),
    #[error("world-model reference `{0}` does not exist in this snapshot")]
    MissingReference(WorldModelFactId),
    #[error("world-model reference `{id}` expects {expected:?}, found {found:?}")]
    ReferenceKindMismatch {
        id: WorldModelFactId,
        expected: WorldEntityKind,
        found: WorldEntityKind,
    },
    #[error("source `{path}` describes commit `{found}`, expected `{expected}`")]
    LocationCommitMismatch {
        path: String,
        expected: String,
        found: String,
    },
    #[error("source `{path}` has invalid line range {start}..{end}")]
    InvalidLineRange { path: String, start: u64, end: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extractor() -> ExtractorIdentity {
        ExtractorIdentity::new("test", "1")
    }

    fn metadata(
        snapshot_id: &WorldModelSnapshotId,
        kind: WorldEntityKind,
        key: &str,
        commit: &str,
    ) -> FactMetadata {
        FactMetadata::new(
            WorldModelFactId::stable(kind, key),
            snapshot_id.clone(),
            EvidenceConfidence::Observed,
            WorldModelProvenance {
                extractor: extractor(),
                source: WorldModelProvenanceSource::SourceCode {
                    location: SourceLocation::new(
                        RepositoryPath::new("src/lib.rs").unwrap(),
                        commit,
                    ),
                },
            },
        )
    }

    fn snapshot(sequence: u64, commit: &str) -> WorldModelSnapshot {
        let snapshot_id = WorldModelSnapshotId::sequential(sequence);
        let component_id = WorldModelFactId::stable(WorldEntityKind::Component, "core");
        WorldModelSnapshot {
            snapshot_id: snapshot_id.clone(),
            repository: "fixture".into(),
            commit: commit.into(),
            created_at: Utc::now(),
            source: WorldModelSnapshotSource::Deterministic,
            schema_version: WORLD_MODEL_SCHEMA_VERSION.into(),
            status: WorldModelSnapshotStatus::Complete,
            extractors: vec![ExtractorRecord {
                identity: extractor(),
                required: true,
                status: ExtractorStatus::Completed,
                facts_produced: 2,
                configuration_fingerprint: "test-v1".into(),
                error: None,
            }],
            facts: WorldModelFacts {
                components: vec![Component {
                    metadata: metadata(&snapshot_id, WorldEntityKind::Component, "core", commit),
                    name: "core".into(),
                    description: "Core component".into(),
                    paths: vec![RepositoryPath::new("src").unwrap()],
                    parent: None,
                    tags: vec!["rust".into()],
                    related_tasks: Vec::new(),
                }],
                modules: vec![Module {
                    metadata: metadata(&snapshot_id, WorldEntityKind::Module, "src/lib.rs", commit),
                    name: "fixture".into(),
                    path: RepositoryPath::new("src/lib.rs").unwrap(),
                    language: Some("rust".into()),
                    component: Some(component_id),
                }],
                ..Default::default()
            },
        }
    }

    #[test]
    fn repository_paths_reject_escape_and_absolute_forms() {
        for path in ["../secret", "/etc/passwd", "a/../../b", "", "."] {
            assert!(RepositoryPath::new(path).is_err(), "accepted {path}");
        }
        assert!(RepositoryPath::new("crates/forge-core/src/lib.rs").is_ok());
    }

    #[test]
    fn snapshot_validation_checks_commit_binding_and_references() {
        let commit = "a".repeat(40);
        let mut snapshot = snapshot(1, &commit);
        snapshot.validate().unwrap();
        snapshot.facts.modules[0].component = Some(WorldModelFactId::stable(
            WorldEntityKind::Component,
            "missing",
        ));
        assert!(matches!(
            snapshot.validate(),
            Err(WorldModelError::MissingReference(_))
        ));
    }

    #[test]
    fn duplicate_fact_ids_are_rejected() {
        let commit = "b".repeat(40);
        let mut snapshot = snapshot(1, &commit);
        snapshot
            .facts
            .components
            .push(snapshot.facts.components[0].clone());
        assert!(matches!(
            snapshot.validate(),
            Err(WorldModelError::DuplicateFact(_))
        ));
    }

    #[test]
    fn stable_ids_survive_metadata_changes() {
        assert_eq!(
            WorldModelFactId::stable(WorldEntityKind::Component, "storage"),
            WorldModelFactId::stable(WorldEntityKind::Component, "storage")
        );
        assert_ne!(
            WorldModelFactId::stable(WorldEntityKind::Component, "storage"),
            WorldModelFactId::stable(WorldEntityKind::Module, "storage")
        );
    }

    #[test]
    fn diff_ignores_snapshot_binding_but_reports_semantic_change() {
        let first_commit = "c".repeat(40);
        let second_commit = "d".repeat(40);
        let first = snapshot(1, &first_commit);
        let mut second = snapshot(2, &second_commit);
        assert!(first.diff(&second).changed.is_empty());
        second.facts.components[0].description = "Changed architecture".into();
        assert_eq!(first.diff(&second).changed.len(), 1);
    }

    #[test]
    fn confidence_distinguishes_declared_observed_and_inferred() {
        assert_ne!(EvidenceConfidence::Declared, EvidenceConfidence::Inferred);
        assert_ne!(EvidenceConfidence::Observed, EvidenceConfidence::Unknown);
    }

    #[test]
    fn provenance_is_mandatory_and_commit_bound() {
        let commit = "e".repeat(40);
        let mut missing = snapshot(1, &commit);
        missing.facts.components[0].metadata.provenance.clear();
        assert!(matches!(
            missing.validate(),
            Err(WorldModelError::MissingProvenance(_))
        ));

        let mut mismatched = snapshot(2, &commit);
        let WorldModelProvenanceSource::SourceCode { location } =
            &mut mismatched.facts.components[0].metadata.provenance[0].source
        else {
            unreachable!()
        };
        location.commit = "f".repeat(40);
        assert!(matches!(
            mismatched.validate(),
            Err(WorldModelError::LocationCommitMismatch { .. })
        ));
    }

    #[test]
    fn contradictory_evidence_is_preserved_without_silent_resolution() {
        let commit = "1".repeat(40);
        let mut snapshot = snapshot(1, &commit);
        let component = snapshot.facts.components[0].metadata.id.clone();
        let module = snapshot.facts.modules[0].metadata.id.clone();
        snapshot.facts.components[0].metadata.contradicts = vec![module.clone()];
        snapshot.facts.modules[0].metadata.contradicts = vec![component];
        snapshot.validate().unwrap();
        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded: WorldModelSnapshot = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            decoded.facts.components[0].metadata.contradicts,
            vec![module]
        );
    }
}
