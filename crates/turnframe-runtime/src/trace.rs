//! A trace of whole turns, for a person debugging one: the message that arrived, each
//! step of its understanding, what was understood and decided, every model call with
//! its prompts and answer, and the reply with its replay record.
//!
//! Opt-in and local. A trace holds the users' words and every prompt, so it belongs on
//! a developer's disk or in a tool the deployment chose, never on by default. Set a
//! [`TurnTrace`] with [`OrchestratorBuilder::trace`](crate::orchestrator::OrchestratorBuilder::trace)
//! and wrap each provider in a [`TracedProvider`](turnframe_provider::trace::TracedProvider);
//! [`JsonlTrace`] is both, one JSON object per line, ready for `jq` or an importer.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use chrono::Utc;
use serde_json::{Value, json};
use turnframe_core::ids::TurnId;
use turnframe_core::reduce::ReductionPlan;
use turnframe_core::replay::ReplayRecord;
use turnframe_core::response::AssistantTurn;
use turnframe_core::turn::TurnInput;
use turnframe_core::understanding::Understanding;
use turnframe_provider::trace::{CallTrace, TracedCall, TracedOutcome};
use turnframe_understand::Step;

/// One thing a turn did, as a trace receives it.
#[derive(Debug)]
#[non_exhaustive]
pub enum TraceEvent<'a> {
    /// A turn arrived.
    Received {
        /// What arrived.
        input: &'a TurnInput,
    },
    /// Understanding decided something.
    Step {
        /// The turn.
        turn: TurnId,
        /// What it decided.
        step: &'a Step,
    },
    /// What the message was understood to say, card acts included.
    Understood {
        /// The turn.
        turn: TurnId,
        /// The understanding reduced.
        understanding: &'a Understanding,
    },
    /// What the reducer decided.
    Reduced {
        /// The turn.
        turn: TurnId,
        /// The plan.
        plan: &'a ReductionPlan,
    },
    /// The turn finished and this is the reply the user got.
    Completed {
        /// The reply.
        reply: &'a AssistantTurn,
        /// Everything the turn recorded.
        record: &'a ReplayRecord,
    },
    /// The turn failed.
    Failed {
        /// The turn.
        turn: TurnId,
        /// The stable error code.
        code: &'a str,
        /// What it recorded before it failed.
        record: &'a ReplayRecord,
    },
}

/// Where a runtime reports its turns. It must not block for long: it runs on the turn.
pub trait TurnTrace: Send + Sync {
    /// Records one event.
    fn event(&self, event: &TraceEvent<'_>);
}

/// Appends every event and model call to one JSON Lines file, flushed line by line
/// so a crash keeps what came before it.
#[derive(Debug)]
pub struct JsonlTrace {
    path: PathBuf,
    file: Mutex<BufWriter<File>>,
}

impl JsonlTrace {
    /// A new file in `directory`, created if missing, named after the moment it opens.
    ///
    /// # Errors
    ///
    /// The [`io::Error`] creating the directory or the file.
    pub fn create(directory: impl AsRef<Path>) -> io::Result<Self> {
        let directory = directory.as_ref();
        fs::create_dir_all(directory)?;
        let name = format!(
            "turnframe-{}-{}.jsonl",
            Utc::now().format("%Y%m%d-%H%M%S"),
            std::process::id()
        );
        let path = directory.join(name);
        let file = File::create(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(BufWriter::new(file)),
        })
    }

    /// The trace the `TURNFRAME_TRACE` variable asks for: `1` or `true` writes into
    /// `default`, any other value names the directory, unset or empty means none.
    ///
    /// # Errors
    ///
    /// The [`io::Error`] creating the directory or the file.
    pub fn from_environment(default: impl AsRef<Path>) -> io::Result<Option<Self>> {
        match std::env::var("TURNFRAME_TRACE") {
            Ok(value) if value == "1" || value.eq_ignore_ascii_case("true") => {
                Self::create(default).map(Some)
            }
            Ok(value) if !value.trim().is_empty() => Self::create(value.trim()).map(Some),
            _ => Ok(None),
        }
    }

    /// The file being written.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, mut line: Value) {
        if let Value::Object(map) = &mut line {
            map.insert("at".to_owned(), json!(Utc::now()));
        }
        let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        let written = serde_json::to_writer(&mut *file, &line)
            .map_err(io::Error::from)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.flush());
        if let Err(error) = written {
            tracing::warn!(
                target: "turnframe.trace",
                error = %error,
                "a trace line could not be written"
            );
        }
    }
}

impl TurnTrace for JsonlTrace {
    fn event(&self, event: &TraceEvent<'_>) {
        self.write(match event {
            TraceEvent::Received { input } => json!({
                "event": "turn_received", "turn": input.turn_id, "input": input,
            }),
            TraceEvent::Step { turn, step } => json!({
                "event": "step", "turn": turn, "step": step, "says": step.describe(),
            }),
            TraceEvent::Understood {
                turn,
                understanding,
            } => json!({
                "event": "understood", "turn": turn, "understanding": understanding,
            }),
            TraceEvent::Reduced { turn, plan } => json!({
                "event": "reduced", "turn": turn, "plan": plan,
            }),
            TraceEvent::Completed { reply, record } => json!({
                "event": "turn_completed", "turn": reply.turn_id, "reply": reply, "record": record,
            }),
            TraceEvent::Failed { turn, code, record } => json!({
                "event": "turn_failed", "turn": turn, "code": code, "record": record,
            }),
        });
    }
}

impl CallTrace for JsonlTrace {
    fn call(&self, call: &TracedCall<'_>) {
        let metadata = &call.request.metadata;
        let mut line = json!({
            "event": "model_call",
            "turn": metadata.get(turnframe_tasks::TURN_LABEL),
            "task": metadata.get(turnframe_tasks::TASK_LABEL),
            "purpose": call.request.purpose.as_str(),
            "provider": call.model.provider,
            "model": call.model.model,
            "latency_ms": u64::try_from(call.latency.as_millis()).unwrap_or(u64::MAX),
            "request": call.request,
        });
        let (key, value) = match &call.outcome {
            TracedOutcome::Response(response) => ("response", json!(response)),
            TracedOutcome::Streamed {
                text,
                finish,
                usage,
            } => (
                "streamed",
                json!({"text": text, "finish": finish, "usage": usage}),
            ),
            TracedOutcome::Failed(error) => (
                "error",
                json!({
                    "kind": error.kind().as_str(),
                    "code": error.code().map(|code| code.as_str().to_owned()),
                    "detail": error.detail().map(|detail| detail.as_str().to_owned()),
                }),
            ),
            _ => ("outcome", Value::Null),
        };
        if let Value::Object(map) = &mut line {
            map.insert(key.to_owned(), value);
        }
        self.write(line);
    }
}
