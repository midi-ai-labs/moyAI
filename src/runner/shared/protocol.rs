//! The bounded shared-work wire contract. Local paths and permission choices are never inputs.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::RunnerError;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SharedInput {
    pub version: u32,
    pub prompt: String,
    #[serde(default)]
    pub input_refs: Vec<String>,
}

impl SharedInput {
    pub fn validate(&self) -> Result<(), RunnerError> {
        if !matches!(self.version, 1 | 2)
            || self.prompt.trim().is_empty()
            || self.prompt.len() > crate::agent::shared::MAX_SHARED_PROMPT_BYTES
            || (self.version == 1 && !self.input_refs.is_empty())
            || self.input_refs.len() > 32
            || self
                .input_refs
                .iter()
                .any(|id| !crate::device_network::stable_id(id))
            || self
                .input_refs
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.input_refs.len()
        {
            return Err(RunnerError::new(
                "Shared input requires version 1 or 2, a nonempty prompt of at most 32 KiB, and at most 32 immutable input references",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Assigned,
    Running,
    WaitingChild,
    Succeeded,
    Failed,
    Cancelling,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub id: String,
    pub root_id: String,
    pub parent_id: Option<String>,
    pub project_id: String,
    pub environment_id: String,
    pub requestor_id: String,
    pub assignee_id: String,
    pub title: String,
    pub input: Value,
    pub checkpoint: Option<Value>,
    pub result: Option<Value>,
    pub state: JobState,
    pub awaiting_child_id: Option<String>,
    pub revision: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Assignment {
    pub attempt_id: String,
    pub generation: u64,
    #[serde(default = "initial_authority_generation")]
    pub authority_generation: u64,
    pub runner_id: String,
    pub stop_requested: bool,
    pub job: Job,
    pub child_result: Option<Box<Job>>,
    #[serde(default)]
    pub allowed_child_environments: Vec<String>,
}

fn initial_authority_generation() -> u64 {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttemptStatus {
    pub assignment: Assignment,
    pub state: String,
    pub uncertainty_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub event_id: String,
    pub attempt_id: String,
    pub generation: u64,
    pub outcome: ReportOutcome,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReportOutcome {
    Started,
    YieldToChild {
        checkpoint: Value,
        child_environment_id: String,
        child_title: String,
        child_input: Value,
        resources_released: bool,
    },
    Finished {
        success: bool,
        result: Value,
        resources_released: bool,
    },
    Uncertain {
        reason: String,
    },
    ApprovalRequested {
        approval_id: String,
        request: Value,
        expires_at_ms: u64,
    },
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApprovalDecision {
    pub approval_id: String,
    pub decision: super::super::LocalApprovalDecision,
}

impl Report {
    pub(crate) fn for_assignment(
        assignment: &Assignment,
        suffix: &str,
        outcome: ReportOutcome,
    ) -> Self {
        Self {
            event_id: format!("{}:{suffix}", assignment.attempt_id),
            attempt_id: assignment.attempt_id.clone(),
            generation: assignment.generation,
            outcome,
        }
    }
}
