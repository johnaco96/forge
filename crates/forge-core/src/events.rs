//! The trajectory of a run, recorded as an ordered event stream.
//!
//! Forge stores what happened, not just whether it worked. `task -> success`
//! throws away the commands, retries, and dead ends that make the record
//! useful later — those are exactly the signal a routing model will need.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{RunId, TaskId};
use crate::result::{Dimension, Verdict};
use crate::run::{AgentExecutionStatus, RunOutcome};

/// One recorded occurrence in a run.
///
/// Serializes to the documented shape:
/// `{"run_id": ..., "timestamp": ..., "event_type": ..., "data": {...}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub run_id: RunId,
    /// Monotonic per run. Timestamps can collide at millisecond resolution;
    /// ordering must not depend on them.
    #[serde(default)]
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    #[serde(flatten)]
    pub payload: EventPayload,
}

impl Event {
    pub fn event_type(&self) -> &'static str {
        self.payload.event_type()
    }
}

/// The event kinds Forge records, with their payloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "data")]
pub enum EventPayload {
    RunStarted {
        task_id: TaskId,
        agent_id: String,
        base_commit: String,
    },
    WorkspaceCreated {
        path: PathBuf,
        branch: String,
        base_commit: String,
    },
    AgentStarted {
        command: String,
    },
    PromptSubmitted {
        prompt: String,
    },
    FileRead {
        path: PathBuf,
    },
    FileModified {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        insertions: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deletions: Option<u64>,
    },
    CommandExecuted {
        command: String,
        exit_code: i32,
        duration_ms: u64,
    },
    TestPassed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suite: Option<String>,
        duration_ms: u64,
    },
    TestFailed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suite: Option<String>,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    BenchmarkStarted {
        name: String,
    },
    BenchmarkCompleted {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        duration_ms: u64,
    },
    AgentFinished {
        /// How the process ended. Never how well it did.
        status: AgentExecutionStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        timed_out: bool,
        duration_ms: u64,
        /// References to captured output, not the output itself.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_path: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_path: Option<PathBuf>,
    },
    /// Forge read the change out of Git. Emitted on the evaluation side of the
    /// trust boundary: this is measured, not reported.
    PatchCaptured {
        files_changed: u64,
        insertions: u64,
        deletions: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff_path: Option<PathBuf>,
    },
    EvaluationStarted {
        evaluators: Vec<String>,
    },
    EvaluationCompleted {
        verdict: Verdict,
    },
    RunScored {
        dimensions: BTreeMap<Dimension, f64>,
    },
    RunCompleted {
        outcome: RunOutcome,
        duration_ms: u64,
    },
    RunFailed {
        reason: String,
    },
    RunCancelled {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl EventPayload {
    /// The discriminant, used as the indexed `event_type` column in the store.
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::RunStarted { .. } => "RunStarted",
            Self::WorkspaceCreated { .. } => "WorkspaceCreated",
            Self::AgentStarted { .. } => "AgentStarted",
            Self::PromptSubmitted { .. } => "PromptSubmitted",
            Self::FileRead { .. } => "FileRead",
            Self::FileModified { .. } => "FileModified",
            Self::CommandExecuted { .. } => "CommandExecuted",
            Self::TestPassed { .. } => "TestPassed",
            Self::TestFailed { .. } => "TestFailed",
            Self::BenchmarkStarted { .. } => "BenchmarkStarted",
            Self::BenchmarkCompleted { .. } => "BenchmarkCompleted",
            Self::AgentFinished { .. } => "AgentFinished",
            Self::PatchCaptured { .. } => "PatchCaptured",
            Self::EvaluationStarted { .. } => "EvaluationStarted",
            Self::EvaluationCompleted { .. } => "EvaluationCompleted",
            Self::RunScored { .. } => "RunScored",
            Self::RunCompleted { .. } => "RunCompleted",
            Self::RunFailed { .. } => "RunFailed",
            Self::RunCancelled { .. } => "RunCancelled",
        }
    }
}

/// Where components send events without knowing how they are persisted.
///
/// Deliberately synchronous and infallible: recording a trajectory must never
/// be able to fail a run or force every call site to be async.
pub trait EventSink: Send + Sync {
    fn emit(&self, payload: EventPayload);
}

/// Discards everything. For tests and for callers that do not persist runs.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _payload: EventPayload) {}
}

/// Buffers events in memory, stamping run id, sequence, and timestamp.
///
/// The CLI flushes the buffer to the store at the end of a run. A streaming
/// sink can replace this without changing any producer.
#[derive(Debug)]
pub struct RecordingSink {
    run_id: RunId,
    seq: AtomicU64,
    events: Mutex<Vec<Event>>,
}

impl RecordingSink {
    pub fn new(run_id: RunId) -> Self {
        Self {
            run_id,
            seq: AtomicU64::new(0),
            events: Mutex::new(Vec::new()),
        }
    }

    /// Returns a copy of everything recorded so far, in order.
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().expect("event buffer poisoned").clone()
    }

    /// Empties the buffer and returns its contents.
    pub fn drain(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().expect("event buffer poisoned"))
    }

    pub fn len(&self) -> usize {
        self.events.lock().expect("event buffer poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl EventSink for RecordingSink {
    fn emit(&self, payload: EventPayload) {
        let event = Event {
            run_id: self.run_id.clone(),
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            timestamp: Utc::now(),
            payload,
        };
        self.events
            .lock()
            .expect("event buffer poisoned")
            .push(event);
    }
}

impl<T: EventSink + ?Sized> EventSink for &T {
    fn emit(&self, payload: EventPayload) {
        (**self).emit(payload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The event JSON in the design document must round-trip through `Event`.
    #[test]
    fn parses_the_design_document_example() {
        let raw = r#"
        {
          "run_id": "R-8821",
          "timestamp": "2026-08-10T21:32:15Z",
          "event_type": "CommandExecuted",
          "data": {
            "command": "cargo test -p storage",
            "exit_code": 1,
            "duration_ms": 4821
          }
        }
        "#;

        let event: Event = serde_json::from_str(raw).unwrap();
        assert_eq!(event.run_id.as_str(), "R-8821");
        assert_eq!(event.event_type(), "CommandExecuted");
        assert!(matches!(
            event.payload,
            EventPayload::CommandExecuted { exit_code: 1, .. }
        ));

        let value: serde_json::Value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["event_type"], "CommandExecuted");
        assert_eq!(value["data"]["duration_ms"], 4821);
    }

    #[test]
    fn every_variant_round_trips() {
        let payloads = vec![
            EventPayload::RunStarted {
                task_id: TaskId::sequential(1),
                agent_id: "claude".into(),
                base_commit: "a73cf21".into(),
            },
            EventPayload::WorkspaceCreated {
                path: PathBuf::from("/tmp/ws"),
                branch: "forge/run-1".into(),
                base_commit: "a73cf21".into(),
            },
            EventPayload::AgentStarted {
                command: "claude -p".into(),
            },
            EventPayload::PromptSubmitted {
                prompt: "improve throughput".into(),
            },
            EventPayload::FileRead {
                path: PathBuf::from("src/storage.rs"),
            },
            EventPayload::FileModified {
                path: PathBuf::from("src/storage.rs"),
                insertions: Some(10),
                deletions: None,
            },
            EventPayload::CommandExecuted {
                command: "cargo test".into(),
                exit_code: 0,
                duration_ms: 1,
            },
            EventPayload::TestPassed {
                suite: None,
                duration_ms: 1,
            },
            EventPayload::TestFailed {
                suite: Some("storage".into()),
                duration_ms: 1,
                detail: Some("assertion failed".into()),
            },
            EventPayload::BenchmarkStarted {
                name: "checkpoint".into(),
            },
            EventPayload::BenchmarkCompleted {
                name: "checkpoint".into(),
                value: Some(4.72),
                unit: Some("GB/s".into()),
                duration_ms: 1,
            },
            EventPayload::AgentFinished {
                status: AgentExecutionStatus::NonZeroExit,
                exit_code: Some(1),
                timed_out: false,
                duration_ms: 1,
                stdout_path: Some(PathBuf::from("stdout.log")),
                stderr_path: None,
            },
            EventPayload::PatchCaptured {
                files_changed: 3,
                insertions: 120,
                deletions: 63,
                diff_path: Some(PathBuf::from("patch.diff")),
            },
            EventPayload::RunCompleted {
                outcome: RunOutcome::Passed,
                duration_ms: 1,
            },
            EventPayload::EvaluationStarted {
                evaluators: vec!["tests".into()],
            },
            EventPayload::EvaluationCompleted {
                verdict: Verdict::Pass,
            },
            EventPayload::RunScored {
                dimensions: BTreeMap::from([(Dimension::Correctness, 1.0)]),
            },
            EventPayload::RunFailed {
                reason: "timeout".into(),
            },
            EventPayload::RunCancelled { reason: None },
        ];

        for payload in payloads {
            let event = Event {
                run_id: RunId::sequential(1),
                seq: 0,
                timestamp: Utc::now(),
                payload: payload.clone(),
            };
            let json = serde_json::to_string(&event).unwrap();
            let back: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(back.payload, payload, "round trip failed for {json}");
            assert_eq!(back.event_type(), payload.event_type());
        }
    }

    #[test]
    fn recording_sink_assigns_monotonic_sequence_numbers() {
        let sink = RecordingSink::new(RunId::sequential(4));
        sink.emit(EventPayload::AgentStarted {
            command: "a".into(),
        });
        sink.emit(EventPayload::AgentFinished {
            status: AgentExecutionStatus::Completed,
            exit_code: Some(0),
            timed_out: false,
            duration_ms: 5,
            stdout_path: None,
            stderr_path: None,
        });

        let events = sink.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 0);
        assert_eq!(events[1].seq, 1);
        assert!(events.iter().all(|e| e.run_id.as_str() == "R-0004"));
    }

    #[test]
    fn draining_empties_the_buffer() {
        let sink = RecordingSink::new(RunId::sequential(1));
        sink.emit(EventPayload::FileRead {
            path: PathBuf::from("a"),
        });
        assert_eq!(sink.drain().len(), 1);
        assert!(sink.is_empty());
    }

    #[test]
    fn sequence_numbers_are_unique_under_concurrency() {
        use std::sync::Arc;

        let sink = Arc::new(RecordingSink::new(RunId::sequential(1)));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let sink = Arc::clone(&sink);
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        sink.emit(EventPayload::FileRead {
                            path: PathBuf::from("a"),
                        });
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let mut seqs: Vec<u64> = sink.events().iter().map(|e| e.seq).collect();
        seqs.sort_unstable();
        seqs.dedup();
        assert_eq!(seqs.len(), 400);
    }
}
