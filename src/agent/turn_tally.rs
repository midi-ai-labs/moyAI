//! The tallies one agent turn reports in every terminal, checkpoint and cancellation.
use super::*;

/// Counts and usage that `run_inner` accumulates across model rounds. A shared
/// run resumes them from its checkpoint progress.
pub(super) struct TurnTally {
    pub tool_call_count: usize,
    pub failed_tool_count: usize,
    pub change_count: usize,
    pub model_request_count: usize,
    pub tool_calls_by_name: BTreeMap<String, usize>,
    pub failed_tool_calls_by_name: BTreeMap<String, usize>,
    pub latest_usage: Option<TokenUsage>,
    pub cumulative_token_usage: crate::session::CumulativeTokenUsage,
    /// The newest response whose assistant/tool-call bundle is durable.
    pub last_model_response_id: Option<ModelResponseId>,
}

impl TurnTally {
    pub(super) fn from_progress(progress: shared::SharedProgress) -> Self {
        Self {
            tool_call_count: progress.tool_call_count,
            failed_tool_count: progress.failed_tool_count,
            change_count: progress.change_count,
            model_request_count: progress.model_request_count,
            tool_calls_by_name: progress.tool_calls_by_name,
            failed_tool_calls_by_name: progress.failed_tool_calls_by_name,
            latest_usage: progress.latest_usage,
            cumulative_token_usage: progress.cumulative_token_usage.unwrap_or_default(),
            last_model_response_id: None,
        }
    }

    pub(super) fn shared_progress(&self) -> shared::SharedProgress {
        shared::SharedProgress {
            tool_call_count: self.tool_call_count,
            failed_tool_count: self.failed_tool_count,
            change_count: self.change_count,
            model_request_count: self.model_request_count,
            tool_calls_by_name: self.tool_calls_by_name.clone(),
            failed_tool_calls_by_name: self.failed_tool_calls_by_name.clone(),
            latest_usage: self.latest_usage.clone(),
            cumulative_token_usage: Some(self.cumulative_token_usage.clone()),
        }
    }

    pub(super) fn metrics(
        &self,
        hub_route: Option<&crate::hub::HubTurnRoute>,
        request: &AgentRunRequest,
        started_at: Instant,
    ) -> RunMetrics {
        run_metrics(
            hub_route,
            request,
            started_at,
            self.model_request_count,
            self.latest_usage.clone(),
            &self.cumulative_token_usage,
            &self.tool_calls_by_name,
            &self.failed_tool_calls_by_name,
        )
    }
}

impl AgentLoop {
    /// Settle the turn by its recorded run-control cause with the current tallies.
    pub(super) async fn finish_for_cause(
        &self,
        request: &AgentRunRequest,
        tally: &TurnTally,
        started_at: Instant,
        expected_active_goal_id: Option<&str>,
        sink: &mut dyn RunEventSink,
    ) -> Result<shared::AgentRunOutcome, AgentError> {
        self.finish_for_run_control_cause(
            request,
            tally.last_model_response_id,
            tally.latest_usage.clone(),
            &tally.cumulative_token_usage,
            tally.tool_call_count,
            tally.failed_tool_count,
            tally.change_count,
            tally.model_request_count,
            tally.tool_calls_by_name.clone(),
            tally.failed_tool_calls_by_name.clone(),
            started_at,
            expected_active_goal_id,
            sink,
        )
        .await
        .map(shared::AgentRunOutcome::Completed)
    }
}
