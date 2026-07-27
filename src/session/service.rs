use std::fs;

use camino::{Utf8Path, Utf8PathBuf};

use crate::config::{ProviderEndpoint, ResolvedConfig};
use crate::error::SessionError;
use crate::protocol::{
    CanonicalProtocolSnapshot, CanonicalRuntimeEventProjector, HistoryItem, ModeKind,
    ProtocolEventStore, ProtocolPageRequest, SteerTurn, TurnId, TurnInterruptionCause,
    TurnTerminalOutcome,
};
#[cfg(test)]
use crate::protocol::{TurnItem, UserTurn};
use crate::runtime::{ActiveRunInterruptOutcome, RunCancellationCause};
#[cfg(test)]
use crate::session::AdmissionId;
use crate::session::{
    CanonicalHistoryPage, CanonicalRuntimeEventPage, CanonicalSessionFence, CanonicalSessionRead,
    CanonicalSessionSnapshot, CanonicalTurnPage, DurableTurnTerminal, IdleTurnAdmission,
    IdleTurnRejectionReason, LoadedSessionList, LoadedSessionStatus, LoadedSessionSummary,
    NewSession, ProjectId, ProjectRecord, ProjectRepository, RunEvent, RunningSessionRejoin,
    SessionContext, SessionForkResult, SessionId, SessionRecord, SessionRepository,
    SessionRollbackResult, SessionSelector, SessionSettingsPatch, SessionSettingsUpdate,
    SessionStartRequest, SessionStatus, SessionTitleUpdate,
};
use crate::storage::StoreBundle;
use crate::storage::session_repo::{
    AgentExecutionWakeTerminalOwner, AgentExecutionWakeTerminalSettlement, AgentTreeStopFence,
    DurableSessionStopState, PendingAgentTriggerSettlement, RunningSessionTerminalTarget,
};
use crate::workspace::{PathGuard, Workspace, WorkspaceDiscovery};

const RUNNING_SESSION_RECOVERY_PAGE_SIZE: usize = 64;

#[derive(Clone)]
pub struct SessionService {
    pub store: StoreBundle,
    runtime_event_projector: Option<CanonicalRuntimeEventProjector>,
}

impl SessionService {
    pub fn new(store: StoreBundle) -> Self {
        Self {
            store,
            runtime_event_projector: None,
        }
    }

    pub fn with_runtime_event_projector(
        mut self,
        projector: CanonicalRuntimeEventProjector,
    ) -> Self {
        self.runtime_event_projector = Some(projector);
        self
    }

    pub async fn start_or_resume(
        &self,
        mut request: SessionStartRequest,
        workspace: Workspace,
    ) -> Result<SessionContext, SessionError> {
        request.base_url = ProviderEndpoint::parse(&request.base_url)
            .map_err(|error| SessionError::Message(error.to_string()))?
            .as_str()
            .to_string();
        let project_vcs_kind = match workspace.vcs {
            crate::workspace::VcsKind::Git => "git",
            crate::workspace::VcsKind::None => "none",
        };
        let workspace_cwd = normalize_session_cwd_for_project(
            &workspace.root,
            workspace.project_id,
            project_vcs_kind,
            workspace.authority_root(),
        )?;
        let requested_cwd = normalize_session_cwd_for_project(
            &workspace.root,
            workspace.project_id,
            project_vcs_kind,
            &request.cwd,
        )?;
        if !PathGuard::same_path_identity(&requested_cwd, &workspace_cwd) {
            return Err(SessionError::Message(format!(
                "session request workspace directory {} does not match the current workspace authority {}",
                request.cwd,
                workspace.authority_root()
            )));
        }
        let repository = self.store.session_repo();
        let session = match &request.selector {
            SessionSelector::New => {
                let title = request.title.unwrap_or_else(|| "New Session".to_string());
                repository
                    .create_session(NewSession {
                        project_id: workspace.project_id,
                        title,
                        cwd: workspace_cwd,
                        model: request.model.clone(),
                        base_url: request.base_url.clone(),
                        access_mode: request.access_mode,
                    })
                    .await?
            }
            SessionSelector::ById(_) | SessionSelector::Latest => self
                .resolve_session_for_workspace(&request.selector, &workspace)
                .await?
                .ok_or_else(|| SessionError::Message("no recent session exists".to_string()))?,
        };

        let has_fresh_run_admission = repository.has_fresh_run_admission(session.id).await?;
        if has_fresh_run_admission || self.store.active_runs().is_active(session.id) {
            return Err(SessionError::Message(format!(
                "session {} is already running; use cancel or an active-turn steer/rejoin surface instead of starting a replacement run",
                session.id
            )));
        }
        ProviderEndpoint::parse(&session.base_url)
            .map_err(|error| SessionError::Message(error.to_string()))?;
        Ok(SessionContext { session, workspace })
    }

    pub async fn resolve_session_for_workspace(
        &self,
        selector: &SessionSelector,
        workspace: &Workspace,
    ) -> Result<Option<SessionRecord>, SessionError> {
        let repository = self.store.session_repo();
        let session = match selector {
            SessionSelector::New => None,
            SessionSelector::ById(id) => Some(repository.get_session(*id).await?),
            SessionSelector::Latest => repository.latest_session(workspace.project_id).await?,
        };
        if let Some(session) = &session
            && session.project_id != workspace.project_id
        {
            return Err(SessionError::Message(format!(
                "session {} belongs to project {}, not the current workspace project {}; reopen its workspace before resuming it",
                session.id, session.project_id, workspace.project_id
            )));
        }
        let session = match session {
            Some(session) => Some(self.normalize_session_record_cwd(session).await?),
            None => None,
        };
        if let Some(session) = &session {
            if !PathGuard::same_path_identity(&session.cwd, workspace.authority_root()) {
                return Err(SessionError::Message(format!(
                    "session {} uses workspace directory {}, not the current workspace authority {}; reopen the session workspace before resuming it",
                    session.id,
                    session.cwd,
                    workspace.authority_root()
                )));
            }
        }
        Ok(session)
    }

    #[cfg(test)]
    pub(crate) async fn store_user_turn_with_protocol_bundle(
        &self,
        ctx: &SessionContext,
        admission_id: AdmissionId,
        turn: &UserTurn,
        protocol_turn_id: crate::protocol::TurnId,
        protocol_sequence_no: i64,
    ) -> Result<(), SessionError> {
        let repository = self.store.session_repo();
        repository
            .append_user_turn_with_protocol_bundle(
                ctx.session.id,
                admission_id,
                turn,
                protocol_turn_id,
                protocol_sequence_no,
            )
            .await?;
        Ok(())
    }

    pub async fn cancel_running_session(
        &self,
        session_id: crate::session::SessionId,
    ) -> Result<bool, SessionError> {
        let repository = self.store.session_repo();
        let state = repository
            .durable_session_stop_state(session_id)
            .await?
            .ok_or_else(|| SessionError::Message(format!("session {session_id} was not found")))?;
        let DurableSessionStopState::Running(target) = state else {
            return Ok(false);
        };
        self.cancel_running_session_turn(
            session_id,
            target.turn_id(),
            TurnInterruptionCause::UserStop,
        )
        .await
    }

    /// Interrupts only the exact durable turn captured by a caller.
    ///
    /// A replacement turn, an idle/terminal session, or a differently-classified local
    /// cancellation all fail closed. When no process-local run owns the exact durable turn, the
    /// same compare-and-set terminalization used by ordinary Stop closes that captured turn.
    pub async fn cancel_running_session_turn(
        &self,
        session_id: SessionId,
        expected_turn_id: TurnId,
        cause: TurnInterruptionCause,
    ) -> Result<bool, SessionError> {
        let repository = self.store.session_repo();
        let state = repository
            .durable_session_stop_state(session_id)
            .await?
            .ok_or_else(|| SessionError::Message(format!("session {session_id} was not found")))?;
        let DurableSessionStopState::Running(target) = state else {
            return Ok(false);
        };
        if target.turn_id() != expected_turn_id {
            return Ok(false);
        }
        let active_control = self.store.active_runs().run_control(session_id);
        Ok(
            match self
                .store
                .active_runs()
                .cancel_turn(session_id, expected_turn_id, cause)
            {
                ActiveRunInterruptOutcome::Applied | ActiveRunInterruptOutcome::Deferred => true,
                ActiveRunInterruptOutcome::AlreadyClassified => {
                    active_control.is_some_and(|control| {
                        control.cause() == Some(RunCancellationCause::Interruption(cause))
                    })
                }
                ActiveRunInterruptOutcome::TargetChanged => false,
                ActiveRunInterruptOutcome::NotActive => {
                    self.terminalize_running_session(
                        session_id,
                        RunEvent::TurnTerminal {
                            session_id,
                            terminal: Box::new(DurableTurnTerminal {
                                outcome: TurnTerminalOutcome::Interrupted { cause },
                                final_response_id: None,
                                tool_call_count: 0,
                                failed_tool_count: 0,
                                change_count: 0,
                                metrics: Default::default(),
                            }),
                        },
                        target,
                    )
                    .await?
                }
            },
        )
    }

    pub async fn cancel_running_session_tree(
        &self,
        session_id: crate::session::SessionId,
        root_cause: TurnInterruptionCause,
    ) -> Result<bool, SessionError> {
        let repo = self.store.session_repo();
        let root_stop_state = repo
            .durable_session_stop_state(session_id)
            .await?
            .ok_or_else(|| SessionError::Message(format!("session {session_id} was not found")))?;
        let observed_root_turn = match root_stop_state {
            DurableSessionStopState::Running(target) => Some(target.turn_id()),
            DurableSessionStopState::Idle | DurableSessionStopState::Terminal(_) => None,
        };

        let root_control = self.store.active_runs().run_control(session_id);
        let (fanout_authorized, mut cancelled) = match root_stop_state {
            DurableSessionStopState::Running(root_target) => match self
                .store
                .active_runs()
                .cancel_turn(session_id, root_target.turn_id(), root_cause)
            {
                ActiveRunInterruptOutcome::Applied => {
                    // The in-process worker owns settlement for its current admission.
                    (true, true)
                }
                ActiveRunInterruptOutcome::AlreadyClassified => {
                    let owns_requested_stop = root_control.as_ref().is_some_and(|control| {
                        control.cause() == Some(RunCancellationCause::Interruption(root_cause))
                    });
                    if owns_requested_stop {
                        (true, true)
                    } else if root_control
                        .as_ref()
                        .is_some_and(|control| control.success_is_sealed())
                        && matches!(
                            repo.durable_session_stop_state(session_id).await?,
                            Some(DurableSessionStopState::Terminal(SessionStatus::Completed))
                        )
                    {
                        // Durable root success is final even while its in-memory lease is being
                        // released. A user Stop may still target detached descendants.
                        (true, false)
                    } else {
                        (false, false)
                    }
                }
                ActiveRunInterruptOutcome::Deferred => {
                    // The root success commit remains authoritative, but an explicit user Stop
                    // may still stop detached descendants while that commit settles.
                    (true, true)
                }
                ActiveRunInterruptOutcome::TargetChanged => {
                    // A replacement turn won after the durable target was captured. It is
                    // classified only if its turn began before the fence recorded below.
                    (true, false)
                }
                ActiveRunInterruptOutcome::NotActive => {
                    self.settle_captured_root_for_tree_stop(session_id, root_target, root_cause)
                        .await?
                }
            },
            DurableSessionStopState::Terminal(_) => {
                // The root worker is gone, so a later explicit tree-wide Stop may target
                // detached descendants without rewriting the root's durable result.
                (true, false)
            }
            DurableSessionStopState::Idle => (false, false),
        };
        if !fanout_authorized {
            // A competing in-memory terminal classification at the requested root is
            // authoritative. Descendants must not be stopped through an independent fallback
            // path; durable terminal roots are handled above after the worker lease is gone.
            return Ok(false);
        }

        // Persist the exact global append-order boundary before enumerating or settling any
        // descendants. This is the durable owner of result-vs-Stop races, including a Stop
        // against detached descendants after the requested root already completed.
        let fence = match observed_root_turn {
            Some(turn_id) => {
                repo.record_agent_tree_stop_fence_for_observed_turn(session_id, root_cause, turn_id)
                    .await?
            }
            None => {
                repo.record_agent_tree_stop_fence(session_id, root_cause)
                    .await?
            }
        }
        .ok_or_else(|| {
            SessionError::Message(format!(
                "session {session_id} disappeared before its tree-stop fence was recorded"
            ))
        })?;
        cancelled |= self
            .fanout_agent_tree_stop_at_fence(session_id, fence)
            .await?;
        Ok(cancelled)
    }

    async fn fanout_agent_tree_stop_at_fence(
        &self,
        session_id: SessionId,
        fence: AgentTreeStopFence,
    ) -> Result<bool, SessionError> {
        let repo = self.store.session_repo();
        let mut cancelled = false;
        // New spawn edges are fenced by the exact root admission. Cross-process terminalization
        // closes that fence before this snapshot; in-process cancellation closes the shared
        // AgentControl tree. Enumerating only after either owner is closed prevents a concurrent
        // child from escaping this Stop fan-out.
        let targets = repo.list_session_subtree_ids(session_id).await?;
        // Settle deepest descendants first. A descendant TreeStopped terminal may discard its
        // parent's deferred completion, making a queued explicit trigger on that parent eligible
        // for synthetic Stop settlement later in this same pass.
        for target_session_id in targets.into_iter().rev() {
            let child_stop_state = repo
                .durable_session_stop_state_at_tree_stop_fence(target_session_id, fence)
                .await?
                .unwrap_or(DurableSessionStopState::Idle);
            if let DurableSessionStopState::Running(target) = child_stop_state {
                let Some(cause) = repo
                    .tree_stop_interruption_cause_for_running_target_at_fence(
                        target_session_id,
                        target,
                        fence,
                    )
                    .await?
                else {
                    continue;
                };
                let child_control = self.store.active_runs().run_control(target_session_id);
                match self.store.active_runs().cancel_turn(
                    target_session_id,
                    target.turn_id(),
                    cause,
                ) {
                    ActiveRunInterruptOutcome::Applied => {
                        cancelled = true;
                        continue;
                    }
                    ActiveRunInterruptOutcome::AlreadyClassified => {
                        // An already-classified target keeps its first typed cause. The Stop
                        // boundary never rewrites an independent failure or sealed success.
                        cancelled |= child_control.is_some_and(|control| {
                            control.cause() == Some(RunCancellationCause::Interruption(cause))
                        });
                        continue;
                    }
                    ActiveRunInterruptOutcome::Deferred => {
                        continue;
                    }
                    ActiveRunInterruptOutcome::TargetChanged
                    | ActiveRunInterruptOutcome::NotActive => {}
                }
                cancelled |= self
                    .terminalize_running_session(
                        target_session_id,
                        RunEvent::TurnTerminal {
                            session_id: target_session_id,
                            terminal: Box::new(DurableTurnTerminal {
                                outcome: TurnTerminalOutcome::Interrupted { cause },
                                final_response_id: None,
                                tool_call_count: 0,
                                failed_tool_count: 0,
                                change_count: 0,
                                metrics: Default::default(),
                            }),
                        },
                        target,
                    )
                    .await?;
                continue;
            }
            if let Some(expected_history_item_id) =
                repo.pending_agent_trigger_history_item_id_for_tree_stop(target_session_id, fence)?
            {
                let settlement = self.settle_pending_agent_trigger_at_tree_stop_fence(
                    target_session_id,
                    expected_history_item_id,
                    fence,
                )?;
                match settlement {
                    PendingAgentTriggerSettlement::Applied { .. } => cancelled = true,
                    PendingAgentTriggerSettlement::WakeOwnedOrResolved => {}
                    PendingAgentTriggerSettlement::BlockedByPendingDeferredCompletion {
                        deferred_turn_id,
                    } => {
                        return Err(SessionError::Message(format!(
                            "tree-stop settlement reached an impossible deferred-owner blocker \
                             for session {target_session_id} at turn {deferred_turn_id}"
                        )));
                    }
                }
            }
        }
        Ok(cancelled)
    }

    async fn settle_captured_root_for_tree_stop(
        &self,
        session_id: SessionId,
        root_target: RunningSessionTerminalTarget,
        root_cause: TurnInterruptionCause,
    ) -> Result<(bool, bool), SessionError> {
        let terminalized = self
            .terminalize_running_session(
                session_id,
                RunEvent::TurnTerminal {
                    session_id,
                    terminal: Box::new(DurableTurnTerminal {
                        outcome: TurnTerminalOutcome::Interrupted { cause: root_cause },
                        final_response_id: None,
                        tool_call_count: 0,
                        failed_tool_count: 0,
                        change_count: 0,
                        metrics: Default::default(),
                    }),
                },
                root_target,
            )
            .await?;
        if terminalized {
            return Ok((true, true));
        }

        let repo = self.store.session_repo();
        let current_state = repo
            .durable_session_stop_state(session_id)
            .await?
            .ok_or_else(|| {
                SessionError::Message(format!("session {session_id} disappeared during tree Stop"))
            })?;
        let captured_turn_reached_terminal = repo
            .durable_terminal_for_turn(session_id, root_target.turn_id())
            .await?
            .is_some();
        let replacement_precedes_fence = matches!(
            current_state,
            DurableSessionStopState::Running(current_target)
                if current_target.turn_id() != root_target.turn_id()
        );
        Ok((
            captured_turn_reached_terminal || replacement_precedes_fence,
            false,
        ))
    }

    pub async fn interrupt_running_session(
        &self,
        session_id: SessionId,
    ) -> Result<SessionRecord, SessionError> {
        if !self.cancel_running_session(session_id).await? {
            let session = self.store.session_repo().get_session(session_id).await?;
            return Err(SessionError::Message(format!(
                "session {} is {}; interrupt requires a running session",
                session.id,
                session.status.key()
            )));
        }
        Ok(self.store.session_repo().get_session(session_id).await?)
    }

    pub async fn evaluate_idle_turn_admission(
        &self,
        session_id: SessionId,
        pending_trigger_turn: bool,
    ) -> Result<IdleTurnAdmission, SessionError> {
        let repository = self.store.session_repo();
        let blocks_mutation = repository.session_blocks_mutation(session_id).await?;
        let session = repository.get_session(session_id).await?;
        let rejection_reason = if pending_trigger_turn {
            Some(IdleTurnRejectionReason::PendingTriggerTurn)
        } else if blocks_mutation
            || !matches!(
                session.status,
                SessionStatus::Idle | SessionStatus::Completed
            )
        {
            Some(IdleTurnRejectionReason::Busy)
        } else {
            None
        };
        Ok(IdleTurnAdmission {
            session,
            admitted: rejection_reason.is_none(),
            rejection_reason,
        })
    }

    pub async fn store_active_turn_steer(
        &self,
        session_id: crate::session::SessionId,
        steer: &SteerTurn,
    ) -> Result<(), SessionError> {
        self.store
            .session_repo()
            .accept_active_turn_steer(session_id, steer)
            .await?;
        if self.store.active_runs().is_active(session_id) {
            let _ = self
                .store
                .active_runs()
                .notify_steer_activity(session_id, steer.expected_turn_id);
        }
        Ok(())
    }

    pub async fn mark_stale_running_sessions(&self, reason: &str) -> Result<usize, SessionError> {
        let repository = self.store.session_repo();
        let Some(fence) = repository.running_session_recovery_fence().await? else {
            return Ok(0);
        };
        let mut after = None;
        let mut cancelled = 0;
        loop {
            let sessions = repository
                .running_session_recovery_page(after, fence, RUNNING_SESSION_RECOVERY_PAGE_SIZE)
                .await?;
            let Some(last_cursor) = sessions.last().map(|candidate| candidate.cursor()) else {
                break;
            };
            after = Some(last_cursor);

            for candidate in sessions {
                if self.store.active_runs().is_active(candidate.session.id) {
                    continue;
                }
                let Ok(_process_lease) = self
                    .store
                    .try_acquire_run_process_lease(candidate.session.id)
                else {
                    continue;
                };
                if self
                    .recover_orphaned_running_session(
                        candidate.session.id,
                        RunEvent::TurnTerminal {
                            session_id: candidate.session.id,
                            terminal: Box::new(DurableTurnTerminal {
                                outcome: TurnTerminalOutcome::Failed {
                                    error: reason.to_string(),
                                },
                                final_response_id: None,
                                tool_call_count: 0,
                                failed_tool_count: 0,
                                change_count: 0,
                                metrics: Default::default(),
                            }),
                        },
                        candidate.terminal_target,
                    )
                    .await?
                {
                    cancelled += 1;
                }
            }
        }
        Ok(cancelled)
    }

    /// Replays canonical terminals into any mapped harness run left Started by
    /// a process crash between the semantic commit and observer projection.
    pub(crate) fn reconcile_started_harness_terminals(
        &self,
    ) -> Result<usize, crate::error::StorageError> {
        let store = self.store.harness_run_store();
        let mut after_run_id = None;
        let mut terminalized_runs = 0usize;
        loop {
            let page = store.reconcile_started_canonical_terminals_page(
                after_run_id,
                crate::harness::MAX_HARNESS_TERMINAL_RECONCILIATION_PAGE_SIZE,
            )?;
            terminalized_runs = terminalized_runs.saturating_add(page.terminalized_runs);
            let Some(next_after_run_id) = page.next_after_run_id else {
                break;
            };
            if Some(next_after_run_id) == after_run_id {
                return Err(crate::error::StorageError::Message(
                    "harness terminal reconciliation cursor did not advance".to_string(),
                ));
            }
            after_run_id = Some(next_after_run_id);
        }
        Ok(terminalized_runs)
    }

    async fn terminalize_running_session(
        &self,
        session_id: SessionId,
        event: RunEvent,
        target: RunningSessionTerminalTarget,
    ) -> Result<bool, SessionError> {
        let projection_cursor = self.capture_runtime_projection_cursor(session_id)?;
        let terminalized = self
            .store
            .session_repo()
            .terminalize_captured_running_session_with_protocol_event(session_id, &event, target)
            .await?;
        if terminalized {
            self.project_runtime_events_after_cursor(
                session_id,
                projection_cursor,
                "captured running-session terminal",
            );
        }
        Ok(terminalized)
    }

    async fn recover_orphaned_running_session(
        &self,
        session_id: SessionId,
        event: RunEvent,
        target: RunningSessionTerminalTarget,
    ) -> Result<bool, SessionError> {
        let projection_cursor = self.capture_runtime_projection_cursor(session_id)?;
        let terminalized = self
            .store
            .session_repo()
            .recover_captured_running_session_with_protocol_event(session_id, &event, target)
            .await?;
        if terminalized {
            self.project_runtime_events_after_cursor(
                session_id,
                projection_cursor,
                "orphaned running-session recovery",
            );
        }
        Ok(terminalized)
    }

    pub(crate) fn settle_pending_agent_trigger_with_terminal(
        &self,
        session_id: SessionId,
        expected_history_item_id: crate::protocol::HistoryItemId,
        terminal: DurableTurnTerminal,
    ) -> Result<PendingAgentTriggerSettlement, crate::error::StorageError> {
        let projection_cursor = self.capture_runtime_projection_cursor_storage(session_id)?;
        let settlement = self
            .store
            .session_repo()
            .settle_pending_agent_trigger_with_terminal(
                session_id,
                expected_history_item_id,
                terminal,
            )?;
        if matches!(settlement, PendingAgentTriggerSettlement::Applied { .. }) {
            self.project_runtime_events_after_cursor(
                session_id,
                projection_cursor,
                "pre-admission explicit-trigger terminal",
            );
        }
        Ok(settlement)
    }

    pub(crate) fn settle_pending_owner_resume_with_terminal(
        &self,
        session_id: SessionId,
        expected_request_id: crate::storage::session_repo::OwnerResumeRequestId,
        terminal: DurableTurnTerminal,
    ) -> Result<PendingAgentTriggerSettlement, crate::error::StorageError> {
        let projection_cursor = self.capture_runtime_projection_cursor_storage(session_id)?;
        let settlement = self
            .store
            .session_repo()
            .settle_pending_owner_resume_with_terminal(session_id, expected_request_id, terminal)?;
        if matches!(settlement, PendingAgentTriggerSettlement::Applied { .. }) {
            self.project_runtime_events_after_cursor(
                session_id,
                projection_cursor,
                "pre-admission owner-resume terminal",
            );
        }
        Ok(settlement)
    }

    pub(crate) fn settle_agent_execution_wake_with_terminal(
        &self,
        session_id: SessionId,
        wake: AgentExecutionWakeTerminalOwner,
        terminal: DurableTurnTerminal,
    ) -> Result<AgentExecutionWakeTerminalSettlement, crate::error::StorageError> {
        let projection_cursor = self.capture_runtime_projection_cursor_storage(session_id)?;
        let settlement = self
            .store
            .session_repo()
            .settle_agent_execution_wake_with_terminal(session_id, wake, terminal)?;
        if matches!(
            settlement,
            AgentExecutionWakeTerminalSettlement::Applied { .. }
        ) {
            self.project_runtime_events_after_cursor(
                session_id,
                projection_cursor,
                "agent execution wake terminal",
            );
        }
        Ok(settlement)
    }

    fn settle_pending_agent_trigger_at_tree_stop_fence(
        &self,
        session_id: SessionId,
        expected_history_item_id: crate::protocol::HistoryItemId,
        fence: AgentTreeStopFence,
    ) -> Result<PendingAgentTriggerSettlement, crate::error::StorageError> {
        let projection_cursor = self.capture_runtime_projection_cursor_storage(session_id)?;
        let settlement = self
            .store
            .session_repo()
            .settle_pending_agent_trigger_at_tree_stop_fence(
                session_id,
                expected_history_item_id,
                fence,
            )?;
        if matches!(settlement, PendingAgentTriggerSettlement::Applied { .. }) {
            self.project_runtime_events_after_cursor(
                session_id,
                projection_cursor,
                "tree-stop pending-trigger terminal",
            );
        }
        Ok(settlement)
    }

    fn capture_runtime_projection_cursor(
        &self,
        session_id: SessionId,
    ) -> Result<Option<i64>, SessionError> {
        self.capture_runtime_projection_cursor_storage(session_id)
            .map_err(SessionError::from)
    }

    fn capture_runtime_projection_cursor_storage(
        &self,
        session_id: SessionId,
    ) -> Result<Option<i64>, crate::error::StorageError> {
        match self.runtime_event_projector.as_ref() {
            Some(projector) => projector.latest_cursor(session_id),
            None => Ok(None),
        }
    }

    fn project_runtime_events_after_cursor(
        &self,
        session_id: SessionId,
        cursor: Option<i64>,
        context: &str,
    ) {
        let Some(projector) = self.runtime_event_projector.as_ref() else {
            return;
        };
        match projector.project_after_cursor(session_id, cursor) {
            Ok(report) => report.log_failures(context),
            Err(error) => eprintln!(
                "warning: {context}: committed runtime outbox could not be replayed for session {session_id}: {error}"
            ),
        }
    }

    async fn normalize_session_record_cwd(
        &self,
        mut session: SessionRecord,
    ) -> Result<SessionRecord, SessionError> {
        let project = self
            .store
            .project_repo()
            .get_project(session.project_id)
            .await?;
        session.cwd = normalize_session_cwd_for_project(
            &project.root_path,
            session.project_id,
            &project.vcs_kind,
            &session.cwd,
        )?;
        Ok(session)
    }

    pub async fn get_session(&self, session_id: SessionId) -> Result<SessionRecord, SessionError> {
        let session = self.store.session_repo().get_session(session_id).await?;
        self.normalize_session_record_cwd(session).await
    }

    pub async fn latest_session(
        &self,
        project_id: ProjectId,
    ) -> Result<Option<SessionRecord>, SessionError> {
        match self.store.session_repo().latest_session(project_id).await? {
            Some(session) => Ok(Some(self.normalize_session_record_cwd(session).await?)),
            None => Ok(None),
        }
    }

    pub async fn list_sessions(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        Ok(self
            .store
            .session_repo()
            .list_sessions(project_id, limit)
            .await?)
    }

    pub async fn list_sessions_with_archived(
        &self,
        project_id: ProjectId,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        Ok(self
            .store
            .session_repo()
            .list_sessions_with_archived(project_id, limit, include_archived)
            .await?)
    }

    pub async fn search_sessions(
        &self,
        project_id: ProjectId,
        query: &str,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        if query.trim().is_empty() {
            return Err(SessionError::Message(
                "session search query must not be empty".to_string(),
            ));
        }
        Ok(self
            .store
            .session_repo()
            .search_sessions(project_id, query, limit, include_archived)
            .await?)
    }

    pub async fn set_session_archived(
        &self,
        session_id: SessionId,
        archived: bool,
    ) -> Result<SessionRecord, SessionError> {
        if archived
            && let Some(active_session_id) = self.active_session_in_tree_branch(session_id).await?
        {
            return Err(SessionError::Message(format!(
                "session {session_id} has active or pending agent-tree session {active_session_id}; stop the agent tree before archiving it"
            )));
        }
        Ok(self
            .store
            .session_repo()
            .set_session_archived(session_id, archived)
            .await?)
    }

    pub async fn update_session_settings(
        &self,
        session_id: SessionId,
        patch: SessionSettingsPatch,
    ) -> Result<SessionSettingsUpdate, SessionError> {
        if patch.is_empty() {
            return Err(SessionError::Message(
                "session settings update requires at least one setting".to_string(),
            ));
        }
        let repository = self.store.session_repo();
        let blocks_mutation = repository.session_blocks_mutation(session_id).await?;
        let session = repository.get_session(session_id).await?;
        if blocks_mutation || self.store.active_runs().is_active(session_id) {
            return Err(SessionError::Message(format!(
                "session {} is {}; settings update requires an idle or terminal session",
                session.id,
                session.status.key()
            )));
        }
        let project = self
            .store
            .project_repo()
            .get_project(session.project_id)
            .await?;
        let normalized = normalize_session_settings_patch(
            patch,
            &project.root_path,
            session.project_id,
            &project.vcs_kind,
        )?;
        Ok(self
            .store
            .session_repo()
            .update_session_settings(session_id, &normalized)
            .await?)
    }

    pub async fn update_root_session_access_mode(
        &self,
        session_id: SessionId,
        access_mode: crate::config::AccessMode,
    ) -> Result<SessionSettingsUpdate, SessionError> {
        for _ in 0..8 {
            let current = self.store.session_repo().get_session(session_id).await?;
            if let Some(update) = self
                .compare_and_set_root_session_access_mode(
                    session_id,
                    current.access_mode,
                    access_mode,
                )
                .await?
            {
                return Ok(update);
            }
        }
        Err(SessionError::Message(format!(
            "root session {session_id} access mode changed repeatedly; retry the operation"
        )))
    }

    pub async fn compare_and_set_root_session_access_mode(
        &self,
        session_id: SessionId,
        expected_access_mode: crate::config::AccessMode,
        access_mode: crate::config::AccessMode,
    ) -> Result<Option<SessionSettingsUpdate>, SessionError> {
        let repository = self.store.session_repo();
        Ok(repository
            .compare_and_set_root_session_access_mode(session_id, expected_access_mode, access_mode)
            .await?)
    }

    pub async fn update_session_title(
        &self,
        session_id: SessionId,
        title: String,
    ) -> Result<SessionTitleUpdate, SessionError> {
        let title = title.trim().to_string();
        if title.is_empty() {
            return Err(SessionError::Message(
                "session title must not be empty".to_string(),
            ));
        }
        Ok(self
            .store
            .session_repo()
            .update_session_title(session_id, &title)
            .await?)
    }

    pub async fn rollback_session(
        &self,
        session_id: SessionId,
        num_turns: usize,
    ) -> Result<SessionRollbackResult, SessionError> {
        if num_turns == 0 {
            return Err(SessionError::Message(
                "session rollback turn count must be greater than zero".to_string(),
            ));
        }
        if let Some(active_session_id) = self.active_session_in_tree_branch(session_id).await? {
            return Err(SessionError::Message(format!(
                "session {session_id} has active or pending agent-tree session {active_session_id}; stop the agent tree before rollback"
            )));
        }
        Ok(self
            .store
            .session_repo()
            .rollback_session_transaction(session_id, num_turns)
            .await?)
    }

    pub async fn fork_session(
        &self,
        source_session_id: SessionId,
        title: Option<String>,
    ) -> Result<SessionForkResult, SessionError> {
        Ok(self
            .store
            .session_repo()
            .fork_session_snapshot(source_session_id, title)
            .await?)
    }

    pub async fn list_recent_sessions(
        &self,
        limit: usize,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        Ok(self
            .store
            .session_repo()
            .list_recent_sessions(limit)
            .await?)
    }

    pub async fn loaded_sessions(
        &self,
        project_id: ProjectId,
        limit: usize,
        include_archived: bool,
    ) -> Result<LoadedSessionList, SessionError> {
        let sessions = self
            .store
            .session_repo()
            .list_sessions_with_projection_state(project_id, limit, include_archived)
            .await?;
        let mut summaries = Vec::with_capacity(sessions.len());
        for projection in sessions {
            summaries.push(loaded_session_summary_from_projection(projection));
        }
        Ok(LoadedSessionList {
            project_id,
            include_archived,
            sessions: summaries,
        })
    }

    pub async fn search_loaded_sessions(
        &self,
        project_id: ProjectId,
        query: &str,
        limit: usize,
        include_archived: bool,
    ) -> Result<LoadedSessionList, SessionError> {
        if query.trim().is_empty() {
            return self
                .loaded_sessions(project_id, limit, include_archived)
                .await;
        }
        let sessions = self
            .store
            .session_repo()
            .search_sessions_with_projection_state(project_id, query, limit, include_archived)
            .await?;
        let mut summaries = Vec::with_capacity(sessions.len());
        for projection in sessions {
            summaries.push(loaded_session_summary_from_projection(projection));
        }
        Ok(LoadedSessionList {
            project_id,
            include_archived,
            sessions: summaries,
        })
    }

    pub async fn loaded_session_summary(
        &self,
        session: SessionRecord,
    ) -> Result<LoadedSessionSummary, SessionError> {
        let projection = self
            .store
            .session_repo()
            .session_projection_state(session.id)
            .await?;
        Ok(loaded_session_summary_from_projection(projection))
    }

    pub async fn rejoin_running_session(
        &self,
        session_id: SessionId,
        history_offset: usize,
        history_limit: usize,
        turn_offset: usize,
        turn_limit: usize,
    ) -> Result<RunningSessionRejoin, SessionError> {
        let session = self.get_session(session_id).await?;
        if session.status != SessionStatus::Running {
            return Err(SessionError::Message(format!(
                "session {} is {}; rejoin is only available for active loaded sessions",
                session.id,
                session.status.key()
            )));
        }
        let summary = self.loaded_session_summary(session).await?;
        if summary.active_turn_id.is_none() {
            return Err(SessionError::Message(format!(
                "session {} is active but has no recorded active turn",
                session_id
            )));
        }
        let read = self
            .canonical_session_read(
                session_id,
                history_offset,
                history_limit,
                turn_offset,
                turn_limit,
            )
            .await?;
        Ok(RunningSessionRejoin { summary, read })
    }

    pub async fn delete_session(&self, session_id: SessionId) -> Result<(), SessionError> {
        if let Some(active_session_id) = self.active_session_in_tree_branch(session_id).await? {
            return Err(SessionError::Message(format!(
                "session {session_id} has active or pending agent-tree session {active_session_id}; stop the agent tree before deleting it"
            )));
        }
        Ok(self.store.session_repo().delete_session(session_id).await?)
    }

    async fn active_session_in_tree_branch(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionId>, SessionError> {
        let repository = self.store.session_repo();
        if let Some(session_id) = repository
            .mutation_blocker_in_session_tree(session_id)
            .await?
        {
            return Ok(Some(session_id));
        }
        let branch_session_ids = repository.list_session_subtree_ids(session_id).await?;

        for branch_session_id in branch_session_ids {
            if self.store.active_runs().is_active(branch_session_id) {
                return Ok(Some(branch_session_id));
            }
        }
        Ok(None)
    }

    pub async fn delete_project(&self, project_id: ProjectId) -> Result<(), SessionError> {
        let mut active_session_id = self
            .store
            .session_repo()
            .active_session_for_project(project_id)
            .await?;
        if active_session_id.is_none() {
            for session_id in self.store.active_runs().active_session_ids() {
                let session = self.store.session_repo().get_session(session_id).await?;
                if session.project_id == project_id {
                    active_session_id = Some(session_id);
                    break;
                }
            }
        }
        if let Some(session_id) = active_session_id {
            return Err(SessionError::Message(format!(
                "project {} contains active or pending session {}; stop it before deleting the project",
                project_id, session_id
            )));
        }
        Ok(self.store.project_repo().delete_project(project_id).await?)
    }

    pub async fn list_projects(&self, limit: usize) -> Result<Vec<ProjectRecord>, SessionError> {
        Ok(self.store.project_repo().list_projects(limit).await?)
    }

    #[cfg(test)]
    pub async fn canonical_history_items(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<HistoryItem>, SessionError> {
        self.store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
            .map_err(|error| SessionError::Message(error.to_string()))
    }

    /// Returns the collaboration mode replayed from canonical thread history.
    /// An empty history has the protocol default; no session column or planner
    /// state participates in this resolution.
    pub async fn collaboration_mode(
        &self,
        session_id: SessionId,
    ) -> Result<ModeKind, SessionError> {
        self.get_session(session_id).await?;
        self.store
            .protocol_event_store()
            .collaboration_mode_for_session(session_id)
            .map_err(|error| SessionError::Message(error.to_string()))
    }

    /// Persists a collaboration-mode instruction for subsequent turns.
    /// Same-value updates are atomic no-ops and therefore do not grow history.
    pub async fn set_collaboration_mode(
        &self,
        session_id: SessionId,
        mode: ModeKind,
    ) -> Result<Option<HistoryItem>, SessionError> {
        self.get_session(session_id).await?;
        self.store
            .protocol_event_store()
            .set_collaboration_mode(session_id, mode)
            .map_err(|error| SessionError::Message(error.to_string()))
    }

    pub async fn canonical_history_page(
        &self,
        session_id: SessionId,
        offset: usize,
        limit: usize,
    ) -> Result<CanonicalHistoryPage, SessionError> {
        validate_canonical_page_limit(limit)?;
        let session = self.get_session(session_id).await?;
        let page = self
            .store
            .protocol_event_store()
            .history_item_page_for_session(session_id, offset, limit)
            .map_err(|error| SessionError::Message(error.to_string()))?;
        let has_more = page.has_more();
        Ok(CanonicalHistoryPage {
            session,
            offset: page.offset,
            limit: page.limit,
            total: page.total,
            has_more,
            items: page.items,
        })
    }

    #[cfg(test)]
    pub async fn canonical_turn_items(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<TurnItem>, SessionError> {
        self.store
            .protocol_event_store()
            .list_turn_items_for_session(session_id)
            .map_err(|error| SessionError::Message(error.to_string()))
    }

    pub async fn canonical_turn_page(
        &self,
        session_id: SessionId,
        offset: usize,
        limit: usize,
    ) -> Result<CanonicalTurnPage, SessionError> {
        validate_canonical_page_limit(limit)?;
        let session = self.get_session(session_id).await?;
        let page = self
            .store
            .protocol_event_store()
            .turn_item_page_for_session(session_id, offset, limit)
            .map_err(|error| SessionError::Message(error.to_string()))?;
        let has_more = page.has_more();
        Ok(CanonicalTurnPage {
            session,
            offset: page.offset,
            limit: page.limit,
            total: page.total,
            has_more,
            items: page.items,
        })
    }

    pub async fn canonical_runtime_event_page(
        &self,
        session_id: SessionId,
        offset: usize,
        limit: usize,
    ) -> Result<CanonicalRuntimeEventPage, SessionError> {
        validate_canonical_page_limit(limit)?;
        let session = self.get_session(session_id).await?;
        let page = self
            .store
            .protocol_event_store()
            .runtime_event_page_for_session(session_id, offset, limit)
            .map_err(|error| SessionError::Message(error.to_string()))?;
        let has_more = page.has_more();
        Ok(CanonicalRuntimeEventPage {
            session,
            offset: page.offset,
            limit: page.limit,
            total: page.total,
            has_more,
            items: page.items,
        })
    }

    pub async fn canonical_session_read(
        &self,
        session_id: SessionId,
        history_offset: usize,
        history_limit: usize,
        turn_offset: usize,
        turn_limit: usize,
    ) -> Result<CanonicalSessionRead, SessionError> {
        Ok(self
            .canonical_session_snapshot(
                session_id,
                history_offset,
                history_limit,
                turn_offset,
                turn_limit,
            )
            .await?
            .read)
    }

    pub async fn canonical_session_snapshot(
        &self,
        session_id: SessionId,
        history_offset: usize,
        history_limit: usize,
        turn_offset: usize,
        turn_limit: usize,
    ) -> Result<CanonicalSessionSnapshot, SessionError> {
        self.canonical_session_snapshot_with_requests(
            session_id,
            ProtocolPageRequest::Offset {
                offset: history_offset,
                limit: history_limit,
            },
            ProtocolPageRequest::Offset {
                offset: turn_offset,
                limit: turn_limit,
            },
        )
        .await
    }

    pub async fn canonical_latest_session_snapshot(
        &self,
        session_id: SessionId,
        history_limit: usize,
        turn_limit: usize,
    ) -> Result<CanonicalSessionSnapshot, SessionError> {
        self.canonical_session_snapshot_with_requests(
            session_id,
            ProtocolPageRequest::Latest {
                limit: history_limit,
            },
            ProtocolPageRequest::Latest { limit: turn_limit },
        )
        .await
    }

    async fn canonical_session_snapshot_with_requests(
        &self,
        session_id: SessionId,
        history_request: ProtocolPageRequest,
        turn_request: ProtocolPageRequest,
    ) -> Result<CanonicalSessionSnapshot, SessionError> {
        let history_limit = match history_request {
            ProtocolPageRequest::Offset { limit, .. }
            | ProtocolPageRequest::Latest { limit }
            | ProtocolPageRequest::After { limit, .. } => limit,
        };
        let turn_limit = match turn_request {
            ProtocolPageRequest::Offset { limit, .. }
            | ProtocolPageRequest::Latest { limit }
            | ProtocolPageRequest::After { limit, .. } => limit,
        };
        validate_canonical_page_limit(history_limit)?;
        validate_canonical_page_limit(turn_limit)?;
        let mut snapshot = self
            .store
            .session_repo()
            .canonical_session_protocol_snapshot(session_id, history_request, turn_request)
            .await?;
        snapshot.session = self.normalize_session_record_cwd(snapshot.session).await?;
        Ok(canonical_session_snapshot_from_storage(snapshot))
    }
}

fn loaded_session_summary_from_projection(
    projection: crate::storage::session_repo::SessionProjectionState,
) -> LoadedSessionSummary {
    LoadedSessionSummary {
        loaded_status: loaded_status_from_session_status(projection.session.status),
        archived: projection.archived,
        active_turn_id: projection.active_turn_id,
        active_turn_sequence_no: projection.active_turn_sequence_no,
        pending_permission_requests: 0,
        pending_user_input_requests: 0,
        session: projection.session,
    }
}

fn canonical_session_snapshot_from_storage(
    snapshot: crate::storage::session_repo::CanonicalSessionStorageSnapshot,
) -> CanonicalSessionSnapshot {
    let crate::storage::session_repo::CanonicalSessionStorageSnapshot {
        session,
        protocol,
        active_turn_position,
        pending_turn_inputs,
    } = snapshot;
    let CanonicalProtocolSnapshot {
        fence,
        history,
        turns,
        turn_elapsed_ms,
        latest_turn_position,
    } = protocol;
    let history_has_more = history.has_more();
    let turn_has_more = turns.has_more();
    CanonicalSessionSnapshot {
        read: CanonicalSessionRead {
            session: session.clone(),
            history: CanonicalHistoryPage {
                session: session.clone(),
                offset: history.offset,
                limit: history.limit,
                total: history.total,
                has_more: history_has_more,
                items: history.items,
            },
            turns: CanonicalTurnPage {
                session,
                offset: turns.offset,
                limit: turns.limit,
                total: turns.total,
                has_more: turn_has_more,
                items: turns.items,
            },
            pending_turn_inputs,
            turn_elapsed_ms,
            latest_turn_id: latest_turn_position.map(|(turn_id, _)| turn_id),
            active_turn_id: active_turn_position.map(|(turn_id, _)| turn_id),
            active_turn_sequence_no: active_turn_position.map(|(_, sequence_no)| sequence_no),
        },
        fence: CanonicalSessionFence {
            append_position: fence.append_position,
            history_count: fence.history_count,
            turn_count: fence.turn_count,
            runtime_event_count: fence.runtime_event_count,
        },
    }
}

fn validate_canonical_page_limit(limit: usize) -> Result<(), SessionError> {
    if limit == 0 {
        return Err(SessionError::Message(
            "canonical item page limit must be greater than zero".to_string(),
        ));
    }
    if limit > crate::protocol::MAX_PROTOCOL_PAGE_LIMIT {
        return Err(SessionError::Message(format!(
            "canonical item page limit {limit} exceeds the maximum {}",
            crate::protocol::MAX_PROTOCOL_PAGE_LIMIT
        )));
    }
    Ok(())
}

fn loaded_status_from_session_status(status: SessionStatus) -> LoadedSessionStatus {
    match status {
        SessionStatus::Running => LoadedSessionStatus::Active,
        SessionStatus::Failed => LoadedSessionStatus::SystemError,
        SessionStatus::Idle | SessionStatus::Completed | SessionStatus::Cancelled => {
            LoadedSessionStatus::Idle
        }
    }
}

pub(crate) fn normalize_session_cwd_for_project(
    project_root: &Utf8Path,
    project_id: ProjectId,
    project_vcs_kind: &str,
    cwd: &Utf8Path,
) -> Result<Utf8PathBuf, SessionError> {
    let normalized = crate::workspace::project::normalize_path(project_root, cwd)
        .map_err(|error| SessionError::Message(error.to_string()))?;
    let relative = PathGuard::relative_path_from_root(&normalized, project_root).ok_or_else(|| {
        SessionError::Message(format!(
            "session workspace directory `{cwd}` is outside stored project root `{project_root}`"
        ))
    })?;
    let normalized = project_root.join(relative);
    match project_vcs_kind {
        "git" => {
            let discovered = WorkspaceDiscovery::discover(&normalized, &ResolvedConfig::default())
                .map_err(|error| SessionError::Message(error.to_string()))?;
            if discovered.project_id != project_id {
                return Err(SessionError::Message(format!(
                    "session workspace directory `{cwd}` resolves to project {}, not stored project {project_id}",
                    discovered.project_id
                )));
            }
        }
        "none" => {}
        other => {
            return Err(SessionError::Message(format!(
                "stored project {project_id} has unsupported vcs kind `{other}`"
            )));
        }
    }
    Ok(normalized)
}

fn normalize_session_settings_patch(
    patch: SessionSettingsPatch,
    project_root: &Utf8Path,
    project_id: ProjectId,
    project_vcs_kind: &str,
) -> Result<SessionSettingsPatch, SessionError> {
    let cwd = patch
        .cwd
        .map(|cwd| {
            normalize_session_cwd_for_project(project_root, project_id, project_vcs_kind, &cwd)
        })
        .transpose()?;
    if let Some(cwd) = cwd.as_ref() {
        let metadata = fs::metadata(cwd).map_err(|error| {
            SessionError::Message(format!(
                "session settings cwd `{cwd}` is not readable: {error}"
            ))
        })?;
        if !metadata.is_dir() {
            return Err(SessionError::Message(format!(
                "session settings cwd `{cwd}` must be a directory"
            )));
        }
    }
    let model = patch
        .model
        .map(|value| value.trim().to_string())
        .transpose_non_empty("session settings model")?;
    let base_url = patch
        .base_url
        .map(|value| {
            ProviderEndpoint::parse(&value)
                .map(|endpoint| endpoint.as_str().to_string())
                .map_err(|error| SessionError::Message(error.to_string()))
        })
        .transpose()?;
    if let Some(value) = patch.temperature {
        validate_finite_non_negative("session settings temperature", value)?;
    }
    if let Some(value) = patch.top_p {
        validate_finite_range("session settings top_p", value, 0.0, 1.0)?;
    }
    if let Some(value) = patch.top_k
        && value == 0
    {
        return Err(SessionError::Message(
            "session settings top_k must be greater than zero".to_string(),
        ));
    }
    if let Some(value) = patch.max_output_tokens
        && value == 0
    {
        return Err(SessionError::Message(
            "session settings max_output_tokens must be greater than zero".to_string(),
        ));
    }
    Ok(SessionSettingsPatch {
        cwd,
        model,
        base_url,
        access_mode: patch.access_mode,
        reset_model_parameters: patch.reset_model_parameters,
        temperature: patch.temperature,
        top_p: patch.top_p,
        top_k: patch.top_k,
        max_output_tokens: patch.max_output_tokens,
    })
}

fn validate_finite_non_negative(label: &str, value: f64) -> Result<(), SessionError> {
    if !value.is_finite() || value < 0.0 {
        return Err(SessionError::Message(format!(
            "{label} must be finite and non-negative"
        )));
    }
    Ok(())
}

fn validate_finite_range(label: &str, value: f64, min: f64, max: f64) -> Result<(), SessionError> {
    if !value.is_finite() || value < min || value > max {
        return Err(SessionError::Message(format!(
            "{label} must be finite and between {min} and {max}"
        )));
    }
    Ok(())
}

trait NonEmptySetting {
    fn transpose_non_empty(self, label: &str) -> Result<Option<String>, SessionError>;
}

impl NonEmptySetting for Option<String> {
    fn transpose_non_empty(self, label: &str) -> Result<Option<String>, SessionError> {
        match self {
            Some(value) if value.is_empty() => {
                Err(SessionError::Message(format!("{label} must not be empty")))
            }
            other => Ok(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AccessMode, ResolvedConfig};
    use crate::harness::{
        HarnessEventKind, HarnessEventPayload, HarnessEventStore, HarnessRunStatus,
        HarnessRunStore, NativeHarnessRecorder,
    };
    use crate::protocol::{
        HistoryItemPayload, InterAgentCommunication, ModeKind, RuntimeEvent, RuntimeEventMsg,
        TurnItemPayload, TurnTerminalOutcome, UserInputItem,
    };
    use crate::runtime::{RunControl, SessionRuntimeEventHub};
    use crate::storage::{SqliteStore, StoragePaths};
    use crate::workspace::WorkspaceDiscovery;

    async fn service_fixture() -> (SessionService, Workspace, Workspace) {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = camino::Utf8PathBuf::from_path_buf(temp.keep()).expect("utf8 root");
        let first_root = root.join("first");
        let second_root = root.join("second");
        fs::create_dir_all(first_root.as_std_path()).expect("first root");
        fs::create_dir_all(second_root.as_std_path()).expect("second root");
        let paths = StoragePaths {
            data_dir: root.join("data"),
            database_path: root.join("data/moyai.sqlite3"),
            truncation_dir: root.join("data/truncation"),
        };
        let sqlite = SqliteStore::open(&paths).expect("store");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let config = ResolvedConfig::default();
        let first = WorkspaceDiscovery::discover_fixed_root(&first_root, &config).expect("first");
        let second =
            WorkspaceDiscovery::discover_fixed_root(&second_root, &config).expect("second");
        for workspace in [&first, &second] {
            store
                .project_repo()
                .upsert_project(workspace.project_id, &workspace.root, "test", "none")
                .await
                .expect("project");
        }
        (SessionService::new(store), first, second)
    }

    async fn create_session(service: &SessionService, workspace: &Workspace) -> SessionContext {
        service
            .start_or_resume(
                SessionStartRequest {
                    selector: SessionSelector::New,
                    title: Some("test".to_string()),
                    cwd: workspace.cwd.clone(),
                    model: "model".to_string(),
                    base_url: "http://localhost:1234".to_string(),
                    access_mode: AccessMode::Default,
                },
                workspace.clone(),
            )
            .await
            .expect("session")
    }

    #[tokio::test]
    async fn session_create_and_settings_reject_url_borne_secrets_before_storage() {
        let (service, workspace, _) = service_fixture().await;
        for endpoint in [
            "https://user:secret@provider.example/v1",
            "https://provider.example/v1?api_key=hidden",
            "https://provider.example/v1#debug",
        ] {
            let error = service
                .start_or_resume(
                    SessionStartRequest {
                        selector: SessionSelector::New,
                        title: Some("rejected".to_string()),
                        cwd: workspace.cwd.clone(),
                        model: "model".to_string(),
                        base_url: endpoint.to_string(),
                        access_mode: AccessMode::Default,
                    },
                    workspace.clone(),
                )
                .await
                .expect_err("secret-bearing endpoint must be rejected");
            let diagnostic = format!("{error:?}: {error}");
            assert!(!diagnostic.contains("secret"));
            assert!(!diagnostic.contains("hidden"));
            assert!(!diagnostic.contains(endpoint));
        }

        let session = create_session(&service, &workspace).await;
        let error = service
            .update_session_settings(
                session.session.id,
                SessionSettingsPatch {
                    base_url: Some(
                        "https://user:secret@provider.example/v1?api_key=hidden".to_string(),
                    ),
                    ..SessionSettingsPatch::default()
                },
            )
            .await
            .expect_err("settings endpoint must be rejected");
        let diagnostic = format!("{error:?}: {error}");
        assert!(!diagnostic.contains("secret"));
        assert!(!diagnostic.contains("hidden"));
    }

    #[tokio::test]
    async fn collaboration_mode_query_replays_canonical_history_for_run_resolution() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;

        assert_eq!(
            service
                .collaboration_mode(session.session.id)
                .await
                .expect("default mode"),
            ModeKind::Default
        );
        assert!(
            service
                .set_collaboration_mode(session.session.id, ModeKind::Plan)
                .await
                .expect("set plan")
                .is_some()
        );
        assert!(
            service
                .set_collaboration_mode(session.session.id, ModeKind::Plan)
                .await
                .expect("same plan")
                .is_none()
        );

        let resumed = SessionService::new(service.store.clone());
        assert_eq!(
            resumed
                .collaboration_mode(session.session.id)
                .await
                .expect("resumed mode"),
            ModeKind::Plan
        );
        let items = resumed
            .canonical_history_items(session.session.id)
            .await
            .expect("history");
        assert_eq!(
            items
                .iter()
                .filter(|item| matches!(
                    &item.payload,
                    HistoryItemPayload::CollaborationModeInstruction { .. }
                ))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn canonical_snapshot_reports_only_a_fresh_durable_active_admission() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        let (admission_id, turn_id) = admit_session_turn(&service, session.session.id).await;
        let user_turn = UserTurn {
            turn_id,
            items: vec![UserInputItem::Text {
                text: "persisted request".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
        };
        service
            .store_user_turn_with_protocol_bundle(&session, admission_id, &user_turn, turn_id, 0)
            .await
            .expect("store user turn");

        let active = service
            .canonical_latest_session_snapshot(session.session.id, 10, 10)
            .await
            .expect("active snapshot");
        assert_eq!(active.read.active_turn_id, Some(turn_id));

        terminalize_admitted_session(&service, session.session.id, turn_id).await;
        let terminal = service
            .canonical_latest_session_snapshot(session.session.id, 10, 10)
            .await
            .expect("terminal snapshot");
        assert_eq!(terminal.read.session.status, SessionStatus::Completed);
        assert_eq!(terminal.read.active_turn_id, None);
        assert_eq!(terminal.read.active_turn_sequence_no, None);
        assert!(terminal.fence.history_count > 0);

        assert!(
            service
                .store
                .session_repo()
                .release_stopped_run_admission(session.session.id, admission_id)
                .await
                .expect("release completed admission")
        );
    }

    #[tokio::test]
    async fn canonical_snapshot_and_markdown_use_a_later_terminal_only_recovery_turn() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        let (older_admission_id, older_turn_id) =
            admit_session_turn(&service, session.session.id).await;
        let user_turn = UserTurn {
            turn_id: older_turn_id,
            items: vec![UserInputItem::Text {
                text: "older persisted request".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
        };
        service
            .store_user_turn_with_protocol_bundle(
                &session,
                older_admission_id,
                &user_turn,
                older_turn_id,
                0,
            )
            .await
            .expect("store older user turn");
        terminalize_admitted_session(&service, session.session.id, older_turn_id).await;
        assert!(
            service
                .store
                .session_repo()
                .release_stopped_run_admission(session.session.id, older_admission_id)
                .await
                .expect("release older admission")
        );

        let terminal_only_turn_id = TurnId::new();
        service
            .store
            .session_repo()
            .admit_session_turn_at(session.session.id, terminal_only_turn_id, 0, 1)
            .await
            .expect("admit expired terminal-only turn")
            .expect("terminal-only turn admitted");
        assert_eq!(
            service
                .mark_stale_running_sessions("recover terminal-only turn")
                .await
                .expect("recover terminal-only turn"),
            1
        );

        let snapshot = service
            .canonical_latest_session_snapshot(session.session.id, 10, 10)
            .await
            .expect("canonical snapshot");
        assert_eq!(snapshot.read.active_turn_id, None);
        assert_eq!(snapshot.read.latest_turn_id, Some(terminal_only_turn_id));
        let markdown = crate::session::canonical_session_read_to_markdown(&snapshot.read);
        assert!(markdown.contains("失敗しました: recover terminal-only turn"));
        assert!(!markdown.contains("完了しました。"));
    }

    async fn cross_process_service_fixture() -> (SessionService, SessionService, Workspace) {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = camino::Utf8PathBuf::from_path_buf(temp.keep()).expect("utf8 root");
        let workspace_root = root.join("workspace");
        fs::create_dir_all(workspace_root.as_std_path()).expect("workspace root");
        let paths = StoragePaths {
            data_dir: root.join("data"),
            database_path: root.join("data/moyai.sqlite3"),
            truncation_dir: root.join("data/truncation"),
        };
        let owner_sqlite = SqliteStore::open(&paths).expect("owner store");
        owner_sqlite.migrate().expect("migrate");
        let canceller_sqlite = SqliteStore::open(&paths).expect("canceller store");
        let owner = SessionService::new(StoreBundle::new(owner_sqlite));
        let canceller = SessionService::new(StoreBundle::new(canceller_sqlite));
        let config = ResolvedConfig::default();
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config).expect("workspace");
        owner
            .store
            .project_repo()
            .upsert_project(workspace.project_id, &workspace.root, "test", "none")
            .await
            .expect("project");
        (owner, canceller, workspace)
    }

    async fn admit_session_turn(
        service: &SessionService,
        session_id: SessionId,
    ) -> (AdmissionId, TurnId) {
        let repository = service.store.session_repo();
        let turn_id = TurnId::new();
        let admission_id = repository
            .admit_session_turn(session_id, turn_id)
            .await
            .expect("admit run")
            .expect("run admitted")
            .admission_id;
        (admission_id, turn_id)
    }

    async fn terminalize_admitted_session(
        service: &SessionService,
        session_id: SessionId,
        turn_id: TurnId,
    ) {
        let repository = service.store.session_repo();
        assert_eq!(
            repository
                .fresh_running_turn_for_session(session_id)
                .await
                .expect("active turn"),
            Some(turn_id)
        );
        let target = repository
            .captured_running_terminal_target(session_id)
            .await
            .expect("capture terminal target")
            .expect("running terminal target");
        assert!(
            repository
                .terminalize_captured_running_session_with_protocol_event(
                    session_id,
                    &test_terminal_event(session_id, TurnTerminalOutcome::Completed),
                    target,
                )
                .await
                .expect("complete admitted session")
        );
    }

    fn test_terminal_event(session_id: SessionId, outcome: TurnTerminalOutcome) -> RunEvent {
        RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(DurableTurnTerminal {
                outcome,
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        }
    }

    async fn assert_cancelled_admission(
        service: &SessionService,
        session_id: SessionId,
        admission_id: AdmissionId,
        turn_id: TurnId,
    ) {
        let repository = service.store.session_repo();
        assert_eq!(
            repository
                .get_session(session_id)
                .await
                .expect("cancelled session")
                .status,
            SessionStatus::Cancelled
        );
        assert!(matches!(
            repository
                .admitted_run_state(session_id, admission_id, turn_id)
                .await
                .expect("admission status"),
            crate::storage::session_repo::AdmittedRunState::Terminal(DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Interrupted { .. },
                ..
            })
        ));
        assert!(matches!(
            repository
                .renew_admitted_run_lease(session_id, admission_id, turn_id)
                .await
                .expect("terminal heartbeat"),
            crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome::Terminal(terminal)
                if terminal.session_status() == SessionStatus::Cancelled
        ));
        assert_eq!(
            repository
                .durable_terminal_for_turn(session_id, turn_id)
                .await
                .expect("protocol terminal status")
                .map(|terminal| terminal.session_status()),
            Some(SessionStatus::Cancelled)
        );
        assert!(
            repository
                .release_stopped_run_admission(session_id, admission_id)
                .await
                .expect("release stopped admission")
        );
        assert!(
            !repository
                .has_fresh_run_admission(session_id)
                .await
                .expect("released admission")
        );
    }

    async fn create_nested_agent_tree(
        service: &SessionService,
        workspace: &Workspace,
    ) -> (
        SessionContext,
        SessionContext,
        SessionContext,
        SessionContext,
    ) {
        let root = create_session(service, workspace).await;
        let middle = create_session(service, workspace).await;
        let leaf = create_session(service, workspace).await;
        let sibling = create_session(service, workspace).await;
        let repository = service.store.session_repo();
        repository
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                middle.session.id,
                "/root/middle",
                "middle",
            )
            .await
            .expect("middle edge");
        repository
            .insert_session_spawn_edge(
                root.session.id,
                middle.session.id,
                leaf.session.id,
                "/root/middle/leaf",
                "leaf",
            )
            .await
            .expect("leaf edge");
        repository
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                sibling.session.id,
                "/root/sibling",
                "sibling",
            )
            .await
            .expect("sibling edge");
        (root, middle, leaf, sibling)
    }

    #[tokio::test]
    async fn resume_rejects_a_session_from_another_workspace_project() {
        let (service, first, second) = service_fixture().await;
        let session = create_session(&service, &first).await;

        let error = service
            .resolve_session_for_workspace(&SessionSelector::ById(session.session.id), &second)
            .await
            .expect_err("foreign workspace must fail");

        assert!(error.to_string().contains("belongs to project"));
        assert!(error.to_string().contains("reopen its workspace"));
    }

    #[tokio::test]
    async fn resume_rejects_a_different_authority_within_the_same_project() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        let nested = workspace.root.join("nested");
        std::fs::create_dir_all(&nested).expect("nested authority");
        let mut mismatched = workspace.clone();
        mismatched.cwd = nested.clone();
        mismatched.path_policy.workspace_root = nested;

        let error = service
            .resolve_session_for_workspace(&SessionSelector::ById(session.session.id), &mismatched)
            .await
            .expect_err("same-project authority mismatch must fail closed");

        assert!(error.to_string().contains("workspace directory"));
        assert!(error.to_string().contains("reopen the session workspace"));
    }

    #[tokio::test]
    async fn new_session_rejects_a_request_cwd_outside_the_workspace_authority() {
        let (service, workspace, other) = service_fixture().await;

        let error = service
            .start_or_resume(
                SessionStartRequest {
                    selector: SessionSelector::New,
                    title: Some("mismatched cwd".to_string()),
                    cwd: other.cwd,
                    model: "model".to_string(),
                    base_url: "http://localhost:1234".to_string(),
                    access_mode: AccessMode::Default,
                },
                workspace.clone(),
            )
            .await
            .expect_err("mismatched cwd must fail before session creation");

        assert!(error.to_string().contains("outside stored project root"));
        assert!(
            service
                .list_sessions(workspace.project_id, 10)
                .await
                .expect("sessions")
                .is_empty(),
            "rejected request must not leave a session row"
        );
    }

    #[tokio::test]
    async fn settings_normalize_equivalent_cwd_and_reject_cross_project_cwd() {
        let (service, workspace, other) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        let child = workspace.root.join("child");
        fs::create_dir_all(&child).expect("child");
        let equivalent = child.join("..");

        let updated = service
            .update_session_settings(
                session.session.id,
                SessionSettingsPatch {
                    cwd: Some(equivalent),
                    ..Default::default()
                },
            )
            .await
            .expect("normalize equivalent cwd");
        assert_eq!(updated.session.cwd, workspace.root);

        let error = service
            .update_session_settings(
                session.session.id,
                SessionSettingsPatch {
                    cwd: Some(other.root),
                    ..Default::default()
                },
            )
            .await
            .expect_err("cross-project cwd must fail");
        assert!(error.to_string().contains("outside stored project root"));
        assert_eq!(
            service
                .get_session(session.session.id)
                .await
                .expect("preserved session")
                .cwd,
            workspace.root
        );
    }

    #[tokio::test]
    async fn session_projection_normalizes_a_legacy_equivalent_cwd() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        let child = workspace.root.join("legacy-child");
        fs::create_dir_all(&child).expect("legacy child");
        let legacy_equivalent = child.join("..");
        service
            .store
            .session_repo()
            .update_session_settings(
                session.session.id,
                &SessionSettingsPatch {
                    cwd: Some(legacy_equivalent.clone()),
                    ..Default::default()
                },
            )
            .await
            .expect("inject legacy cwd");
        assert_eq!(
            service
                .store
                .session_repo()
                .get_session(session.session.id)
                .await
                .expect("raw legacy session")
                .cwd,
            legacy_equivalent
        );

        let projected = service
            .get_session(session.session.id)
            .await
            .expect("normalized projection");
        assert_eq!(projected.cwd, workspace.root);
        let canonical = service
            .canonical_latest_session_snapshot(session.session.id, 10, 10)
            .await
            .expect("normalized canonical projection");
        assert_eq!(canonical.read.session.cwd, workspace.root);
        let resumed = service
            .resolve_session_for_workspace(&SessionSelector::ById(session.session.id), &workspace)
            .await
            .expect("legacy resume")
            .expect("session");
        assert_eq!(resumed.cwd, workspace.root);
    }

    #[test]
    fn git_session_cwd_rejects_a_nested_repository_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root =
            Utf8PathBuf::from_path_buf(temp.path().join("aaa")).expect("utf8 project root");
        let nested = project_root.join("nested");
        fs::create_dir_all(project_root.join(".git")).expect("project git marker");
        fs::create_dir_all(nested.join(".git")).expect("nested git marker");
        let workspace = WorkspaceDiscovery::discover(&project_root, &ResolvedConfig::default())
            .expect("outer workspace");

        let error =
            normalize_session_cwd_for_project(&project_root, workspace.project_id, "git", &nested)
                .expect_err("nested repository must not inherit the outer project identity");

        assert!(error.to_string().contains("resolves to project"));
        assert!(
            error
                .to_string()
                .contains(&workspace.project_id.to_string())
        );
    }

    #[tokio::test]
    async fn active_run_blocks_session_and_project_delete() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        let control = RunControl::new();
        let _lease = service
            .store
            .active_runs()
            .try_start(session.session.id, control)
            .expect("active run");

        assert!(service.delete_session(session.session.id).await.is_err());
        assert!(service.delete_project(workspace.project_id).await.is_err());
        assert!(service.get_session(session.session.id).await.is_ok());
    }

    #[tokio::test]
    async fn protocol_history_queues_steer_for_a_run_owned_by_another_process() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        let turn_id = TurnId::new();
        let user_turn = UserTurn {
            turn_id,
            items: vec![UserInputItem::Text {
                text: "start".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
        };
        let admission_id = service
            .store
            .session_repo()
            .admit_session_turn(session.session.id, turn_id)
            .await
            .expect("admit run")
            .expect("run admitted")
            .admission_id;
        service
            .store_user_turn_with_protocol_bundle(&session, admission_id, &user_turn, turn_id, 0)
            .await
            .expect("store user");
        assert_eq!(
            service
                .store
                .session_repo()
                .get_session(session.session.id)
                .await
                .expect("running session")
                .status,
            SessionStatus::Running
        );
        let steer = SteerTurn {
            expected_turn_id: turn_id,
            items: vec![UserInputItem::Text {
                text: "steer from another process".to_string(),
            }],
            additional_context: Default::default(),
            client_user_message_id: Some("cross-process".to_string()),
        };

        service
            .store_active_turn_steer(session.session.id, &steer)
            .await
            .expect("queue steer");
        let history = service
            .canonical_history_items(session.session.id)
            .await
            .expect("history");

        assert!(
            history
                .iter()
                .all(|item| !matches!(&item.payload, HistoryItemPayload::SteerTurn { .. }))
        );
        let snapshot = service
            .canonical_latest_session_snapshot(session.session.id, 10, 10)
            .await
            .expect("pending-input snapshot");
        let [pending] = snapshot.read.pending_turn_inputs.as_slice() else {
            panic!("one queued steer must remain separate from canonical history");
        };
        assert_eq!(pending.turn_id, turn_id);
        assert_eq!(pending.text, "steer from another process");
        assert_eq!(
            pending.client_user_message_id.as_deref(),
            Some("cross-process")
        );
    }

    #[tokio::test]
    async fn active_archive_is_rejected_while_projection_and_unarchive_remain_consistent() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        service
            .set_session_archived(session.session.id, true)
            .await
            .expect("archive idle session");
        service
            .store
            .session_repo()
            .admit_session_turn(session.session.id, TurnId::new())
            .await
            .expect("admit run")
            .expect("run owner");

        let visible = service
            .loaded_sessions(workspace.project_id, 20, true)
            .await
            .expect("loaded projection");
        let summary = visible
            .sessions
            .iter()
            .find(|summary| summary.session.id == session.session.id)
            .expect("active archived summary");
        assert_eq!(summary.loaded_status, LoadedSessionStatus::Active);
        assert!(summary.archived);
        let searched = service
            .search_loaded_sessions(workspace.project_id, "test", 20, true)
            .await
            .expect("atomic search projection");
        let searched_summary = searched
            .sessions
            .iter()
            .find(|summary| summary.session.id == session.session.id)
            .expect("searched archived summary");
        assert!(searched_summary.archived);
        assert!(
            service
                .loaded_sessions(workspace.project_id, 20, false)
                .await
                .expect("filtered projection")
                .sessions
                .iter()
                .all(|summary| summary.session.id != session.session.id)
        );

        let error = service
            .set_session_archived(session.session.id, true)
            .await
            .expect_err("active session cannot be archived");
        assert!(error.to_string().contains("active"));

        service
            .set_session_archived(session.session.id, false)
            .await
            .expect("active archived session can be recovered");
        let archived = service
            .store
            .session_repo()
            .session_is_archived(session.session.id)
            .await
            .expect("archive state");
        assert!(!archived);
    }

    #[tokio::test]
    async fn startup_recovery_preserves_hidden_child_with_a_fresh_owner() {
        let (service, workspace, _) = service_fixture().await;
        let root = create_session(&service, &workspace).await;
        let child = create_session(&service, &workspace).await;
        service
            .store
            .session_repo()
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                child.session.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let (child_admission, child_turn) = admit_session_turn(&service, child.session.id).await;
        let _child_owner_lease = service
            .store
            .try_acquire_run_process_lease(child.session.id)
            .expect("child owner process lease");

        let recovery_fence = service
            .store
            .session_repo()
            .running_session_recovery_fence()
            .await
            .expect("recovery fence")
            .expect("running child fence");
        let recovery_candidates = service
            .store
            .session_repo()
            .running_session_recovery_page(
                None,
                recovery_fence,
                crate::session::MAX_SESSION_PAGE_LIMIT,
            )
            .await
            .expect("recovery candidates");
        assert!(
            recovery_candidates
                .iter()
                .any(|candidate| candidate.session.id == child.session.id),
            "child sessions hidden from normal discovery must remain visible to recovery"
        );
        assert_eq!(
            service
                .mark_stale_running_sessions("stale child recovery")
                .await
                .expect("stale recovery"),
            0
        );

        assert_eq!(
            service
                .get_session(root.session.id)
                .await
                .expect("root session")
                .status,
            SessionStatus::Idle
        );
        assert_eq!(
            service
                .get_session(child.session.id)
                .await
                .expect("child session")
                .status,
            SessionStatus::Running
        );
        assert_eq!(
            service
                .store
                .session_repo()
                .admitted_run_status(child.session.id, child_admission, child_turn)
                .await
                .expect("fresh child admission"),
            Some(SessionStatus::Running)
        );
    }

    #[tokio::test]
    async fn startup_recovery_fails_hidden_child_without_an_owner() {
        let (service, workspace, _) = service_fixture().await;
        let root = create_session(&service, &workspace).await;
        let child = create_session(&service, &workspace).await;
        service
            .store
            .session_repo()
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                child.session.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let _ = admit_session_turn(&service, child.session.id).await;

        assert_eq!(
            service
                .mark_stale_running_sessions("stale child recovery")
                .await
                .expect("stale recovery"),
            1
        );
        assert_failed_recovery(&service, child.session.id, "stale child recovery").await;
    }

    #[tokio::test]
    async fn startup_recovery_rejects_a_running_session_without_an_active_turn() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        service
            .store
            .session_repo()
            .inject_raw_runtime_state_for_corruption_test(
                session.session.id,
                "running",
                None,
                None,
                None,
            )
            .expect("create impossible running session fixture");

        let error = service
            .mark_stale_running_sessions("must not invent a turn")
            .await
            .expect_err("recovery must fail closed without a canonical turn");

        assert!(error.to_string().contains("durable run admission"));
        assert!(
            service.get_session(session.session.id).await.is_err(),
            "ordinary reads must reject the unchanged invalid owner state"
        );
        assert!(
            service
                .store
                .protocol_event_store()
                .list_turn_items_for_session(session.session.id)
                .expect("turn items")
                .is_empty(),
            "fail-closed recovery must not persist a terminal under an invented turn identity"
        );
    }

    #[tokio::test]
    async fn startup_recovery_does_not_infer_a_turn_from_canonical_history() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        let (admission_id, turn_id) = admit_session_turn(&service, session.session.id).await;
        terminalize_admitted_session(&service, session.session.id, turn_id).await;
        assert!(
            service
                .store
                .session_repo()
                .release_stopped_run_admission(session.session.id, admission_id)
                .await
                .expect("release completed admission")
        );
        service
            .store
            .session_repo()
            .inject_raw_runtime_state_for_corruption_test(
                session.session.id,
                "running",
                None,
                None,
                None,
            )
            .expect("create impossible historical running fixture");

        let error = service
            .mark_stale_running_sessions("must not infer a historical turn")
            .await
            .expect_err("recovery must fail closed without an active turn");

        assert!(error.to_string().contains("durable run admission"));
        assert!(
            service.get_session(session.session.id).await.is_err(),
            "ordinary reads must reject the unchanged invalid owner state"
        );
        let terminal_items = service
            .store
            .protocol_event_store()
            .list_turn_items_for_session(session.session.id)
            .expect("canonical turn items")
            .into_iter()
            .filter(|item| matches!(item.payload, TurnItemPayload::Terminal { .. }))
            .count();
        assert_eq!(
            terminal_items, 1,
            "recovery must not append another terminal"
        );
    }

    #[tokio::test]
    async fn startup_recovery_clears_a_crashed_fresh_admission_for_immediate_reuse() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        let (crashed_admission, _turn_id) = admit_session_turn(&service, session.session.id).await;
        assert!(
            service
                .store
                .session_repo()
                .has_fresh_run_admission(session.session.id)
                .await
                .expect("fresh crashed admission")
        );

        assert_eq!(
            service
                .mark_stale_running_sessions("recover crashed fresh admission")
                .await
                .expect("startup recovery"),
            1
        );
        assert_failed_recovery(
            &service,
            session.session.id,
            "recover crashed fresh admission",
        )
        .await;
        assert!(
            !service
                .store
                .session_repo()
                .has_fresh_run_admission(session.session.id)
                .await
                .expect("cleared crashed admission")
        );
        let replacement = service
            .store
            .session_repo()
            .admit_session_turn(session.session.id, TurnId::new())
            .await
            .expect("replacement admission")
            .expect("recovered session is immediately reusable")
            .admission_id;
        assert_ne!(replacement, crashed_admission);
    }

    #[tokio::test]
    async fn startup_recovery_uses_the_durable_turn_after_its_lease_expires() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        let turn_id = TurnId::new();
        service
            .store
            .session_repo()
            .admit_session_turn_at(session.session.id, turn_id, 0, 1)
            .await
            .expect("admit expired run fixture")
            .expect("expired run admitted");
        assert!(
            !service
                .store
                .session_repo()
                .has_fresh_run_admission(session.session.id)
                .await
                .expect("expired admission state")
        );

        assert_eq!(
            service
                .mark_stale_running_sessions("recover expired durable turn")
                .await
                .expect("startup recovery"),
            1
        );
        assert_failed_recovery(&service, session.session.id, "recover expired durable turn").await;
        assert!(
            service
                .store
                .session_repo()
                .durable_terminal_for_turn(session.session.id, turn_id)
                .await
                .expect("expired turn terminal lookup")
                .is_some(),
            "startup recovery must terminalize the admitted turn identity"
        );
    }

    #[tokio::test]
    async fn startup_recovery_streams_more_than_one_bounded_page_without_skipping() {
        let (service, workspace, _) = service_fixture().await;
        let session_count = RUNNING_SESSION_RECOVERY_PAGE_SIZE + 1;
        let mut session_ids = Vec::with_capacity(session_count);
        for _ in 0..session_count {
            let session = create_session(&service, &workspace).await;
            let _ = admit_session_turn(&service, session.session.id).await;
            session_ids.push(session.session.id);
        }

        assert_eq!(
            service
                .mark_stale_running_sessions("bounded startup recovery")
                .await
                .expect("recover every bounded page"),
            session_count
        );
        assert!(
            service
                .store
                .session_repo()
                .running_session_recovery_fence()
                .await
                .expect("post-recovery fence")
                .is_none()
        );
        for session_id in session_ids {
            assert_eq!(
                service
                    .get_session(session_id)
                    .await
                    .expect("recovered session")
                    .status,
                SessionStatus::Failed
            );
        }
    }

    #[tokio::test]
    async fn session_list_limits_are_enforced_below_every_public_query_surface() {
        let (service, workspace, _) = service_fixture().await;
        create_session(&service, &workspace).await;

        for limit in [0, crate::session::MAX_SESSION_PAGE_LIMIT + 1] {
            assert!(
                service
                    .list_sessions(workspace.project_id, limit)
                    .await
                    .is_err()
            );
            assert!(
                service
                    .list_sessions_with_archived(workspace.project_id, limit, true)
                    .await
                    .is_err()
            );
            assert!(service.list_recent_sessions(limit).await.is_err());
            assert!(
                service
                    .search_sessions(workspace.project_id, "test", limit, true)
                    .await
                    .is_err()
            );
            assert!(
                service
                    .loaded_sessions(workspace.project_id, limit, true)
                    .await
                    .is_err()
            );
            assert!(
                service
                    .search_loaded_sessions(workspace.project_id, "test", limit, true)
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn startup_recovery_preserves_a_run_owned_by_another_process() {
        let (owner, recovery, workspace) = cross_process_service_fixture().await;
        let session = create_session(&owner, &workspace).await;
        let _ = admit_session_turn(&owner, session.session.id).await;
        let _owner_lease = owner
            .store
            .try_acquire_run_process_lease(session.session.id)
            .expect("owner process lease");

        assert_eq!(
            recovery
                .mark_stale_running_sessions("must not stop another process")
                .await
                .expect("startup recovery"),
            0
        );
        assert_eq!(
            recovery
                .get_session(session.session.id)
                .await
                .expect("process-owned session")
                .status,
            SessionStatus::Running
        );
    }

    #[tokio::test]
    async fn startup_recovery_does_not_cascade_from_an_unowned_parent_into_a_live_child() {
        let (owner, recovery, workspace) = cross_process_service_fixture().await;
        let root = create_session(&owner, &workspace).await;
        let child = create_session(&owner, &workspace).await;
        owner
            .store
            .session_repo()
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                child.session.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        for session_id in [root.session.id, child.session.id] {
            let _ = admit_session_turn(&owner, session_id).await;
        }
        let _child_owner_lease = owner
            .store
            .try_acquire_run_process_lease(child.session.id)
            .expect("child owner process lease");

        assert_eq!(
            recovery
                .mark_stale_running_sessions("recover only unowned sessions")
                .await
                .expect("startup recovery"),
            1
        );
        assert_failed_recovery(&recovery, root.session.id, "recover only unowned sessions").await;
        assert_eq!(
            recovery
                .get_session(child.session.id)
                .await
                .expect("live child session")
                .status,
            SessionStatus::Running
        );
    }

    #[tokio::test]
    async fn startup_root_failure_preserves_independent_nested_trigger() {
        let (service, workspace, _) = service_fixture().await;
        let (root, middle, leaf, _sibling) = create_nested_agent_tree(&service, &workspace).await;
        let _ = admit_session_turn(&service, root.session.id).await;
        let _ = admit_session_turn(&service, middle.session.id).await;
        let trigger = service
            .store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                leaf.session.id,
                InterAgentCommunication {
                    author: "/root/middle".to_string(),
                    recipient: "/root/middle/leaf".to_string(),
                    content: "Message Type: NEW_TASK\nTask name: /root/middle/leaf\nSender: /root/middle\nPayload:\nfinish after restart".to_string(),
                    trigger_turn: true,
                },
                false,
            )
            .expect("pending nested trigger");

        assert_eq!(
            service
                .mark_stale_running_sessions("nested startup recovery")
                .await
                .expect("recover root and middle"),
            2
        );
        assert_eq!(
            service
                .get_session(leaf.session.id)
                .await
                .expect("idle leaf")
                .status,
            SessionStatus::Idle
        );
        assert_eq!(
            service
                .store
                .session_repo()
                .pending_agent_trigger_history_item_id(leaf.session.id)
                .expect("leaf trigger retained"),
            Some(trigger.history_item_id),
            "a recovered root failure must not fan out into an implicit tree Stop"
        );
        assert_eq!(
            service
                .get_session(middle.session.id)
                .await
                .expect("recovered middle")
                .status,
            SessionStatus::Failed,
            "the independently recovered middle remains forensic history"
        );
        let requests = service
            .store
            .session_repo()
            .list_pending_owner_resume_requests(middle.session.id)
            .expect("middle owner-resume requests");
        assert!(
            requests.is_empty(),
            "crash recovery does not auto-resume an owner before a child result arrives"
        );
        let middle_deferred = service
            .store
            .session_repo()
            .pending_deferred_completion(middle.session.id)
            .expect("middle deferred query")
            .expect("middle crash recovery receipt");
        assert_eq!(
            middle_deferred.kind,
            crate::storage::session_repo::DeferredAgentCompletionKind::CrashFailed
        );
        assert!(
            service
                .store
                .session_repo()
                .list_pending_owner_resume_requests(root.session.id)
                .expect("root owner-resume requests")
                .is_empty(),
            "root is never an OwnerResume target"
        );
        let root_history = service
            .store
            .protocol_event_store()
            .list_history_items_for_session(root.session.id)
            .expect("root history");
        assert!(root_history.iter().all(|item| {
            !matches!(
                &item.payload,
                HistoryItemPayload::InterAgentCommunication { communication }
                    if communication.author == "/root/middle"
                        && communication.content.contains("Message Type: FINAL_ANSWER")
            )
        }));
    }

    #[tokio::test]
    async fn startup_recovery_never_projects_explicit_followup_as_owner_resume() {
        let (service, workspace, _) = service_fixture().await;
        let (root, middle, leaf, _sibling) = create_nested_agent_tree(&service, &workspace).await;
        let _ = admit_session_turn(&service, root.session.id).await;
        let _ = admit_session_turn(&service, middle.session.id).await;
        let middle_process_lease = service
            .store
            .try_acquire_run_process_lease(middle.session.id)
            .expect("live middle process lease");
        service
            .store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                leaf.session.id,
                InterAgentCommunication {
                    author: "/root/middle".to_string(),
                    recipient: "/root/middle/leaf".to_string(),
                    content: "Message Type: NEW_TASK\nTask name: /root/middle/leaf\nSender: /root/middle\nPayload:\nremain behind live middle".to_string(),
                    trigger_turn: true,
                },
                false,
            )
            .expect("pending nested trigger");

        assert_eq!(
            service
                .mark_stale_running_sessions("recover root only")
                .await
                .expect("root recovery"),
            1
        );
        assert!(
            service
                .store
                .session_repo()
                .list_pending_owner_resume_requests(middle.session.id)
                .expect("no cross-live-boundary request")
                .is_empty()
        );
        assert_eq!(
            service
                .get_session(middle.session.id)
                .await
                .expect("live middle")
                .status,
            SessionStatus::Running
        );

        drop(middle_process_lease);
        assert_eq!(
            service
                .mark_stale_running_sessions("later recover middle")
                .await
                .expect("middle recovery"),
            1
        );
        assert!(
            service
                .store
                .session_repo()
                .list_pending_owner_resume_requests(middle.session.id)
                .expect("no explicit-followup OwnerResume after middle recovery")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn recovering_owner_defers_to_its_exact_pending_child_without_ancestor_wakes() {
        let (service, workspace, _) = service_fixture().await;
        let root = create_session(&service, &workspace).await;
        let outer = create_session(&service, &workspace).await;
        let recovering = create_session(&service, &workspace).await;
        let target = create_session(&service, &workspace).await;
        let repository = service.store.session_repo();
        for (parent, child, path, task) in [
            (root.session.id, outer.session.id, "/root/outer", "outer"),
            (
                outer.session.id,
                recovering.session.id,
                "/root/outer/recovering",
                "recovering",
            ),
            (
                recovering.session.id,
                target.session.id,
                "/root/outer/recovering/target",
                "target",
            ),
        ] {
            repository
                .insert_session_spawn_edge(root.session.id, parent, child, path, task)
                .await
                .expect("nested recovery edge");
        }
        let _ = admit_session_turn(&service, recovering.session.id).await;
        let trigger = repository
            .append_inter_agent_communication_with_protocol_bundle(
                target.session.id,
                InterAgentCommunication {
                    author: "/root/outer/recovering".to_string(),
                    recipient: "/root/outer/recovering/target".to_string(),
                    content: "Message Type: NEW_TASK\nTask name: /root/outer/recovering/target\nSender: /root/outer/recovering\nPayload:\nresume through idle outer".to_string(),
                    trigger_turn: true,
                },
                false,
            )
            .expect("pending target trigger");
        assert!(
            repository
                .list_pending_owner_resume_requests(recovering.session.id)
                .expect("no request across live recovering owner")
                .is_empty()
        );
        assert!(
            repository
                .list_pending_owner_resume_requests(outer.session.id)
                .expect("no request above live recovering owner")
                .is_empty()
        );

        assert_eq!(
            service
                .mark_stale_running_sessions("recover nested owner")
                .await
                .expect("nested owner recovery"),
            1
        );
        assert!(
            repository
                .list_pending_owner_resume_requests(recovering.session.id)
                .expect("recovering owner request")
                .is_empty(),
            "a pending child trigger must not wake its parent"
        );
        assert!(
            repository
                .list_pending_owner_resume_requests(outer.session.id)
                .expect("idle outer owner request")
                .is_empty(),
            "a crash-deferred owner has no FINAL to wake its parent yet"
        );
        assert!(
            repository
                .list_pending_owner_resume_requests(root.session.id)
                .expect("root request")
                .is_empty()
        );
        assert_eq!(
            repository
                .pending_agent_trigger_history_item_id(target.session.id)
                .expect("exact target trigger"),
            Some(trigger.history_item_id)
        );
        let deferred = repository
            .pending_deferred_completion(recovering.session.id)
            .expect("recovering deferred query")
            .expect("recovering crash is deferred");
        assert_eq!(deferred.parent_session_id, outer.session.id);
        assert_eq!(
            deferred.kind,
            crate::storage::session_repo::DeferredAgentCompletionKind::CrashFailed
        );
    }

    #[tokio::test]
    async fn startup_child_failure_hands_off_without_auto_resuming_idle_parent() {
        let (service, workspace, _) = service_fixture().await;
        let (_root, middle, leaf, _sibling) = create_nested_agent_tree(&service, &workspace).await;
        let (_, leaf_turn_id) = admit_session_turn(&service, leaf.session.id).await;

        assert_eq!(
            service
                .mark_stale_running_sessions("recover running leaf")
                .await
                .expect("leaf recovery"),
            1
        );
        let handoff = service
            .store
            .session_repo()
            .agent_completion_handoff(leaf.session.id, leaf_turn_id)
            .expect("leaf handoff lookup")
            .expect("failed leaf FINAL");
        assert_eq!(handoff.parent_session_id, middle.session.id);
        assert!(
            service
                .store
                .session_repo()
                .schedulable_owner_resume_request_id(middle.session.id)
                .expect("current middle continuation")
                .is_none(),
            "an idle parent consumes the retained FINAL only after explicit follow-up"
        );
        let requests = service
            .store
            .session_repo()
            .list_pending_owner_resume_requests(middle.session.id)
            .expect("middle resume after leaf failure");
        assert!(
            requests.is_empty(),
            "a normal failed child handoff must not synthesize OwnerResume work"
        );
    }

    #[tokio::test]
    async fn cross_process_root_cancel_terminalizes_the_entire_agent_tree() {
        let (owner, canceller, workspace) = cross_process_service_fixture().await;
        let (root, middle, leaf, sibling) = create_nested_agent_tree(&owner, &workspace).await;
        let (root_admission, root_turn) = admit_session_turn(&owner, root.session.id).await;
        let (middle_admission, middle_turn) = admit_session_turn(&owner, middle.session.id).await;
        let (leaf_admission, leaf_turn) = admit_session_turn(&owner, leaf.session.id).await;
        let (sibling_admission, sibling_turn) =
            admit_session_turn(&owner, sibling.session.id).await;
        assert!(
            [
                root.session.id,
                middle.session.id,
                leaf.session.id,
                sibling.session.id
            ]
            .into_iter()
            .all(|session_id| !canceller.store.active_runs().is_active(session_id)),
            "the cancelling process must not depend on the owner's in-memory run registry"
        );

        assert!(
            canceller
                .cancel_running_session_tree(root.session.id, TurnInterruptionCause::UserStop,)
                .await
                .expect("explicit root tree cancellation")
        );

        assert_cancelled_admission(&owner, root.session.id, root_admission, root_turn).await;
        assert_cancelled_admission(&owner, middle.session.id, middle_admission, middle_turn).await;
        assert_cancelled_admission(&owner, leaf.session.id, leaf_admission, leaf_turn).await;
        assert_cancelled_admission(&owner, sibling.session.id, sibling_admission, sibling_turn)
            .await;
    }

    #[tokio::test]
    async fn exact_turn_interrupt_rejects_stale_a_after_replacement_b_without_touching_b() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        let (admission_a, turn_a) = admit_session_turn(&service, session.session.id).await;
        terminalize_admitted_session(&service, session.session.id, turn_a).await;
        assert!(
            service
                .store
                .session_repo()
                .release_stopped_run_admission(session.session.id, admission_a)
                .await
                .expect("release completed turn A admission")
        );
        let (_, turn_b) = admit_session_turn(&service, session.session.id).await;

        assert!(
            !service
                .cancel_running_session_turn(
                    session.session.id,
                    turn_a,
                    TurnInterruptionCause::AgentInterrupted,
                )
                .await
                .expect("stale exact interrupt")
        );
        let repository = service.store.session_repo();
        assert_eq!(
            repository
                .fresh_running_turn_for_session(session.session.id)
                .await
                .expect("replacement turn"),
            Some(turn_b)
        );
        assert!(
            repository
                .durable_terminal_for_turn(session.session.id, turn_b)
                .await
                .expect("replacement terminal lookup")
                .is_none()
        );
    }

    #[tokio::test]
    async fn unloaded_exact_child_interrupt_terminalizes_only_the_captured_child_turn() {
        let (owner, interrupter, workspace) = cross_process_service_fixture().await;
        let root = create_session(&owner, &workspace).await;
        let child_a = create_session(&owner, &workspace).await;
        let child_b = create_session(&owner, &workspace).await;
        for (child, path) in [(&child_a, "/root/child_a"), (&child_b, "/root/child_b")] {
            owner
                .store
                .session_repo()
                .insert_session_spawn_edge(
                    root.session.id,
                    root.session.id,
                    child.session.id,
                    path,
                    path.trim_start_matches("/root/"),
                )
                .await
                .expect("spawn edge");
        }
        let (_, root_turn) = admit_session_turn(&owner, root.session.id).await;
        let (child_a_admission, child_a_turn) =
            admit_session_turn(&owner, child_a.session.id).await;
        let (_, child_b_turn) = admit_session_turn(&owner, child_b.session.id).await;

        assert!(
            interrupter
                .cancel_running_session_turn(
                    child_a.session.id,
                    child_a_turn,
                    TurnInterruptionCause::AgentInterrupted,
                )
                .await
                .expect("unloaded exact child interrupt")
        );
        let repository = owner.store.session_repo();
        let terminal = repository
            .durable_terminal_for_turn(child_a.session.id, child_a_turn)
            .await
            .expect("child terminal lookup")
            .expect("exact child terminal");
        assert_eq!(
            terminal.outcome,
            TurnTerminalOutcome::Interrupted {
                cause: TurnInterruptionCause::AgentInterrupted,
            }
        );
        assert_eq!(
            repository
                .fresh_running_turn_for_session(root.session.id)
                .await
                .expect("root remains running"),
            Some(root_turn)
        );
        assert_eq!(
            repository
                .fresh_running_turn_for_session(child_b.session.id)
                .await
                .expect("sibling remains running"),
            Some(child_b_turn)
        );
        assert_cancelled_admission(&owner, child_a.session.id, child_a_admission, child_a_turn)
            .await;
    }

    #[tokio::test]
    async fn cross_process_captured_stop_projects_exact_terminal_to_live_hub_and_harness() {
        let (owner, canceller, workspace) = cross_process_service_fixture().await;
        let session = create_session(&owner, &workspace).await;
        let (admission_id, turn_id) = admit_session_turn(&owner, session.session.id).await;
        let recorder = NativeHarnessRecorder::start_harness_only_for_turn(
            &owner.store,
            Some(session.session.id),
            workspace.root.clone(),
            turn_id,
        )
        .expect("mapped native harness");
        let run_id = recorder.run_id();
        let hub = SessionRuntimeEventHub::new(16);
        let projector = CanonicalRuntimeEventProjector::new(
            canceller.store.protocol_event_store(),
            canceller.store.harness_run_store(),
            hub.publisher(),
        );
        let canceller = canceller.with_runtime_event_projector(projector);
        let mut subscription = hub.subscribe(session.session.id);

        assert!(
            canceller
                .cancel_running_session(session.session.id)
                .await
                .expect("cross-process captured Stop")
        );
        assert_cancelled_admission(&owner, session.session.id, admission_id, turn_id).await;

        let canonical_terminal = owner
            .store
            .protocol_event_store()
            .list_runtime_events(session.session.id, turn_id)
            .expect("canonical runtime events")
            .into_iter()
            .find(|event| matches!(event.msg, RuntimeEventMsg::TurnTerminal { .. }))
            .expect("canonical terminal");
        assert!(matches!(
            &canonical_terminal.msg,
            RuntimeEventMsg::TurnTerminal { terminal }
                if matches!(
                    &terminal.outcome,
                    TurnTerminalOutcome::Interrupted {
                        cause: TurnInterruptionCause::UserStop,
                    }
                )
        ));
        let published =
            tokio::time::timeout(std::time::Duration::from_secs(1), subscription.recv())
                .await
                .expect("live terminal publication")
                .expect("published runtime event");
        assert_eq!(
            serde_json::to_value(&published).expect("published runtime JSON"),
            serde_json::to_value(&canonical_terminal).expect("canonical runtime JSON"),
            "the live hub must receive the exact canonical event rather than a reconstructed terminal"
        );

        let run = owner
            .store
            .harness_run_store()
            .get_run(run_id)
            .expect("read projected harness run")
            .expect("projected harness run");
        assert_eq!(run.status, HarnessRunStatus::Blocked);
        assert_eq!(run.completed_at_ms, Some(canonical_terminal.created_at_ms));
        assert_eq!(
            run.canonical_terminal_runtime_event_id,
            Some(canonical_terminal.id)
        );
        let terminal_events = owner
            .store
            .harness_event_store()
            .list_events(run_id)
            .expect("projected harness events")
            .into_iter()
            .filter(|event| event.kind == HarnessEventKind::RunTerminalized)
            .collect::<Vec<_>>();
        assert_eq!(terminal_events.len(), 1);
        let RuntimeEventMsg::TurnTerminal { terminal } = &canonical_terminal.msg else {
            panic!("canonical event must be terminal");
        };
        assert_eq!(
            terminal_events[0].payload,
            HarnessEventPayload::generic(
                serde_json::to_value(RunEvent::TurnTerminal {
                    session_id: session.session.id,
                    terminal: terminal.clone(),
                })
                .expect("canonical terminal harness payload")
            )
        );

        canceller
            .runtime_event_projector
            .as_ref()
            .expect("configured projector")
            .project_after_cursor(session.session.id, None)
            .expect("idempotent canonical replay");
        assert_eq!(
            owner
                .store
                .harness_event_store()
                .list_events(run_id)
                .expect("replayed harness events")
                .into_iter()
                .filter(|event| event.kind == HarnessEventKind::RunTerminalized)
                .count(),
            1,
            "replaying the canonical outbox must not duplicate terminal evidence"
        );
    }

    #[tokio::test]
    async fn tree_stop_fence_does_not_cancel_a_post_boundary_replacement_turn() {
        let (owner, canceller, workspace) = cross_process_service_fixture().await;
        let root = create_session(&owner, &workspace).await;
        let child = create_session(&owner, &workspace).await;
        owner
            .store
            .session_repo()
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                child.session.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let (_root_admission, root_turn) = admit_session_turn(&owner, root.session.id).await;
        terminalize_admitted_session(&owner, root.session.id, root_turn).await;
        let (old_admission, old_turn) = admit_session_turn(&owner, child.session.id).await;
        owner
            .store_user_turn_with_protocol_bundle(
                &child,
                old_admission,
                &UserTurn {
                    turn_id: old_turn,
                    items: vec![UserInputItem::Text {
                        text: "old child turn".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
                old_turn,
                0,
            )
            .await
            .expect("store old child turn");
        let old_target = owner
            .store
            .session_repo()
            .captured_running_terminal_target(child.session.id)
            .await
            .expect("capture old child target")
            .expect("old child target");

        let fence = canceller
            .store
            .session_repo()
            .record_agent_tree_stop_fence(root.session.id, TurnInterruptionCause::UserStop)
            .await
            .expect("record cross-process tree-stop fence")
            .expect("tree-stop fence");
        assert!(matches!(
            owner
                .store
                .session_repo()
                .renew_admitted_run_lease(child.session.id, old_admission, old_turn)
                .await
                .expect("renew pre-fence child turn"),
            crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome::StopFenced(
                TurnTerminalOutcome::Interrupted {
                    cause: TurnInterruptionCause::TreeStopped
                }
            )
        ));

        assert!(
            !owner
                .store
                .session_repo()
                .terminalize_captured_running_session_with_protocol_event(
                    child.session.id,
                    &test_terminal_event(child.session.id, TurnTerminalOutcome::Completed),
                    old_target,
                )
                .await
                .expect("reject late old-child success"),
            "the pre-fence turn cannot commit success after the tree Stop boundary"
        );
        assert!(
            owner
                .store
                .session_repo()
                .terminalize_captured_running_session_with_protocol_event(
                    child.session.id,
                    &test_terminal_event(
                        child.session.id,
                        TurnTerminalOutcome::Interrupted {
                            cause: TurnInterruptionCause::TreeStopped,
                        },
                    ),
                    old_target,
                )
                .await
                .expect("settle old child at its tree-stop boundary")
        );
        assert!(
            owner
                .store
                .session_repo()
                .release_stopped_run_admission(child.session.id, old_admission)
                .await
                .expect("release old child admission")
        );
        let (replacement_admission, replacement_turn) =
            admit_session_turn(&owner, child.session.id).await;
        owner
            .store_user_turn_with_protocol_bundle(
                &child,
                replacement_admission,
                &UserTurn {
                    turn_id: replacement_turn,
                    items: vec![UserInputItem::Text {
                        text: "post-fence replacement task".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
                replacement_turn,
                0,
            )
            .await
            .expect("store replacement child turn");
        let replacement_control = RunControl::new();
        let replacement_lease = owner
            .store
            .active_runs()
            .try_start(child.session.id, replacement_control.clone())
            .expect("register replacement child run");
        replacement_lease
            .set_turn_id(replacement_turn)
            .expect("bind replacement turn");
        assert!(matches!(
            owner
                .store
                .session_repo()
                .renew_admitted_run_lease(
                    child.session.id,
                    replacement_admission,
                    replacement_turn,
                )
                .await
                .expect("renew post-fence replacement"),
            crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome::Renewed
        ));

        assert!(
            !owner
                .fanout_agent_tree_stop_at_fence(root.session.id, fence)
                .await
                .expect("fan out original tree-stop boundary"),
            "the original boundary has no remaining pre-fence target"
        );
        assert_eq!(replacement_control.cause(), None);
        assert!(!replacement_control.is_cancelled());
        assert_eq!(
            owner
                .store
                .session_repo()
                .admitted_run_status(child.session.id, replacement_admission, replacement_turn,)
                .await
                .expect("replacement admission status"),
            Some(SessionStatus::Running)
        );
        assert!(
            owner
                .store
                .session_repo()
                .durable_terminal_for_turn(child.session.id, replacement_turn)
                .await
                .expect("replacement terminal lookup")
                .is_none()
        );
    }

    #[tokio::test]
    async fn observed_turn_stop_reuses_first_fence_without_extending_over_followup() {
        let (owner, delayed_service, workspace) = cross_process_service_fixture().await;
        let root = create_session(&owner, &workspace).await;
        let (old_admission, old_turn) = admit_session_turn(&owner, root.session.id).await;
        owner
            .store_user_turn_with_protocol_bundle(
                &root,
                old_admission,
                &UserTurn {
                    turn_id: old_turn,
                    items: vec![UserInputItem::Text {
                        text: "old root turn".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
                old_turn,
                0,
            )
            .await
            .expect("store old root turn");
        let old_target = owner
            .store
            .session_repo()
            .captured_running_terminal_target(root.session.id)
            .await
            .expect("capture old root target")
            .expect("old root target");
        assert!(
            owner
                .store
                .session_repo()
                .terminalize_captured_running_session_with_protocol_event(
                    root.session.id,
                    &test_terminal_event(
                        root.session.id,
                        TurnTerminalOutcome::Interrupted {
                            cause: TurnInterruptionCause::UserStop,
                        },
                    ),
                    old_target,
                )
                .await
                .expect("old root worker commits its Stop")
        );
        let first_fence = owner
            .store
            .session_repo()
            .record_agent_tree_stop_fence_for_observed_turn(
                root.session.id,
                TurnInterruptionCause::UserStop,
                old_turn,
            )
            .await
            .expect("read old-turn Stop fence")
            .expect("old-turn Stop fence");
        assert!(
            owner
                .store
                .session_repo()
                .release_stopped_run_admission(root.session.id, old_admission)
                .await
                .expect("release old stopped admission")
        );

        let (followup_admission, followup_turn) = admit_session_turn(&owner, root.session.id).await;
        owner
            .store_user_turn_with_protocol_bundle(
                &root,
                followup_admission,
                &UserTurn {
                    turn_id: followup_turn,
                    items: vec![UserInputItem::Text {
                        text: "post-fence explicit followup".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
                followup_turn,
                0,
            )
            .await
            .expect("store post-fence followup");
        let followup_control = RunControl::new();
        let followup_lease = delayed_service
            .store
            .active_runs()
            .try_start(root.session.id, followup_control.clone())
            .expect("register post-fence followup");
        followup_lease
            .set_turn_id(followup_turn)
            .expect("bind post-fence followup");

        let delayed_fence = delayed_service
            .store
            .session_repo()
            .record_agent_tree_stop_fence_for_observed_turn(
                root.session.id,
                TurnInterruptionCause::UserStop,
                old_turn,
            )
            .await
            .expect("delayed service records observed Stop")
            .expect("delayed observed-turn fence");
        assert_eq!(
            delayed_fence, first_fence,
            "delayed service must reuse F1 instead of extending Stop to F2"
        );
        assert!(
            !delayed_service
                .fanout_agent_tree_stop_at_fence(root.session.id, delayed_fence)
                .await
                .expect("fan out original Stop fence")
        );
        assert_eq!(followup_control.cause(), None);
        assert_eq!(
            owner
                .store
                .session_repo()
                .admitted_run_status(root.session.id, followup_admission, followup_turn)
                .await
                .expect("followup admission status"),
            Some(SessionStatus::Running)
        );
    }

    #[tokio::test]
    async fn completed_captured_root_still_authorizes_stop_of_pre_fence_descendants() {
        let (owner, canceller, workspace) = cross_process_service_fixture().await;
        let root = create_session(&owner, &workspace).await;
        let child = create_session(&owner, &workspace).await;
        owner
            .store
            .session_repo()
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                child.session.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let (_root_admission, root_turn) = admit_session_turn(&owner, root.session.id).await;
        let captured_root = owner
            .store
            .session_repo()
            .captured_running_terminal_target(root.session.id)
            .await
            .expect("capture running root")
            .expect("running root target");
        terminalize_admitted_session(&owner, root.session.id, root_turn).await;
        let (child_admission, child_turn) = admit_session_turn(&owner, child.session.id).await;

        assert_eq!(
            canceller
                .settle_captured_root_for_tree_stop(
                    root.session.id,
                    captured_root,
                    TurnInterruptionCause::UserStop,
                )
                .await
                .expect("observe result-first root race"),
            (true, false),
            "the exact captured root terminal authorizes descendant fanout without rewriting it"
        );
        let fence = canceller
            .store
            .session_repo()
            .record_agent_tree_stop_fence(root.session.id, TurnInterruptionCause::UserStop)
            .await
            .expect("record result-first Stop fence")
            .expect("result-first Stop fence");
        assert!(
            canceller
                .fanout_agent_tree_stop_at_fence(root.session.id, fence)
                .await
                .expect("fan out result-first Stop")
        );
        assert_eq!(
            owner
                .get_session(root.session.id)
                .await
                .expect("completed root")
                .status,
            SessionStatus::Completed
        );
        assert_eq!(
            owner
                .get_session(child.session.id)
                .await
                .expect("stopped child")
                .status,
            SessionStatus::Cancelled
        );
        assert_eq!(
            owner
                .store
                .session_repo()
                .durable_terminal_for_turn(child.session.id, child_turn)
                .await
                .expect("stopped child terminal")
                .map(|terminal| terminal.session_status()),
            Some(SessionStatus::Cancelled)
        );
        assert!(
            !owner
                .store
                .session_repo()
                .has_fresh_run_admission(child.session.id)
                .await
                .expect("stopped child admission")
        );
        let _ = child_admission;
    }

    #[tokio::test]
    async fn later_root_fanout_finishes_an_earlier_child_stop_with_its_original_cause() {
        let (owner, canceller, workspace) = cross_process_service_fixture().await;
        let root = create_session(&owner, &workspace).await;
        let child = create_session(&owner, &workspace).await;
        owner
            .store
            .session_repo()
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                child.session.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let (_root_admission, _root_turn) = admit_session_turn(&owner, root.session.id).await;
        let (_child_admission, child_turn) = admit_session_turn(&owner, child.session.id).await;
        owner
            .store
            .session_repo()
            .record_agent_tree_stop_fence(child.session.id, TurnInterruptionCause::UserStop)
            .await
            .expect("record earlier child Stop")
            .expect("earlier child Stop fence");

        assert!(
            canceller
                .cancel_running_session_tree(root.session.id, TurnInterruptionCause::UserStop,)
                .await
                .expect("later root Stop")
        );
        let child_terminal = owner
            .store
            .session_repo()
            .durable_terminal_for_turn(child.session.id, child_turn)
            .await
            .expect("child terminal")
            .expect("stopped child terminal");
        assert_eq!(
            child_terminal.outcome,
            TurnTerminalOutcome::Interrupted {
                cause: TurnInterruptionCause::UserStop
            },
            "a later root fanout must finish the earliest child Stop without rewriting it"
        );
    }

    #[tokio::test]
    async fn root_terminal_classification_blocks_stop_fanout_to_live_children() {
        enum RootClassification {
            Failure,
            Superseded,
            SuccessSealed,
        }

        let (service, workspace, _) = service_fixture().await;
        for classification in [
            RootClassification::Failure,
            RootClassification::Superseded,
            RootClassification::SuccessSealed,
        ] {
            let root = create_session(&service, &workspace).await;
            let child = create_session(&service, &workspace).await;
            service
                .store
                .session_repo()
                .insert_session_spawn_edge(
                    root.session.id,
                    root.session.id,
                    child.session.id,
                    "/root/child",
                    "child",
                )
                .await
                .expect("child edge");
            let (_root_admission, root_turn) = admit_session_turn(&service, root.session.id).await;
            let (_child_admission, child_turn) =
                admit_session_turn(&service, child.session.id).await;
            let root_control = RunControl::new();
            let child_control = RunControl::new();
            let root_lease = service
                .store
                .active_runs()
                .try_start(root.session.id, root_control.clone())
                .expect("root run");
            root_lease.set_turn_id(root_turn).expect("bind root turn");
            let child_lease = service
                .store
                .active_runs()
                .try_start(child.session.id, child_control.clone())
                .expect("child run");
            child_lease
                .set_turn_id(child_turn)
                .expect("bind child turn");

            match classification {
                RootClassification::Failure => {
                    assert!(root_control.fail("provider failed"));
                }
                RootClassification::Superseded => {
                    assert!(root_control.supersede());
                }
                RootClassification::SuccessSealed => {
                    assert!(root_control.seal_success());
                }
            }

            assert!(
                !service
                    .cancel_running_session_tree(root.session.id, TurnInterruptionCause::UserStop,)
                    .await
                    .expect("stop result"),
                "the root terminal owner must reject a competing Stop"
            );
            assert_eq!(child_control.cause(), None);
            assert!(!child_control.is_cancelled());
        }
    }

    #[tokio::test]
    async fn existing_same_root_stop_authorizes_child_fanout() {
        let (service, workspace, _) = service_fixture().await;
        let root = create_session(&service, &workspace).await;
        let child = create_session(&service, &workspace).await;
        service
            .store
            .session_repo()
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                child.session.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let (_root_admission, root_turn) = admit_session_turn(&service, root.session.id).await;
        let (_child_admission, child_turn) = admit_session_turn(&service, child.session.id).await;
        let root_control = RunControl::new();
        let child_control = RunControl::new();
        let root_lease = service
            .store
            .active_runs()
            .try_start(root.session.id, root_control.clone())
            .expect("root run");
        root_lease.set_turn_id(root_turn).expect("bind root turn");
        let child_lease = service
            .store
            .active_runs()
            .try_start(child.session.id, child_control.clone())
            .expect("child run");
        child_lease
            .set_turn_id(child_turn)
            .expect("bind child turn");
        assert!(root_control.interrupt(TurnInterruptionCause::UserStop));

        assert!(
            service
                .cancel_running_session_tree(root.session.id, TurnInterruptionCause::UserStop,)
                .await
                .expect("stop result")
        );
        assert_eq!(
            child_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::TreeStopped
            ))
        );
    }

    async fn assert_failed_recovery(service: &SessionService, session_id: SessionId, reason: &str) {
        assert_eq!(
            service
                .get_session(session_id)
                .await
                .expect("recovered session")
                .status,
            SessionStatus::Failed
        );
        let items = service
            .store
            .protocol_event_store()
            .list_turn_items_for_session(session_id)
            .expect("recovery turn items");
        assert!(items.iter().any(|item| matches!(
            &item.payload,
            TurnItemPayload::Terminal {
                outcome: TurnTerminalOutcome::Failed { error },
            } if error == reason
        )));
        assert!(!items.iter().any(|item| matches!(
            &item.payload,
            TurnItemPayload::Terminal {
                outcome: TurnTerminalOutcome::Interrupted { .. },
            }
        )));
    }

    #[tokio::test]
    async fn sealed_durable_root_success_allows_detached_child_stop_before_lease_drop() {
        let (service, workspace, _) = service_fixture().await;
        let root = create_session(&service, &workspace).await;
        let child = create_session(&service, &workspace).await;
        service
            .store
            .session_repo()
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                child.session.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let (_root_admission, root_turn) = admit_session_turn(&service, root.session.id).await;
        let root_control = RunControl::new();
        let root_lease = service
            .store
            .active_runs()
            .try_start(root.session.id, root_control.clone())
            .expect("root run");
        root_lease.set_turn_id(root_turn).expect("bind root turn");
        terminalize_admitted_session(&service, root.session.id, root_turn).await;
        assert!(root_control.seal_success());

        let (_child_admission, child_turn) = admit_session_turn(&service, child.session.id).await;
        let child_control = RunControl::new();
        let child_lease = service
            .store
            .active_runs()
            .try_start(child.session.id, child_control.clone())
            .expect("child run");
        child_lease
            .set_turn_id(child_turn)
            .expect("bind child turn");

        assert!(
            service
                .cancel_running_session_tree(root.session.id, TurnInterruptionCause::UserStop,)
                .await
                .expect("tree stop")
        );
        assert_eq!(
            service
                .get_session(root.session.id)
                .await
                .expect("completed root")
                .status,
            SessionStatus::Completed
        );
        assert_eq!(
            child_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::TreeStopped
            ))
        );
    }

    #[tokio::test]
    async fn deferred_stop_preserves_committing_root_success_and_stops_child() {
        let (service, workspace, _) = service_fixture().await;
        let root = create_session(&service, &workspace).await;
        let child = create_session(&service, &workspace).await;
        service
            .store
            .session_repo()
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                child.session.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let (_root_admission, root_turn) = admit_session_turn(&service, root.session.id).await;
        let (_child_admission, child_turn) = admit_session_turn(&service, child.session.id).await;
        let root_control = RunControl::new();
        let child_control = RunControl::new();
        let root_lease = service
            .store
            .active_runs()
            .try_start(root.session.id, root_control.clone())
            .expect("root run");
        root_lease.set_turn_id(root_turn).expect("bind root turn");
        let child_lease = service
            .store
            .active_runs()
            .try_start(child.session.id, child_control.clone())
            .expect("child run");
        child_lease
            .set_turn_id(child_turn)
            .expect("bind child turn");
        let success_commit = root_control
            .begin_success_commit()
            .expect("reserve success commit");

        assert!(
            service
                .cancel_running_session_tree(root.session.id, TurnInterruptionCause::UserStop,)
                .await
                .expect("tree stop")
        );
        assert_eq!(
            child_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::TreeStopped
            ))
        );
        assert!(success_commit.seal());
        assert!(root_control.success_is_sealed());
        assert_eq!(root_control.cause(), None);
    }

    #[tokio::test]
    async fn completed_root_archive_and_delete_wait_for_active_child_across_processes() {
        let (owner, manager, workspace) = cross_process_service_fixture().await;
        let root = create_session(&owner, &workspace).await;
        let child = create_session(&owner, &workspace).await;
        owner
            .store
            .session_repo()
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                child.session.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let (root_admission, root_turn) = admit_session_turn(&owner, root.session.id).await;
        terminalize_admitted_session(&owner, root.session.id, root_turn).await;
        assert!(
            owner
                .store
                .session_repo()
                .release_stopped_run_admission(root.session.id, root_admission)
                .await
                .expect("release completed root admission")
        );
        assert_eq!(
            owner
                .get_session(root.session.id)
                .await
                .expect("completed root")
                .status,
            SessionStatus::Completed
        );

        let child_live_lease = owner
            .store
            .active_runs()
            .try_start(child.session.id, RunControl::new())
            .expect("in-memory child run");
        for error in [
            owner
                .set_session_archived(root.session.id, true)
                .await
                .expect_err("active child blocks root archive"),
            owner
                .delete_session(root.session.id)
                .await
                .expect_err("active child blocks root delete"),
        ] {
            assert!(error.to_string().contains(&child.session.id.to_string()));
        }
        drop(child_live_lease);

        let (child_admission, child_turn) = admit_session_turn(&owner, child.session.id).await;
        assert!(
            !manager.store.active_runs().is_active(child.session.id),
            "the second process must detect the child from its durable admission"
        );
        for error in [
            manager
                .set_session_archived(root.session.id, true)
                .await
                .expect_err("cross-process child blocks root archive"),
            manager
                .delete_session(root.session.id)
                .await
                .expect_err("cross-process child blocks root delete"),
        ] {
            assert!(error.to_string().contains(&child.session.id.to_string()));
        }

        terminalize_admitted_session(&owner, child.session.id, child_turn).await;
        for error in [
            manager
                .set_session_archived(root.session.id, true)
                .await
                .expect_err("fresh terminal child admission blocks root archive"),
            manager
                .delete_session(root.session.id)
                .await
                .expect_err("fresh terminal child admission blocks root delete"),
        ] {
            assert!(error.to_string().contains(&child.session.id.to_string()));
        }
        assert!(
            owner
                .store
                .session_repo()
                .release_stopped_run_admission(child.session.id, child_admission)
                .await
                .expect("release completed child admission")
        );
        manager
            .set_session_archived(root.session.id, true)
            .await
            .expect("terminal tree can be archived");
        manager
            .set_session_archived(root.session.id, false)
            .await
            .expect("terminal tree can be unarchived");
        manager
            .delete_session(root.session.id)
            .await
            .expect("terminal tree can be deleted");
        assert!(manager.get_session(root.session.id).await.is_err());
        assert!(manager.get_session(child.session.id).await.is_err());
    }

    #[tokio::test]
    async fn explicit_stop_after_root_completion_terminalizes_only_detached_running_child() {
        let (owner, canceller, workspace) = cross_process_service_fixture().await;
        let root = create_session(&owner, &workspace).await;
        let child = create_session(&owner, &workspace).await;
        owner
            .store
            .session_repo()
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                child.session.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        let (_root_admission, root_turn) = admit_session_turn(&owner, root.session.id).await;
        terminalize_admitted_session(&owner, root.session.id, root_turn).await;
        let (child_admission, child_turn) = admit_session_turn(&owner, child.session.id).await;

        assert!(
            canceller
                .cancel_running_session_tree(root.session.id, TurnInterruptionCause::UserStop,)
                .await
                .expect("tree stop")
        );
        assert_eq!(
            owner
                .get_session(root.session.id)
                .await
                .expect("completed root")
                .status,
            SessionStatus::Completed,
            "tree Stop must not rewrite the durable root result"
        );
        assert_cancelled_admission(&owner, child.session.id, child_admission, child_turn).await;
    }

    #[tokio::test]
    async fn explicit_stop_settles_running_root_and_only_idle_child_with_pending_trigger() {
        let (owner, canceller, workspace) = cross_process_service_fixture().await;
        let root = create_session(&owner, &workspace).await;
        let triggered_child = create_session(&owner, &workspace).await;
        let quiet_child = create_session(&owner, &workspace).await;
        let repository = owner.store.session_repo();
        repository
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                triggered_child.session.id,
                "/root/triggered",
                "triggered",
            )
            .await
            .expect("triggered child edge");
        repository
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                quiet_child.session.id,
                "/root/quiet",
                "quiet",
            )
            .await
            .expect("quiet child edge");
        let _ = admit_session_turn(&owner, root.session.id).await;
        let pending = repository
            .append_inter_agent_communication_with_protocol_bundle(
                triggered_child.session.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/triggered".to_string(),
                    content: "Message Type: NEW_TASK\nTask name: /root/triggered\nSender: /root\nPayload:\nrun the pending task".to_string(),
                    trigger_turn: true,
                },
                false,
            )
            .expect("pending child trigger");
        assert!(pending.schedule_turn);
        assert_eq!(
            repository
                .pending_agent_trigger_history_item_id(triggered_child.session.id)
                .expect("pending trigger before Stop"),
            Some(pending.history_item_id)
        );
        assert!(
            canceller
                .cancel_running_session_tree(root.session.id, TurnInterruptionCause::UserStop,)
                .await
                .expect("cross-process tree Stop")
        );
        assert_eq!(
            owner
                .get_session(root.session.id)
                .await
                .expect("stopped root")
                .status,
            SessionStatus::Cancelled
        );
        assert_eq!(
            owner
                .get_session(triggered_child.session.id)
                .await
                .expect("settled triggered child")
                .status,
            SessionStatus::Cancelled
        );
        assert_eq!(
            owner
                .get_session(quiet_child.session.id)
                .await
                .expect("untouched quiet child")
                .status,
            SessionStatus::Idle
        );

        let triggered_events = owner
            .store
            .protocol_event_store()
            .list_runtime_events_for_session(triggered_child.session.id)
            .expect("synthetic interrupted events");
        assert_eq!(triggered_events.len(), 2);
        let synthetic_turn_id = triggered_events[0].turn_id;
        assert!(
            triggered_events
                .iter()
                .all(|event| event.turn_id == synthetic_turn_id)
        );
        assert!(matches!(
            triggered_events.as_slice(),
            [RuntimeEvent {
                msg: RuntimeEventMsg::Warning { message },
                ..
            }, RuntimeEvent {
                msg: RuntimeEventMsg::TurnTerminal { terminal },
                ..
            }] if message.starts_with("thread started:")
                && matches!(
                    terminal.outcome,
                    TurnTerminalOutcome::Interrupted {
                        cause: TurnInterruptionCause::TreeStopped
                    }
                )
        ));
        assert!(
            repository
                .agent_completion_handoff(triggered_child.session.id, synthetic_turn_id)
                .expect("interrupted child handoff query")
                .is_none()
        );
        assert!(
            owner
                .store
                .protocol_event_store()
                .list_history_items_for_session(root.session.id)
                .expect("root history after child Stop")
                .into_iter()
                .all(|item| !matches!(
                    item.payload,
                    HistoryItemPayload::InterAgentCommunication { .. }
                ))
        );

        let paths = owner.store.paths().clone();
        let reopened_sqlite = SqliteStore::open(&paths).expect("reopen database after Stop");
        let reopened = StoreBundle::new(reopened_sqlite);
        assert_eq!(
            reopened
                .session_repo()
                .pending_agent_trigger_history_item_id(triggered_child.session.id)
                .expect("reopened pending trigger query"),
            None
        );
        let descendants = reopened
            .protocol_event_store()
            .retained_descendant_page(root.session.id, 0, 10)
            .expect("reopened retained descendant projection");
        assert!(descendants.items.iter().all(|descendant| {
            descendant.edge.child_session_id != triggered_child.session.id
                || descendant.pending_trigger_history_item_id.is_none()
        }));
        assert!(
            reopened
                .protocol_event_store()
                .list_runtime_events_for_session(quiet_child.session.id)
                .expect("quiet child runtime history")
                .is_empty()
        );
        assert_eq!(
            reopened
                .protocol_event_store()
                .latest_turn_position_for_session(quiet_child.session.id)
                .expect("quiet child latest turn"),
            None,
            "an idle descendant without pending mail must not receive a synthetic TurnId"
        );
    }

    #[tokio::test]
    async fn explicit_stop_settles_queued_completed_owner_while_child_worker_is_live() {
        let (service, workspace, _) = service_fixture().await;
        let root = create_session(&service, &workspace).await;
        let owner = create_session(&service, &workspace).await;
        let child = create_session(&service, &workspace).await;
        let repository = service.store.session_repo();
        repository
            .insert_session_spawn_edge(
                root.session.id,
                root.session.id,
                owner.session.id,
                "/root/owner",
                "owner",
            )
            .await
            .expect("owner edge");
        repository
            .insert_session_spawn_edge(
                root.session.id,
                owner.session.id,
                child.session.id,
                "/root/owner/child",
                "child",
            )
            .await
            .expect("child edge");

        let (root_admission, root_turn) = admit_session_turn(&service, root.session.id).await;
        terminalize_admitted_session(&service, root.session.id, root_turn).await;
        assert!(
            repository
                .release_stopped_run_admission(root.session.id, root_admission)
                .await
                .expect("release completed Stop-test root admission")
        );
        let (child_admission, child_turn) = admit_session_turn(&service, child.session.id).await;
        let child_control = RunControl::new();
        let child_live_lease = service
            .store
            .active_runs()
            .try_start(child.session.id, child_control.clone())
            .expect("live child worker token");
        child_live_lease
            .set_turn_id(child_turn)
            .expect("bind live child turn");
        let (owner_admission, owner_turn) = admit_session_turn(&service, owner.session.id).await;
        terminalize_admitted_session(&service, owner.session.id, owner_turn).await;
        assert!(
            repository
                .release_stopped_run_admission(owner.session.id, owner_admission)
                .await
                .expect("release completed owner admission")
        );
        assert!(
            repository
                .pending_deferred_completion(owner.session.id)
                .expect("completed owner deferred")
                .is_none(),
            "normal completion publishes directly even while a child remains live"
        );
        let queued = repository
            .append_inter_agent_communication_with_protocol_bundle(
                owner.session.id,
                InterAgentCommunication {
                    author: "/root".to_string(),
                    recipient: "/root/owner".to_string(),
                    content: "Message Type: NEW_TASK\nTask name: /root/owner\nSender: /root\nPayload:\nqueue until descendants settle".to_string(),
                    trigger_turn: true,
                },
                false,
            )
            .expect("queued explicit owner trigger");
        assert!(
            queued.schedule_turn,
            "a completed owner is eligible for an explicit follow-up"
        );

        assert!(
            service
                .cancel_running_session_tree(root.session.id, TurnInterruptionCause::UserStop,)
                .await
                .expect("tree Stop")
        );
        assert_eq!(
            child_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::TreeStopped
            ))
        );
        assert_eq!(
            repository
                .pending_agent_trigger_history_item_id(owner.session.id)
                .expect("stopped queued owner trigger"),
            None
        );
        assert_eq!(
            service
                .get_session(owner.session.id)
                .await
                .expect("stopped completed owner")
                .status,
            SessionStatus::Cancelled
        );
        assert!(
            repository
                .pending_deferred_completion(owner.session.id)
                .expect("discarded owner deferred")
                .is_none()
        );

        let child_target = repository
            .captured_running_terminal_target(child.session.id)
            .await
            .expect("capture stopped child")
            .expect("durably running child");
        assert!(
            repository
                .terminalize_captured_running_session_with_protocol_event(
                    child.session.id,
                    &test_terminal_event(
                        child.session.id,
                        TurnTerminalOutcome::Interrupted {
                            cause: TurnInterruptionCause::TreeStopped,
                        },
                    ),
                    child_target,
                )
                .await
                .expect("child worker terminal")
        );
        assert!(
            repository
                .release_stopped_run_admission(child.session.id, child_admission)
                .await
                .expect("release stopped child admission")
        );
        assert_eq!(
            repository
                .fresh_running_turn_for_session(child.session.id)
                .await
                .expect("stopped child turn"),
            None
        );
        let _ = child_turn;
        drop(child_live_lease);

        service
            .delete_session(root.session.id)
            .await
            .expect("delete fully stopped tree");
        service
            .delete_project(workspace.project_id)
            .await
            .expect("delete project after stopped tree cleanup");
    }

    #[tokio::test]
    async fn explicit_stop_resolves_pending_owner_resume_without_creating_final() {
        let (service, workspace, _) = service_fixture().await;
        let (root, middle, leaf, _sibling) = create_nested_agent_tree(&service, &workspace).await;
        let repository = service.store.session_repo();
        let (_, root_turn_id) = admit_session_turn(&service, root.session.id).await;
        terminalize_admitted_session(&service, root.session.id, root_turn_id).await;
        let (_, middle_turn_id) = admit_session_turn(&service, middle.session.id).await;
        let (leaf_admission_id, leaf_turn_id) = admit_session_turn(&service, leaf.session.id).await;
        let middle_target = repository
            .captured_running_terminal_target(middle.session.id)
            .await
            .expect("capture crashed middle")
            .expect("running middle");
        assert!(
            repository
                .recover_captured_running_session_with_protocol_event(
                    middle.session.id,
                    &test_terminal_event(
                        middle.session.id,
                        TurnTerminalOutcome::Failed {
                            error: "middle crashed while leaf remained live".to_string(),
                        },
                    ),
                    middle_target,
                )
                .await
                .expect("recover crashed middle")
        );
        let deferred = repository
            .pending_deferred_completion(middle.session.id)
            .expect("middle crash receipt")
            .expect("middle crash deferred while leaf is live");
        assert_eq!(deferred.agent_turn_id, middle_turn_id);
        assert_eq!(
            deferred.kind,
            crate::storage::session_repo::DeferredAgentCompletionKind::CrashFailed
        );
        assert_eq!(
            repository
                .terminalize_admitted_turn_with_protocol_event(
                    leaf.session.id,
                    leaf_admission_id,
                    &test_terminal_event(
                        leaf.session.id,
                        TurnTerminalOutcome::Failed {
                            error: "leaf failed before Stop".to_string(),
                        },
                    ),
                    leaf_turn_id,
                    None,
                    None,
                )
                .await
                .expect("leaf failure"),
            crate::storage::session_repo::AdmittedTerminalCommit::Applied
        );
        assert!(
            repository
                .schedulable_owner_resume_request_id(middle.session.id)
                .expect("pending middle resume")
                .is_some()
        );

        assert!(
            !service
                .cancel_running_session_tree(root.session.id, TurnInterruptionCause::UserStop,)
                .await
                .expect("tree Stop"),
            "the fence cancels dormant resume work without synthesizing an agent turn"
        );
        assert_eq!(
            service
                .get_session(middle.session.id)
                .await
                .expect("settled middle")
                .status,
            SessionStatus::Failed
        );
        assert_eq!(
            repository
                .schedulable_owner_resume_request_id(middle.session.id)
                .expect("resolved middle resume"),
            None
        );
        assert!(
            service
                .store
                .protocol_event_store()
                .list_runtime_events_for_session(middle.session.id)
                .expect("middle runtime events")
                .into_iter()
                .all(|event| !matches!(
                    event.msg,
                    RuntimeEventMsg::TurnTerminal {
                        terminal
                    } if matches!(
                        terminal.outcome,
                        TurnTerminalOutcome::Interrupted { .. }
                    )
                )),
            "cancelling a dormant OwnerResume must not synthesize a terminal turn or FINAL"
        );
    }

    #[tokio::test]
    async fn explicit_stop_after_root_completion_without_live_descendant_is_a_noop() {
        let (owner, canceller, workspace) = cross_process_service_fixture().await;
        let root = create_session(&owner, &workspace).await;
        let (_root_admission, root_turn) = admit_session_turn(&owner, root.session.id).await;
        terminalize_admitted_session(&owner, root.session.id, root_turn).await;

        assert!(
            !canceller
                .cancel_running_session_tree(root.session.id, TurnInterruptionCause::UserStop,)
                .await
                .expect("tree stop")
        );
        assert_eq!(
            owner
                .get_session(root.session.id)
                .await
                .expect("completed root")
                .status,
            SessionStatus::Completed
        );
    }

    #[tokio::test]
    async fn terminal_failed_or_cancelled_root_is_preserved_while_detached_child_stops() {
        let (owner, canceller, workspace) = cross_process_service_fixture().await;
        for terminal_status in [SessionStatus::Failed, SessionStatus::Cancelled] {
            let root = create_session(&owner, &workspace).await;
            let child = create_session(&owner, &workspace).await;
            owner
                .store
                .session_repo()
                .insert_session_spawn_edge(
                    root.session.id,
                    root.session.id,
                    child.session.id,
                    "/root/child",
                    "child",
                )
                .await
                .expect("child edge");
            let (_root_admission, _root_turn) = admit_session_turn(&owner, root.session.id).await;
            let terminal_event = match terminal_status {
                SessionStatus::Failed => test_terminal_event(
                    root.session.id,
                    TurnTerminalOutcome::Failed {
                        error: "root failed".to_string(),
                    },
                ),
                SessionStatus::Cancelled => test_terminal_event(
                    root.session.id,
                    TurnTerminalOutcome::Interrupted {
                        cause: TurnInterruptionCause::UserStop,
                    },
                ),
                _ => unreachable!(),
            };
            let root_target = owner
                .store
                .session_repo()
                .captured_running_terminal_target(root.session.id)
                .await
                .expect("capture root terminal target")
                .expect("root running target");
            assert!(
                owner
                    .store
                    .session_repo()
                    .terminalize_captured_running_session_with_protocol_event(
                        root.session.id,
                        &terminal_event,
                        root_target,
                    )
                    .await
                    .expect("root terminal")
            );
            let (child_admission, child_turn) = admit_session_turn(&owner, child.session.id).await;

            assert!(
                canceller
                    .cancel_running_session_tree(root.session.id, TurnInterruptionCause::UserStop,)
                    .await
                    .expect("tree stop")
            );
            assert_eq!(
                owner
                    .get_session(root.session.id)
                    .await
                    .expect("terminal root")
                    .status,
                terminal_status
            );
            assert_cancelled_admission(&owner, child.session.id, child_admission, child_turn).await;
        }
    }

    #[tokio::test]
    async fn cross_process_child_cancel_terminalizes_only_its_nested_subtree() {
        let (owner, canceller, workspace) = cross_process_service_fixture().await;
        let (root, middle, leaf, sibling) = create_nested_agent_tree(&owner, &workspace).await;
        let (root_admission, root_turn) = admit_session_turn(&owner, root.session.id).await;
        let (middle_admission, middle_turn) = admit_session_turn(&owner, middle.session.id).await;
        let (leaf_admission, leaf_turn) = admit_session_turn(&owner, leaf.session.id).await;
        let (sibling_admission, sibling_turn) =
            admit_session_turn(&owner, sibling.session.id).await;

        assert!(
            canceller
                .cancel_running_session_tree(middle.session.id, TurnInterruptionCause::UserStop,)
                .await
                .expect("middle cancellation")
        );

        assert_cancelled_admission(&owner, middle.session.id, middle_admission, middle_turn).await;
        assert_cancelled_admission(&owner, leaf.session.id, leaf_admission, leaf_turn).await;
        for (session_id, admission_id, turn_id) in [
            (root.session.id, root_admission, root_turn),
            (sibling.session.id, sibling_admission, sibling_turn),
        ] {
            assert_eq!(
                owner
                    .store
                    .session_repo()
                    .get_session(session_id)
                    .await
                    .expect("unaffected session")
                    .status,
                SessionStatus::Running
            );
            assert_eq!(
                owner
                    .store
                    .session_repo()
                    .admitted_run_status(session_id, admission_id, turn_id)
                    .await
                    .expect("unaffected admission"),
                Some(SessionStatus::Running)
            );
            assert!(
                owner
                    .store
                    .session_repo()
                    .durable_terminal_for_turn(session_id, turn_id)
                    .await
                    .expect("unaffected protocol")
                    .is_none()
            );
        }
    }
}
