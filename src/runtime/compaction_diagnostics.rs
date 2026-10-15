use serde::{Deserialize, Serialize};

use crate::llm::{ProviderFailureKind, ProviderRequestId};
use crate::protocol::ModelResponseId;
use crate::session::{FinishReason, TokenUsage};

/// Private harness evidence, kept separate from RunEvent and canonical history.
/// Only typed outcomes and sizes are retained; checkpoint text and raw errors are excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionDiagnostic {
    pub response_id: ModelResponseId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<ProviderRequestId>,
    pub outcome: CompactionDiagnosticOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    pub local_request_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_failure_kind: Option<ProviderFailureKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_budget: Option<CompactionCheckpointBudget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionDiagnosticOutcome {
    PreparationFailed,
    Cancelled,
    ProviderFailed,
    SourceContextOverflow,
    UsageAccountingFailed,
    InvalidFinish,
    UnexpectedToolCalls,
    EmptySummary,
    ReasoningOnlySaturation,
    InvalidCheckpoint,
    CheckpointRejected,
    CommitFailed,
    Committed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionCheckpointBudget {
    pub before_context_tokens: u32,
    pub after_context_tokens: u32,
    pub before_request_tokens: u32,
    pub projected_request_tokens: u32,
    pub provider_adjusted_projected_request_tokens: u32,
    pub observed_prompt_gap: u32,
    pub working_context_token_limit: u32,
}
