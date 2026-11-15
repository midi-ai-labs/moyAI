//! How one agent turn settles: the success terminal after a final answer, and the
//! failure terminal after an error. Both read the same tallies as cancellation.
use super::*;

impl AgentLoop {
    /// Commit the Completed terminal for a final answer. Storage may select a different
    /// durable terminal (an exact interrupt, or a terminal another owner already wrote);
    /// the returned summary always follows the durable one.
    pub(super) async fn commit_success_terminal(
        &self,
        request: &AgentRunRequest,
        tally: &TurnTally,
        started_at: Instant,
        active_goal_id_for_turn: Option<&str>,
        accepts_current_turn_mail: bool,
        sink: &mut dyn RunEventSink,
    ) -> Result<shared::AgentRunOutcome, AgentError> {
        let repo = self.store.session_repo();
        let review_state =
            repo.security_review_state(request.session.session.id, request.turn_id())?;
        if let Some(feedback) = security_review::completion_feedback(&review_state) {
            sink.emit(RunEvent::RecoverableRuntimeFeedback {
                session_id: request.session.session.id,
                feedback,
            })?;
        }
        let terminal = DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Completed,
            final_response_id: tally.last_model_response_id,
            tool_call_count: tally.tool_call_count,
            failed_tool_count: tally.failed_tool_count,
            change_count: tally.change_count,
            metrics: tally.metrics(self.hub_route.as_ref(), request, started_at),
        };
        let event = RunEvent::TurnTerminal {
            session_id: request.session.session.id,
            terminal: Box::new(terminal.clone()),
        };
        let Some(success_commit) = request.run_control.begin_success_commit() else {
            return self
                .finish_for_cause(
                    request,
                    &tally,
                    started_at,
                    active_goal_id_for_turn.as_deref(),
                    sink,
                )
                .await;
        };
        #[cfg(test)]
        if let Some(hook) = self.before_success_terminal_commit.as_ref() {
            hook();
        }
        let terminal_settlement = match repo
            .settle_admitted_turn_with_protocol_event_and_mailbox_phase(
                request.session.session.id,
                request.admission_id(),
                &event,
                request.turn_id(),
                sink.reserve_protocol_sequence_no(),
                accepts_current_turn_mail,
                None,
            )
            .await
        {
            Ok(settlement) => settlement,
            Err(error) => match self.durable_terminal_summary(request).await {
                Ok(Some(summary)) => {
                    resolve_success_commit_from_durable_summary(success_commit, &summary);
                    return Ok(shared::AgentRunOutcome::Completed(summary));
                }
                Ok(None) => {
                    let message = format!(
                        "success terminal commit failed without durable terminal evidence: {error}"
                    );
                    success_commit
                        .abandon_with_cancellation(RunCancellationCause::Failure(message.clone()));
                    return Err(AgentError::Message(message));
                }
                Err(authority_error) => {
                    let message = format!(
                        "success terminal commit failed and durable ownership could not be read: {error}; authority read: {authority_error}"
                    );
                    success_commit
                        .abandon_with_cancellation(RunCancellationCause::Failure(message.clone()));
                    return Err(AgentError::Message(message));
                }
            },
        };
        if matches!(&terminal_settlement, AdmittedTerminalSettlement::NotOwned) {
            let durable_summary = match self.durable_terminal_summary(request).await {
                Ok(summary) => summary,
                Err(error) => {
                    let message = format!(
                        "durable terminal ownership could not be resolved after a lost success commit: {error}"
                    );
                    success_commit
                        .abandon_with_cancellation(RunCancellationCause::Failure(message.clone()));
                    return Err(AgentError::Message(message));
                }
            };
            if let Some(summary) = durable_summary {
                resolve_success_commit_from_durable_summary(success_commit, &summary);
                return Ok(shared::AgentRunOutcome::Completed(summary));
            }
            success_commit.abandon_with_cancellation(RunCancellationCause::Superseded);
            return Err(run_superseded_error(request));
        }
        let applied = matches!(
            &terminal_settlement,
            AdmittedTerminalSettlement::Applied { .. }
        );
        let effective_terminal = terminal_settlement
            .into_terminal()
            .expect("owned admitted settlement carries its durable terminal");
        let summary = run_summary_from_terminal(request, effective_terminal.clone());
        resolve_success_commit_from_durable_summary(success_commit, &summary);
        if applied {
            // Storage may replace stale Completed with a committed exact interrupt.
            // Emit the same authoritative terminal that the transaction selected.
            let _ = sink.emit_committed(RunEvent::TurnTerminal {
                session_id: request.session.session.id,
                terminal: Box::new(effective_terminal),
            });
        }
        return Ok(shared::AgentRunOutcome::Completed(summary));
    }

    /// Settle a turn that ended with `error`: prefer a durable terminal, then the
    /// run-control cause, then ownership, and record Failed only while still owned.
    pub(super) async fn settle_run_error(
        &self,
        request: &AgentRunRequest,
        error: AgentError,
        tally: &TurnTally,
        started_at: Instant,
        active_goal_id_for_turn: Option<&str>,
        sink: &mut dyn RunEventSink,
    ) -> Result<shared::AgentRunOutcome, AgentError> {
        let repo = self.store.session_repo();
        if let Some(summary) = self.durable_terminal_summary(request).await? {
            return Ok(shared::AgentRunOutcome::Completed(summary));
        }
        // The RunControl classification is the in-process terminal owner. A durable
        // tree-stop fence intentionally makes the admission look unavailable so no new
        // work can start, but it does not turn an already-classified interruption into a
        // supersession. Settle the first typed cause before interpreting admission loss.
        if request.run_control.is_cancelled() {
            return self
                .finish_for_cause(
                    request,
                    &tally,
                    started_at,
                    active_goal_id_for_turn.as_deref(),
                    sink,
                )
                .await;
        }
        if matches!(&error, AgentError::RunSuperseded { .. }) {
            return Err(error);
        }
        match repo
            .admitted_run_state(
                request.session.session.id,
                request.turn.admission_id,
                request.turn.turn_id,
            )
            .await?
        {
            AdmittedRunState::OwnedRunning => {}
            AdmittedRunState::Terminal(terminal) => {
                return Ok(shared::AgentRunOutcome::Completed(
                    run_summary_from_terminal(request, terminal),
                ));
            }
            AdmittedRunState::StopFenced(outcome) => {
                classify_run_control_for_terminal_outcome(&request.run_control, &outcome);
                return self
                    .finish_for_cause(
                        request,
                        &tally,
                        started_at,
                        active_goal_id_for_turn.as_deref(),
                        sink,
                    )
                    .await;
            }
            AdmittedRunState::SupersededOrExpired => {
                request.run_control.supersede();
                return Err(run_superseded_error(request));
            }
        }
        let failure_message = error.public_message();
        if request
            .run_control
            .request_cancel(RunCancellationCause::Failure(failure_message.clone()))
            != RunCancelOutcome::Applied
        {
            if request.run_control.cause().is_some() {
                return self
                    .finish_for_cause(
                        request,
                        &tally,
                        started_at,
                        active_goal_id_for_turn.as_deref(),
                        sink,
                    )
                    .await;
            }
            return Err(error);
        }
        let terminal = DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Failed {
                error: failure_message,
            },
            final_response_id: None,
            tool_call_count: tally.tool_call_count,
            failed_tool_count: tally.failed_tool_count,
            change_count: tally.change_count,
            metrics: tally.metrics(self.hub_route.as_ref(), request, started_at),
        };
        let terminal_settlement = self
            .settle_terminal(request, &terminal, active_goal_id_for_turn.as_deref(), sink)
            .await?;
        let applied = matches!(
            &terminal_settlement,
            AdmittedTerminalSettlement::Applied { .. }
        );
        if let Some(effective_terminal) = terminal_settlement.into_terminal() {
            if applied
                && matches!(
                    effective_terminal.outcome,
                    crate::protocol::TurnTerminalOutcome::Failed { .. }
                )
            {
                return Err(error);
            }
            return Ok(shared::AgentRunOutcome::Completed(
                run_summary_from_terminal(request, effective_terminal),
            ));
        }
        if let Some(summary) = self.durable_terminal_summary(request).await? {
            return Ok(shared::AgentRunOutcome::Completed(summary));
        }
        request.run_control.supersede();
        Err(run_superseded_error(request))
    }
}
