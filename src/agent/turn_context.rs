use std::sync::Arc;

use crate::agent::mode::CollaborationMode;
use crate::config::{MultiAgentMode, ResolvedTurnConfig};
use crate::context::current_time::CurrentTimeSnapshot;
use crate::llm::model_policy::ResolvedTurnPolicy;
use crate::protocol::TurnId;
use crate::session::AdmissionId;

#[derive(Debug, Clone)]
pub struct TurnContext {
    pub turn_id: TurnId,
    pub admission_id: AdmissionId,
    pub mode: CollaborationMode,
    pub policy: Arc<ResolvedTurnPolicy>,
    /// Complete effective configuration captured once for this admitted turn.
    /// Policies and per-step state are derived projections, never co-owners.
    pub config: Arc<ResolvedTurnConfig>,
    /// Goal identity and steering state captured by the same transaction that
    /// admitted this turn. Later goal edits cannot rewrite an in-flight model
    /// contract or redirect this turn's usage accounting.
    pub(crate) goal: Option<super::goal_steering::GoalSnapshot>,
    /// Turn-start wall clock used by every step in this turn. Keeping this
    /// transient snapshot immutable prevents a clock tick from severing
    /// Responses cursor lineage between otherwise identical tool rounds.
    pub current_time: CurrentTimeSnapshot,
}

impl TurnContext {
    /// Complete effective configuration captured when this turn was admitted.
    pub fn resolved_config(&self) -> &crate::config::ResolvedTurnConfig {
        &self.config
    }

    pub fn provider_target(&self) -> &crate::config::ProviderTarget {
        self.resolved_config().provider()
    }

    /// Multi-agent behavior is derived from the immutable turn configuration;
    /// it is never captured as a second field with an independent lifetime.
    pub fn multi_agent_mode(&self) -> Option<MultiAgentMode> {
        let multi_agent = &self.resolved_config().runtime_config().multi_agent;
        multi_agent.enabled.then_some(multi_agent.mode)
    }

    pub(crate) fn goal(&self) -> Option<&super::goal_steering::GoalSnapshot> {
        self.goal.as_ref()
    }
}
