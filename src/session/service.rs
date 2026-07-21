use std::fs;

use crate::config::ProviderEndpoint;
use crate::error::SessionError;
use crate::protocol::{
    CanonicalProtocolSnapshot, HistoryItem, ModeKind, ProtocolEventStore, ProtocolPageRequest,
    SteerTurn, TurnInterruptionCause, TurnTerminalOutcome, UserTurn,
};
#[cfg(test)]
use crate::protocol::{TurnId, TurnItem};
use crate::runtime::{ActiveRunInterruptOutcome, RunCancellationCause};
use crate::session::{
    AdmissionId, CanonicalHistoryPage, CanonicalRuntimeEventPage, CanonicalSessionFence,
    CanonicalSessionRead, CanonicalSessionSnapshot, CanonicalTurnPage, DurableTurnTerminal,
    IdleTurnAdmission, IdleTurnRejectionReason, LoadedSessionList, LoadedSessionStatus,
    LoadedSessionSummary, NewSession, ProjectId, ProjectRecord, ProjectRepository, RunEvent,
    RunningSessionRejoin, SessionContext, SessionForkResult, SessionId, SessionRecord,
    SessionRepository, SessionRollbackResult, SessionSelector, SessionSettingsPatch,
    SessionSettingsUpdate, SessionStartRequest, SessionStatus, SessionTitleUpdate,
};
use crate::storage::StoreBundle;
use crate::storage::session_repo::{DurableSessionStopState, RunningSessionTerminalTarget};
use crate::workspace::Workspace;

const RUNNING_SESSION_RECOVERY_PAGE_SIZE: usize = 64;

#[derive(Clone)]
pub struct SessionService {
    pub store: StoreBundle,
}

impl SessionService {
    pub fn new(store: StoreBundle) -> Self {
        Self { store }
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
        let repository = self.store.session_repo();
        let session = match &request.selector {
            SessionSelector::New => {
                let title = request.title.unwrap_or_else(|| "New Session".to_string());
                repository
                    .create_session(NewSession {
                        project_id: workspace.project_id,
                        title,
                        cwd: request.cwd.clone(),
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
        Ok(session)
    }

    pub async fn store_user_turn_with_protocol_bundle(
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
        self.cancel_running_session_with_cause(session_id, TurnInterruptionCause::UserStop)
            .await
    }

    async fn cancel_running_session_with_cause(
        &self,
        session_id: crate::session::SessionId,
        root_cause: TurnInterruptionCause,
    ) -> Result<bool, SessionError> {
        let repo = self.store.session_repo();
        let root_stop_state = repo
            .durable_session_stop_state(session_id)
            .await?
            .ok_or_else(|| SessionError::Message(format!("session {session_id} was not found")))?;
        let mut targets = vec![session_id];
        if repo
            .session_spawn_edge_for_child(session_id)
            .await?
            .is_none()
        {
            targets.extend(
                repo.list_session_spawn_edges(session_id)
                    .await?
                    .into_iter()
                    .map(|edge| edge.child_session_id),
            );
        }

        let root_control = self.store.active_runs().run_control(session_id);
        let (fanout_authorized, mut cancelled) =
            match self.store.active_runs().cancel(session_id, root_cause) {
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
                ActiveRunInterruptOutcome::NotActive => {
                    match root_stop_state {
                        DurableSessionStopState::Running(target) => {
                            let terminalized = self
                                .terminalize_running_session(
                                    session_id,
                                    RunEvent::TurnTerminal {
                                        session_id,
                                        terminal: Box::new(DurableTurnTerminal {
                                            outcome: TurnTerminalOutcome::Interrupted {
                                                cause: root_cause,
                                            },
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
                            (terminalized, terminalized)
                        }
                        DurableSessionStopState::Terminal(_) => {
                            // The root worker is gone, so a later explicit tree-wide Stop may target
                            // detached descendants without rewriting the root's durable result.
                            (true, false)
                        }
                        DurableSessionStopState::Idle => (false, false),
                    }
                }
            };
        if !fanout_authorized {
            // A competing in-memory terminal classification at the requested root is
            // authoritative. Descendants must not be stopped through an independent fallback
            // path; durable terminal roots are handled above after the worker lease is gone.
            return Ok(false);
        }

        for target_session_id in targets.into_iter().filter(|target| *target != session_id) {
            let cause = TurnInterruptionCause::TreeStopped;
            let child_stop_state = repo
                .durable_session_stop_state(target_session_id)
                .await?
                .ok_or_else(|| {
                    SessionError::Message(format!("session {target_session_id} was not found"))
                })?;
            let child_control = self.store.active_runs().run_control(target_session_id);
            match self.store.active_runs().cancel(target_session_id, cause) {
                ActiveRunInterruptOutcome::Applied => {
                    cancelled = true;
                    continue;
                }
                ActiveRunInterruptOutcome::AlreadyClassified => {
                    // An already-classified descendant keeps its first typed cause. The root has
                    // nevertheless authorized this fanout, so no independent reclassification or
                    // durable overwrite is attempted here.
                    cancelled |= child_control.is_some_and(|control| {
                        control.cause() == Some(RunCancellationCause::Interruption(cause))
                    });
                    continue;
                }
                ActiveRunInterruptOutcome::Deferred => {
                    continue;
                }
                ActiveRunInterruptOutcome::NotActive => {}
            }
            if let DurableSessionStopState::Running(target) = child_stop_state {
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
            }
        }
        Ok(cancelled)
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
            let Some(last_session_id) = sessions.last().map(|candidate| candidate.session.id)
            else {
                break;
            };
            after = Some(last_session_id);

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

    async fn terminalize_running_session(
        &self,
        session_id: SessionId,
        event: RunEvent,
        target: RunningSessionTerminalTarget,
    ) -> Result<bool, SessionError> {
        let terminalized = self
            .store
            .session_repo()
            .terminalize_captured_running_session_with_protocol_event(session_id, &event, target)
            .await?;
        Ok(terminalized)
    }

    async fn recover_orphaned_running_session(
        &self,
        session_id: SessionId,
        event: RunEvent,
        target: RunningSessionTerminalTarget,
    ) -> Result<bool, SessionError> {
        let terminalized = self
            .store
            .session_repo()
            .recover_captured_running_session_with_protocol_event(session_id, &event, target)
            .await?;
        Ok(terminalized)
    }

    pub async fn get_session(&self, session_id: SessionId) -> Result<SessionRecord, SessionError> {
        Ok(self.store.session_repo().get_session(session_id).await?)
    }

    pub async fn latest_session(
        &self,
        project_id: ProjectId,
    ) -> Result<Option<SessionRecord>, SessionError> {
        Ok(self.store.session_repo().latest_session(project_id).await?)
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
                "session {session_id} has active agent-tree session {active_session_id}; stop the agent tree before archiving it"
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
        let normalized = normalize_session_settings_patch(patch)?;
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
                "session {session_id} has active agent-tree session {active_session_id}; stop the agent tree before rollback"
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
                "session {session_id} has active agent-tree session {active_session_id}; stop the agent tree before deleting it"
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
        let mut branch_session_ids = vec![session_id];
        if repository
            .session_spawn_edge_for_child(session_id)
            .await?
            .is_none()
        {
            branch_session_ids.extend(
                repository
                    .list_session_spawn_edges(session_id)
                    .await?
                    .into_iter()
                    .map(|edge| edge.child_session_id),
            );
        }

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
                "project {} contains active session {}; stop it before deleting the project",
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
        let snapshot = self
            .store
            .session_repo()
            .canonical_session_protocol_snapshot(session_id, history_request, turn_request)
            .await?;
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

fn normalize_session_settings_patch(
    patch: SessionSettingsPatch,
) -> Result<SessionSettingsPatch, SessionError> {
    if let Some(cwd) = patch.cwd.as_ref() {
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
        cwd: patch.cwd,
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
    use crate::protocol::{
        HistoryItemPayload, ModeKind, TurnItemPayload, TurnTerminalOutcome, UserInputItem,
    };
    use crate::runtime::RunControl;
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
        assert_eq!(
            repository
                .admitted_run_status(session_id, admission_id, turn_id)
                .await
                .expect("admission status"),
            Some(SessionStatus::Cancelled)
        );
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

    async fn create_flat_agent_tree(
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
                root.session.id,
                leaf.session.id,
                "/root/leaf",
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

        assert!(history.iter().any(|item| matches!(
            &item.payload,
            HistoryItemPayload::SteerTurn { client_user_message_id, .. }
                if client_user_message_id.as_deref() == Some("cross-process")
        )));
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
    async fn cross_process_root_cancel_terminalizes_the_entire_agent_tree() {
        let (owner, canceller, workspace) = cross_process_service_fixture().await;
        let (root, middle, leaf, sibling) = create_flat_agent_tree(&owner, &workspace).await;
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
                .cancel_running_session(root.session.id)
                .await
                .expect("root cancellation")
        );

        assert_cancelled_admission(&owner, root.session.id, root_admission, root_turn).await;
        assert_cancelled_admission(&owner, middle.session.id, middle_admission, middle_turn).await;
        assert_cancelled_admission(&owner, leaf.session.id, leaf_admission, leaf_turn).await;
        assert_cancelled_admission(&owner, sibling.session.id, sibling_admission, sibling_turn)
            .await;
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
            let root_control = RunControl::new();
            let child_control = RunControl::new();
            let _root_lease = service
                .store
                .active_runs()
                .try_start(root.session.id, root_control.clone())
                .expect("root run");
            let _child_lease = service
                .store
                .active_runs()
                .try_start(child.session.id, child_control.clone())
                .expect("child run");

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
                    .cancel_running_session(root.session.id)
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
        let root_control = RunControl::new();
        let child_control = RunControl::new();
        let _root_lease = service
            .store
            .active_runs()
            .try_start(root.session.id, root_control.clone())
            .expect("root run");
        let _child_lease = service
            .store
            .active_runs()
            .try_start(child.session.id, child_control.clone())
            .expect("child run");
        assert!(root_control.interrupt(TurnInterruptionCause::UserStop));

        assert!(
            service
                .cancel_running_session(root.session.id)
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
        let root_control = RunControl::new();
        let child_control = RunControl::new();
        let _root_lease = service
            .store
            .active_runs()
            .try_start(root.session.id, root_control.clone())
            .expect("root run");
        let _child_lease = service
            .store
            .active_runs()
            .try_start(child.session.id, child_control.clone())
            .expect("child run");
        let (_root_admission, root_turn) = admit_session_turn(&service, root.session.id).await;
        terminalize_admitted_session(&service, root.session.id, root_turn).await;
        assert!(root_control.seal_success());

        assert!(
            service
                .cancel_running_session(root.session.id)
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
        let root_control = RunControl::new();
        let child_control = RunControl::new();
        let _root_lease = service
            .store
            .active_runs()
            .try_start(root.session.id, root_control.clone())
            .expect("root run");
        let _child_lease = service
            .store
            .active_runs()
            .try_start(child.session.id, child_control.clone())
            .expect("child run");
        let success_commit = root_control
            .begin_success_commit()
            .expect("reserve success commit");

        assert!(
            service
                .cancel_running_session(root.session.id)
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
                .cancel_running_session(root.session.id)
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
    async fn explicit_stop_after_root_completion_without_live_descendant_is_a_noop() {
        let (owner, canceller, workspace) = cross_process_service_fixture().await;
        let root = create_session(&owner, &workspace).await;
        let (_root_admission, root_turn) = admit_session_turn(&owner, root.session.id).await;
        terminalize_admitted_session(&owner, root.session.id, root_turn).await;

        assert!(
            !canceller
                .cancel_running_session(root.session.id)
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
                    .cancel_running_session(root.session.id)
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
    async fn cross_process_child_cancel_terminalizes_only_that_direct_child() {
        let (owner, canceller, workspace) = cross_process_service_fixture().await;
        let (root, middle, leaf, sibling) = create_flat_agent_tree(&owner, &workspace).await;
        let (root_admission, root_turn) = admit_session_turn(&owner, root.session.id).await;
        let (middle_admission, middle_turn) = admit_session_turn(&owner, middle.session.id).await;
        let (leaf_admission, leaf_turn) = admit_session_turn(&owner, leaf.session.id).await;
        let (sibling_admission, sibling_turn) =
            admit_session_turn(&owner, sibling.session.id).await;

        assert!(
            canceller
                .cancel_running_session(middle.session.id)
                .await
                .expect("middle cancellation")
        );

        assert_cancelled_admission(&owner, middle.session.id, middle_admission, middle_turn).await;
        for (session_id, admission_id, turn_id) in [
            (root.session.id, root_admission, root_turn),
            (leaf.session.id, leaf_admission, leaf_turn),
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
