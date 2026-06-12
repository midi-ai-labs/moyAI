//! Phase14 core rebuild: thin agent loop boundary.
//!
//! The previous behavior-correction agent layer (~3MB: prompt projection,
//! lifecycle kernel/guard, tool orchestrator, repair/recovery/grounding
//! contracts) was removed in R1. This module keeps the public boundary that
//! `app::bootstrap` / `app::run_service` depend on, and R2 implements the new
//! minimal loop behind it (see `docs/design/core-rebuild-plan.md`).

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::cli::ConfirmationPrompt;
use crate::config::ResolvedConfig;
use crate::error::AgentError;
use crate::llm::LlmClient;
use crate::llm::ModelProfile;
use crate::protocol::{HistoryItem, HistoryItemPayload, TurnId};
use crate::runtime::RunEventSink;
use crate::session::{MessageId, RunSummary, SessionContext, SessionStateSnapshot};
use crate::storage::StoreBundle;
use crate::tool::context::ToolServices;
use crate::tool::registry::ToolRegistry;

/// Prompt construction marker. R2 replaces this with a loader for the
/// Markdown system-prompt asset.
#[derive(Debug, Default, Clone, Copy)]
pub struct PromptBuilder;

/// Canonical history input for a turn, read from the protocol item stream.
#[derive(Debug, Clone)]
pub struct RuntimeInputView {
    pub history_items: Vec<HistoryItem>,
}

impl RuntimeInputView {
    pub fn from_history_items(history_items: Vec<HistoryItem>) -> Self {
        Self { history_items }
    }

    pub fn has_user_turn(&self) -> bool {
        self.history_items
            .iter()
            .any(|item| matches!(item.payload, HistoryItemPayload::UserTurn { .. }))
    }
}

/// One turn-run request from the run service.
pub struct AgentRunRequest {
    pub session: SessionContext,
    pub user_message_id: MessageId,
    pub protocol_turn_id: TurnId,
    pub runtime_input: RuntimeInputView,
    pub state: SessionStateSnapshot,
    pub config: ResolvedConfig,
    pub model: ModelProfile,
    pub cancel: CancellationToken,
}

#[derive(Clone)]
pub struct AgentLoop {
    #[allow(dead_code)]
    llm: Arc<dyn LlmClient>,
    #[allow(dead_code)]
    registry: ToolRegistry,
    #[allow(dead_code)]
    store: StoreBundle,
    #[allow(dead_code)]
    prompt_builder: PromptBuilder,
    #[allow(dead_code)]
    tool_services: ToolServices,
}

impl AgentLoop {
    pub fn new(
        llm: Arc<dyn LlmClient>,
        registry: ToolRegistry,
        store: StoreBundle,
        prompt_builder: PromptBuilder,
        tool_services: ToolServices,
    ) -> Self {
        Self {
            llm,
            registry,
            store,
            prompt_builder,
            tool_services,
        }
    }

    pub async fn run(
        &self,
        _request: AgentRunRequest,
        _prompt: &mut dyn ConfirmationPrompt,
        _sink: &mut dyn RunEventSink,
    ) -> Result<RunSummary, AgentError> {
        Err(AgentError::Message(
            "agent core rebuild in progress (Phase14 R2): the turn loop is not wired yet"
                .to_string(),
        ))
    }
}
