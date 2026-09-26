//! A shared job pauses only at a completed model response with one outstanding child call.
//! Version 2 receipts bind session-relative canonical order and support authenticated archive restore.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::TurnId;
use crate::session::{RunSummary, SessionId, TokenUsage, ToolCallId};

pub const MAX_SHARED_PROMPT_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone)]
pub struct SharedRunContext {
    pub job_id: String,
    pub attempt_id: String,
    pub generation: u64,
    pub project_id: String,
    pub environment_id: String,
    pub allowed_child_environments: Vec<String>,
    pub allowed_child_candidates: Vec<crate::runner::shared::SharedCandidate>,
    pub resume: Option<SharedResume>,
    pub continuation: Option<SharedContinuation>,
}

#[derive(Debug, Clone)]
pub struct SharedContinuation {
    pub previous_job_id: String,
    pub archive: Value,
}

#[derive(Debug, Clone)]
pub struct SharedResume {
    pub checkpoint: Value,
    pub child_result: Value,
    /// Authenticated Hub archive; only the storage owner may restore its missing exact session.
    pub archive: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SharedChildRequest {
    pub environment_id: String,
    pub title: String,
    pub input: Value,
}

#[derive(Debug, Clone)]
pub enum SharedRunOutcome {
    Completed(RunSummary),
    Yielded(SharedYield),
}

#[derive(Debug, Clone)]
pub struct SharedYield {
    pub checkpoint: Value,
    pub child: SharedChildRequest,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub(crate) retained_service: Option<crate::tool::shell::RetainedService>,
}

/// A confirmed Hub outcome can retire a paused local continuation without running the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SharedCheckpointSettlement {
    Applied,
    NoLongerPaused,
    Pending,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SharedProgress {
    pub tool_call_count: usize,
    pub failed_tool_count: usize,
    pub change_count: usize,
    pub model_request_count: usize,
    pub tool_calls_by_name: BTreeMap<String, usize>,
    pub failed_tool_calls_by_name: BTreeMap<String, usize>,
    pub latest_usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SharedCheckpoint {
    pub version: u32,
    pub checkpoint_id: String,
    pub job_id: String,
    pub project_id: String,
    pub environment_id: String,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub tool_call_id: ToolCallId,
    pub history_digest: String,
    pub child: SharedChildRequest,
    pub progress: SharedProgress,
}

#[derive(Debug)]
pub(crate) enum AgentRunOutcome {
    Completed(RunSummary),
    Yielded(SharedYieldProposal),
}

#[derive(Debug)]
pub(crate) struct SharedYieldProposal {
    pub tool_call_id: ToolCallId,
    pub child: SharedChildRequest,
    pub progress: SharedProgress,
    pub retained_service: Option<crate::tool::shell::RetainedService>,
}

impl SharedRunContext {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.resume.is_some() && self.continuation.is_some() {
            return Err(
                "a shared execution cannot resume a checkpoint and fork a continuation together"
                    .into(),
            );
        }
        if [
            &self.job_id,
            &self.attempt_id,
            &self.project_id,
            &self.environment_id,
        ]
        .into_iter()
        .any(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
            || self.generation == 0
            || self.allowed_child_environments.len() > 128
            || self.allowed_child_candidates.len() > 128
            || self
                .allowed_child_environments
                .iter()
                .any(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
            || self.allowed_child_candidates.iter().any(|candidate| {
                !self
                    .allowed_child_environments
                    .contains(&candidate.environment_id)
                    || candidate.device_label.len() > 256
                    || candidate.environment_label.len() > 256
                    || candidate.device_label.chars().any(char::is_control)
                    || candidate.environment_label.chars().any(char::is_control)
                    || candidate.capabilities.len() > 32
                    || candidate.capabilities.iter().any(|capability| {
                        capability.len() > 128 || capability.chars().any(char::is_control)
                    })
            })
        {
            return Err("invalid shared job identity or allowed environments".into());
        }
        Ok(())
    }

    pub(crate) fn checkpoint(&self) -> Result<Option<SharedCheckpoint>, String> {
        self.resume
            .as_ref()
            .map(|resume| {
                let receipt: SharedCheckpoint =
                    serde_json::from_value(resume.checkpoint.clone())
                        .map_err(|error| format!("invalid shared checkpoint: {error}"))?;
                if !matches!(receipt.version, 1 | 2)
                    || receipt.job_id != self.job_id
                    || receipt.project_id != self.project_id
                    || receipt.environment_id != self.environment_id
                {
                    return Err("shared checkpoint belongs to another job or environment".into());
                }
                Ok(receipt)
            })
            .transpose()
    }
}
