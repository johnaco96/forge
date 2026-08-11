//! The unit of work Forge assigns to an agent.
//!
//! A task deliberately carries three separable things: a natural-language
//! objective for the agent, machine-readable constraints, and machine-readable
//! evaluation instructions that Forge — not the agent — executes.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ids::TaskId;
use crate::integrity::ProtectionPolicy;

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("failed to read task file `{path}`")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse task file `{path}`")]
    Yaml {
        path: String,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("failed to parse task file `{path}`")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("task `{task_id}` is invalid: {reason}")]
    Invalid { task_id: String, reason: String },
}

/// A command Forge will execute to evaluate a change.
///
/// Accepts either the shorthand form (`command: cargo test`) or a bare string,
/// so both spellings used in the design document parse:
///
/// ```yaml
/// evaluation:
///   tests: cargo test --workspace          # bare
///   lint:
///     command: cargo clippy               # expanded
///     timeout_secs: 600
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandSpec {
    /// Executed through `sh -c`, so pipelines and `&&` behave as written.
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Directory to run in, relative to the workspace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

impl CommandSpec {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            timeout_secs: None,
            working_dir: None,
        }
    }
}

/// A benchmark command and the structured metrics file it produces.
///
/// The metrics file is repository-relative and read only after the trusted
/// benchmark command completes. Omitting it preserves the V0 command-only
/// behavior while giving future competitive evaluation a stable contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BenchmarkSpec {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_file: Option<String>,
}

impl BenchmarkSpec {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            timeout_secs: None,
            working_dir: None,
            metrics_file: None,
        }
    }

    pub fn with_metrics_file(mut self, path: impl Into<String>) -> Self {
        self.metrics_file = Some(path.into());
        self
    }

    pub fn command_spec(&self) -> CommandSpec {
        CommandSpec {
            command: self.command.clone(),
            timeout_secs: self.timeout_secs,
            working_dir: self.working_dir.clone(),
        }
    }
}

impl From<CommandSpec> for BenchmarkSpec {
    fn from(spec: CommandSpec) -> Self {
        Self {
            command: spec.command,
            timeout_secs: spec.timeout_secs,
            working_dir: spec.working_dir,
            metrics_file: None,
        }
    }
}

impl<'de> Deserialize<'de> for BenchmarkSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(BenchmarkSpecVisitor)
    }
}

struct BenchmarkSpecVisitor;

impl<'de> serde::de::Visitor<'de> for BenchmarkSpecVisitor {
    type Value = BenchmarkSpec;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a benchmark command string, or a table with a `command` key")
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(BenchmarkSpec::new(value))
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Err(E::custom(format!(
            "expected a benchmark command, found the boolean `{value}`; quote it as \"{value}\""
        )))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut command: Option<String> = None;
        let mut timeout_secs: Option<u64> = None;
        let mut working_dir: Option<String> = None;
        let mut metrics_file: Option<String> = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "command" => command = Some(map.next_value()?),
                "timeout_secs" => timeout_secs = Some(map.next_value()?),
                "working_dir" => working_dir = Some(map.next_value()?),
                "metrics_file" => metrics_file = Some(map.next_value()?),
                other => {
                    return Err(serde::de::Error::custom(format!(
                        "unknown key `{other}`; expected `command`, `timeout_secs`, `working_dir`, \
                         or `metrics_file`"
                    )));
                }
            }
        }

        Ok(BenchmarkSpec {
            command: command.ok_or_else(|| serde::de::Error::custom("missing `command`"))?,
            timeout_secs,
            working_dir,
            metrics_file,
        })
    }
}

/// Accepts both spellings, with errors a task author can act on.
///
/// Written by hand rather than derived from an untagged enum: untagged
/// deserialization reports only "data did not match any variant", which is
/// useless when the real problem is that YAML read an unquoted `true` as a
/// boolean. Task files are handwritten, so this error is worth the code.
impl<'de> Deserialize<'de> for CommandSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(CommandSpecVisitor)
    }
}

struct CommandSpecVisitor;

impl<'de> serde::de::Visitor<'de> for CommandSpecVisitor {
    type Value = CommandSpec;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a command string, or a table with a `command` key")
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(CommandSpec::new(value))
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
        // The classic YAML trap: `command: true` is a boolean, not the
        // `true(1)` shell builtin.
        Err(E::custom(format!(
            "expected a command, found the boolean `{value}`; quote it as \"{value}\" if you \
             meant the command of that name"
        )))
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Err(E::custom(format!(
            "expected a command, found the number `{value}`; quote it if it is a command name"
        )))
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Err(E::custom(format!(
            "expected a command, found the number `{value}`; quote it if it is a command name"
        )))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Err(E::custom(format!(
            "expected a command, found the number `{value}`; quote it if it is a command name"
        )))
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Err(E::custom("expected a command, found nothing"))
    }

    fn visit_seq<A>(self, _seq: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        Err(serde::de::Error::custom(
            "expected a command string, found a list; write the command as one string, \
             e.g. `cargo test --workspace`",
        ))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut command: Option<String> = None;
        let mut timeout_secs: Option<u64> = None;
        let mut working_dir: Option<String> = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "command" => command = Some(map.next_value()?),
                "timeout_secs" => timeout_secs = Some(map.next_value()?),
                "working_dir" => working_dir = Some(map.next_value()?),
                other => {
                    return Err(serde::de::Error::custom(format!(
                        "unknown key `{other}`; expected `command`, `timeout_secs`, or \
                         `working_dir`"
                    )));
                }
            }
        }

        Ok(CommandSpec {
            command: command.ok_or_else(|| serde::de::Error::custom("missing `command`"))?,
            timeout_secs,
            working_dir,
        })
    }
}

/// A repository-defined evaluation step beyond the well-known ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedCommand {
    pub name: String,
    #[serde(flatten)]
    pub spec: CommandSpec,
}

/// How Forge independently judges a change.
///
/// Every field is optional; a task with no evaluation commands can still run,
/// but Forge will only be able to report what the agent did, not whether the
/// result is any good.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationSpec {
    #[serde(
        default,
        alias = "test_command",
        skip_serializing_if = "Option::is_none"
    )]
    pub tests: Option<CommandSpec>,
    #[serde(
        default,
        alias = "benchmark_command",
        skip_serializing_if = "Option::is_none"
    )]
    pub benchmark: Option<BenchmarkSpec>,
    #[serde(
        default,
        alias = "lint_command",
        skip_serializing_if = "Option::is_none"
    )]
    pub lint: Option<CommandSpec>,
    #[serde(
        default,
        alias = "build_command",
        skip_serializing_if = "Option::is_none"
    )]
    pub build: Option<CommandSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom: Vec<NamedCommand>,
}

impl EvaluationSpec {
    /// Returns every configured check as `(kind, spec)` pairs, in the order
    /// Forge should run them: build, tests, lint, benchmark, then custom.
    pub fn checks(&self) -> Vec<(String, CommandSpec)> {
        let mut out = Vec::new();
        for (kind, spec) in [
            ("build", self.build.clone()),
            ("tests", self.tests.clone()),
            ("lint", self.lint.clone()),
        ] {
            if let Some(spec) = spec {
                out.push((kind.to_string(), spec));
            }
        }
        if let Some(benchmark) = &self.benchmark {
            out.push(("benchmark".to_string(), benchmark.command_spec()));
        }
        for custom in &self.custom {
            out.push((custom.name.clone(), custom.spec.clone()));
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.checks().is_empty()
    }
}

/// Task attributes used later for routing and cohort analysis.
///
/// These are the features the design document names as routing inputs
/// (language, subsystem, task category); they are free-form strings for now
/// because a controlled vocabulary should be derived from real tasks rather
/// than guessed at.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subsystem: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

/// A structured engineering task assigned to an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringTask {
    pub task_id: TaskId,
    /// Logical repository name, matched against the initialized repository.
    pub repository: String,
    /// What the change should achieve, in natural language.
    pub objective: String,
    /// Machine-readable invariants the change must not violate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub evaluation: EvaluationSpec,
    /// Trusted evaluation inputs and task-scoped exceptions.
    ///
    /// Flattening keeps the task spelling concise:
    /// `protected_paths:` and `allowed_protected_paths:` are top-level keys.
    #[serde(flatten, default)]
    pub protection: ProtectionPolicy,
    #[serde(default)]
    pub metadata: TaskMetadata,
}

impl EngineeringTask {
    /// Loads a task from `.yaml`, `.yml`, or `.json`.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, TaskError> {
        let path = path.as_ref();
        let display = path.display().to_string();
        let raw = fs::read_to_string(path).map_err(|source| TaskError::Io {
            path: display.clone(),
            source,
        })?;

        let task = if path.extension().is_some_and(|ext| ext == "json") {
            serde_json::from_str(&raw).map_err(|source| TaskError::Json {
                path: display.clone(),
                source,
            })?
        } else {
            serde_yaml_ng::from_str(&raw).map_err(|source| TaskError::Yaml {
                path: display.clone(),
                source,
            })?
        };
        Ok(task)
    }

    /// Rejects tasks that cannot produce a meaningful run.
    pub fn validate(&self) -> Result<(), TaskError> {
        let invalid = |reason: &str| TaskError::Invalid {
            task_id: self.task_id.to_string(),
            reason: reason.to_string(),
        };

        if self.objective.trim().is_empty() {
            return Err(invalid("objective must not be empty"));
        }
        if self.repository.trim().is_empty() {
            return Err(invalid("repository must not be empty"));
        }
        if self.constraints.iter().any(|c| c.trim().is_empty()) {
            return Err(invalid("constraints must not contain empty entries"));
        }
        for (kind, spec) in self.evaluation.checks() {
            if spec.command.trim().is_empty() {
                return Err(invalid(&format!(
                    "evaluation `{kind}` has an empty command"
                )));
            }
            if spec.timeout_secs == Some(0) {
                return Err(invalid(&format!(
                    "evaluation `{kind}` has a zero timeout; omit it for no limit"
                )));
            }
            if let Some(working_dir) = &spec.working_dir {
                validate_repository_relative(working_dir).map_err(|reason| {
                    invalid(&format!("evaluation `{kind}` working directory {reason}"))
                })?;
            }
        }
        if let Some(benchmark) = &self.evaluation.benchmark
            && let Some(metrics_file) = &benchmark.metrics_file
        {
            validate_repository_relative(metrics_file)
                .map_err(|reason| invalid(&format!("benchmark metrics file {reason}")))?;
        }
        self.protection
            .validate()
            .map_err(|error| invalid(&error.to_string()))?;
        Ok(())
    }

    /// Non-fatal observations worth surfacing to the author of a task file.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.evaluation.is_empty() {
            warnings.push(
                "no evaluation commands: Forge cannot independently judge this task's result"
                    .to_string(),
            );
        }
        if !self.evaluation.is_empty() && self.protection.is_empty() {
            warnings.push(
                "no protected paths: an agent could modify evaluation inputs without an \
                 integrity violation"
                    .to_string(),
            );
        }
        if self.constraints.is_empty() {
            warnings.push(
                "no constraints: regressions outside the objective will not be caught by the task \
                 definition"
                    .to_string(),
            );
        }
        if self.objective.split_whitespace().count() < 4 {
            warnings.push(
                "objective is very short; agents perform better with a stated outcome and context"
                    .to_string(),
            );
        }
        warnings
    }
}

fn validate_repository_relative(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("must not be empty".to_string());
    }
    let normalized = path.replace('\\', "/");
    if normalized
        .as_bytes()
        .get(1)
        .is_some_and(|byte| *byte == b':')
        || normalized.split('/').any(|segment| segment == "..")
    {
        return Err("must not escape the repository root".to_string());
    }
    let path = Path::new(&normalized);
    if path.is_absolute() {
        return Err("must be relative to the repository root".to_string());
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return Err("must not escape the repository root".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact task JSON from the design document must parse unchanged.
    #[test]
    fn parses_the_design_document_example() {
        let raw = r#"
        {
          "task_id": "T-1042",
          "repository": "distributed-runtime",
          "objective": "Improve checkpoint write throughput",
          "constraints": [
            "All existing tests must pass",
            "Recovery semantics cannot change",
            "Memory increase must remain below 10%"
          ],
          "evaluation": {
            "test_command": "cargo test --workspace",
            "benchmark_command": "./bench/checkpoint.sh"
          }
        }
        "#;

        let task: EngineeringTask = serde_json::from_str(raw).unwrap();
        task.validate().unwrap();
        assert_eq!(task.task_id.as_str(), "T-1042");
        assert_eq!(task.constraints.len(), 3);
        assert_eq!(
            task.evaluation.tests.as_ref().unwrap().command,
            "cargo test --workspace"
        );
        assert_eq!(
            task.evaluation.benchmark.as_ref().unwrap().command,
            "./bench/checkpoint.sh"
        );
    }

    /// The evaluator-configuration YAML from the design document must also parse.
    #[test]
    fn parses_the_expanded_evaluation_form() {
        let raw = r#"
task_id: T-0007
repository: distributed-runtime
objective: Reduce allocator pressure in the storage path
evaluation:
  tests:
    command: cargo test --workspace
  benchmark:
    command: ./bench/checkpoint.sh
  lint:
    command: cargo clippy --workspace -- -D warnings
"#;
        let task: EngineeringTask = serde_yaml_ng::from_str(raw).unwrap();
        task.validate().unwrap();
        let checks = task.evaluation.checks();
        assert_eq!(
            checks.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["tests", "lint", "benchmark"]
        );
    }

    #[test]
    fn expanded_command_carries_a_timeout() {
        let raw = r#"
task_id: T-0008
repository: r
objective: Speed up the slow integration suite
evaluation:
  tests:
    command: cargo test --workspace
    timeout_secs: 900
  custom:
    - name: fuzz
      command: ./scripts/fuzz.sh
"#;
        let task: EngineeringTask = serde_yaml_ng::from_str(raw).unwrap();
        assert_eq!(task.evaluation.tests.unwrap().timeout_secs, Some(900));
        assert_eq!(task.evaluation.custom[0].name, "fuzz");
        assert_eq!(task.evaluation.custom[0].spec.command, "./scripts/fuzz.sh");
    }

    #[test]
    fn unknown_fields_are_rejected_so_typos_surface() {
        let raw = r#"
task_id: T-0009
repository: r
objective: Something
evaluatoin:
  tests: cargo test
"#;
        let err = serde_yaml_ng::from_str::<EngineeringTask>(raw).unwrap_err();
        assert!(err.to_string().contains("evaluatoin"), "{err}");
    }

    /// `true` is a YAML boolean, and also a real command. Both spellings of a
    /// task have to do something sensible with it.
    #[test]
    fn an_unquoted_yaml_boolean_is_handled_in_both_command_forms() {
        // Under a `command:` key the scalar is read as the text it was written
        // as, which is what the author meant.
        let expanded =
            "task_id: T-1\nrepository: r\nobjective: o\nevaluation:\n  tests:\n    command: true\n";
        let task: EngineeringTask = serde_yaml_ng::from_str(expanded).unwrap();
        assert_eq!(task.evaluation.tests.unwrap().command, "true");

        // In the shorthand form there is no key to disambiguate, so YAML types
        // it as a boolean. The error has to say so, because the file looks
        // perfectly reasonable to whoever wrote it.
        let bare = "task_id: T-1\nrepository: r\nobjective: o\nevaluation:\n  tests: true\n";
        let err = serde_yaml_ng::from_str::<EngineeringTask>(bare).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("boolean"), "{message}");
        assert!(message.contains("quote it"), "{message}");
    }

    #[test]
    fn a_mistyped_command_key_names_the_valid_keys() {
        let raw = "task_id: T-1\nrepository: r\nobjective: o\nevaluation:\n  tests:\n    commnad: cargo test\n";
        let err = serde_yaml_ng::from_str::<EngineeringTask>(raw).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("commnad"), "{message}");
        assert!(message.contains("timeout_secs"), "{message}");
    }

    #[test]
    fn a_command_written_as_a_list_is_explained() {
        let raw = "task_id: T-1\nrepository: r\nobjective: o\nevaluation:\n  tests:\n    - cargo\n    - test\n";
        let err = serde_yaml_ng::from_str::<EngineeringTask>(raw).unwrap_err();
        assert!(err.to_string().contains("one string"), "{err}");
    }

    #[test]
    fn empty_objective_is_invalid() {
        let task = EngineeringTask {
            task_id: TaskId::sequential(1),
            repository: "r".into(),
            objective: "   ".into(),
            constraints: vec![],
            evaluation: EvaluationSpec::default(),
            protection: ProtectionPolicy::default(),
            metadata: TaskMetadata::default(),
        };
        assert!(task.validate().is_err());
    }

    #[test]
    fn a_task_without_evaluation_warns_but_validates() {
        let task = EngineeringTask {
            task_id: TaskId::sequential(1),
            repository: "r".into(),
            objective: "Make the storage layer faster under contention".into(),
            constraints: vec![],
            evaluation: EvaluationSpec::default(),
            protection: ProtectionPolicy::default(),
            metadata: TaskMetadata::default(),
        };
        task.validate().unwrap();
        assert!(
            task.warnings()
                .iter()
                .any(|w| w.contains("cannot independently judge"))
        );
    }

    #[test]
    fn serialization_round_trips() {
        let raw = r#"
task_id: T-0010
repository: forge
objective: Improve worktree cleanup on interrupted runs
constraints:
  - All existing tests must pass
evaluation:
  tests: cargo test --workspace
metadata:
  task_type: bugfix
  language: rust
  labels:
    priority: high
"#;
        let task: EngineeringTask = serde_yaml_ng::from_str(raw).unwrap();
        let round_tripped: EngineeringTask =
            serde_yaml_ng::from_str(&serde_yaml_ng::to_string(&task).unwrap()).unwrap();
        assert_eq!(task, round_tripped);
    }

    #[test]
    fn protected_paths_and_task_scoped_exceptions_parse_at_the_top_level() {
        let raw = r#"
task_id: T-0011
repository: forge
objective: Add a regression test for worktree cleanup
protected_paths:
  - tests/**
  - benches/**
allowed_protected_paths:
  - tests/worktree_cleanup.rs
evaluation:
  tests: cargo test --workspace
"#;
        let task: EngineeringTask = serde_yaml_ng::from_str(raw).unwrap();
        task.validate().unwrap();
        assert_eq!(task.protection.protected, vec!["tests/**", "benches/**"]);
        assert_eq!(task.protection.allowed, vec!["tests/worktree_cleanup.rs"]);

        let round_trip: EngineeringTask =
            serde_yaml_ng::from_str(&serde_yaml_ng::to_string(&task).unwrap()).unwrap();
        assert_eq!(round_trip, task);
    }

    #[test]
    fn benchmark_metrics_file_is_typed_and_must_stay_in_the_repository() {
        let valid = r#"
task_id: T-0012
repository: forge
objective: Measure checkpoint throughput with structured output
evaluation:
  benchmark:
    command: ./bench.sh
    metrics_file: .forge-metrics.json
"#;
        let task: EngineeringTask = serde_yaml_ng::from_str(valid).unwrap();
        task.validate().unwrap();
        assert_eq!(
            task.evaluation
                .benchmark
                .as_ref()
                .unwrap()
                .metrics_file
                .as_deref(),
            Some(".forge-metrics.json")
        );

        let hostile = valid.replace(".forge-metrics.json", "../../outside.json");
        let task: EngineeringTask = serde_yaml_ng::from_str(&hostile).unwrap();
        assert!(task.validate().is_err());
    }
}
