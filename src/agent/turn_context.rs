use std::sync::Arc;

use crate::agent::mode::CollaborationMode;
use crate::config::model::MultiAgentMode;
use crate::context::current_time::CurrentTimeSnapshot;
use crate::llm::model_policy::ResolvedTurnPolicy;
use crate::protocol::TurnId;

#[derive(Debug, Clone)]
pub struct TurnContext {
    pub turn_id: TurnId,
    pub admission_id: String,
    pub mode: CollaborationMode,
    pub policy: Arc<ResolvedTurnPolicy>,
    pub multi_agent_mode: Option<MultiAgentMode>,
    /// Turn-start wall clock used by every step in this turn. Keeping this
    /// transient snapshot immutable prevents a clock tick from severing
    /// Responses cursor lineage between otherwise identical tool rounds.
    pub current_time: CurrentTimeSnapshot,
}
