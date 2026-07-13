use std::fs;

use crate::error::SessionError;
use crate::protocol::{
    HistoryItem, HistoryItemPayload, ProtocolEventStore, SteerTurn, TurnId, TurnItem, UserTurn,
};
use crate::session::{
    CanonicalHistoryPage, CanonicalRuntimeEventPage, CanonicalSessionRead, CanonicalTurnPage,
    IdleTurnAdmission, IdleTurnRejectionReason, LoadedSessionList, LoadedSessionStatus,
    LoadedSessionSummary, MessageMetadata, MessagePart, MessageRole, NewMessage, NewPart,
    NewSession, PartKind, ProjectId, ProjectRecord, ProjectRepository, RunEvent,
    RunningSessionRejoin, SessionCompactResult, SessionContext, SessionForkResult, SessionId,
    SessionMemoryMode, SessionMemoryModeUpdate, SessionRecord, SessionRepository,
    SessionRollbackResult, SessionSelector, SessionSettingsPatch, SessionSettingsUpdate,
    SessionStartRequest, SessionStateSnapshot, SessionStatus, SessionTitleUpdate, Transcript,
    UserMessageMeta, transcript_from_history_items,
};
use crate::storage::StoreBundle;
use crate::workspace::Workspace;

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
        request: SessionStartRequest,
        workspace: Workspace,
    ) -> Result<SessionContext, SessionError> {
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

        if self.store.active_runs().is_active(session.id)
            || repository.has_fresh_run_admission(session.id).await?
        {
            return Err(SessionError::Message(format!(
                "session {} is already running; use cancel or an active-turn steer/rejoin surface instead of starting a replacement run",
                session.id
            )));
        }

        Ok(SessionContext {
            session: SessionRecord {
                status: SessionStatus::Idle,
                ..session
            },
            workspace,
        })
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

    pub async fn store_user_thread_op_with_protocol_bundle(
        &self,
        ctx: &SessionContext,
        admission_id: &str,
        turn: &UserTurn,
        requested_model: Option<String>,
        initial_state: SessionStateSnapshot,
        protocol_turn_id: crate::protocol::TurnId,
        protocol_sequence_no: i64,
    ) -> Result<crate::session::MessageRecord, SessionError> {
        let repository = self.store.session_repo();
        let mut parts = vec![NewPart {
            kind: PartKind::Text,
            payload: MessagePart::Text(crate::session::TextPart { text: turn.text() }),
        }];
        for image in turn.images() {
            parts.push(NewPart {
                kind: PartKind::Image,
                payload: MessagePart::Image(image),
            });
        }
        if let Some(prompt_dispatch) = turn.prompt_dispatch.clone() {
            parts.push(NewPart {
                kind: PartKind::PromptDispatch,
                payload: MessagePart::PromptDispatch(prompt_dispatch),
            });
        }
        Ok(repository
            .append_user_message_with_protocol_bundle(
                NewMessage {
                    session_id: ctx.session.id,
                    parent_message_id: None,
                    role: MessageRole::User,
                    metadata: MessageMetadata::User(UserMessageMeta {
                        cwd: ctx.workspace.cwd.clone(),
                        requested_model,
                        editor_context: turn.editor_context.clone(),
                    }),
                },
                admission_id,
                parts,
                &initial_state,
                turn,
                protocol_turn_id,
                protocol_sequence_no,
            )
            .await?)
    }

    pub async fn mark_interrupted_running_sessions(
        &self,
        session_id: crate::session::SessionId,
    ) -> Result<(), SessionError> {
        self.cancel_running_session(session_id, "Previous run was interrupted.")
            .await?;
        Ok(())
    }

    pub async fn cancel_running_session(
        &self,
        session_id: crate::session::SessionId,
        reason: &str,
    ) -> Result<bool, SessionError> {
        let repo = self.store.session_repo();
        let root_session_id = repo
            .session_spawn_edge_for_child(session_id)
            .await?
            .map(|edge| edge.root_session_id)
            .unwrap_or(session_id);
        let edges = repo.list_session_spawn_edges(root_session_id).await?;
        let mut targets = vec![session_id];
        loop {
            let mut changed = false;
            for edge in &edges {
                if targets.contains(&edge.parent_session_id)
                    && !targets.contains(&edge.child_session_id)
                {
                    targets.push(edge.child_session_id);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        let mut cancelled = false;
        for target_session_id in targets {
            cancelled |= self.store.active_runs().cancel(target_session_id);
            let session = repo.get_session(target_session_id).await?;
            if !matches!(
                session.status,
                SessionStatus::Running | SessionStatus::AwaitingUser
            ) {
                continue;
            }
            cancelled |= self
                .terminalize_running_session(
                    target_session_id,
                    SessionStatus::Cancelled,
                    RunEvent::SessionInterrupted {
                        session_id: target_session_id,
                        reason: reason.to_string(),
                    },
                )
                .await?;
        }
        Ok(cancelled)
    }

    pub async fn interrupt_running_session(
        &self,
        session_id: SessionId,
        reason: String,
    ) -> Result<SessionRecord, SessionError> {
        let reason = normalize_session_interrupt_reason(reason);
        if !self.cancel_running_session(session_id, &reason).await? {
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
        plan_mode: bool,
    ) -> Result<IdleTurnAdmission, SessionError> {
        let session = self.store.session_repo().get_session(session_id).await?;
        let rejection_reason = if pending_trigger_turn {
            Some(IdleTurnRejectionReason::PendingTriggerTurn)
        } else if plan_mode {
            Some(IdleTurnRejectionReason::PlanMode)
        } else if !matches!(
            session.status,
            SessionStatus::Idle | SessionStatus::Completed
        ) {
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
        let history_item_id = self
            .store
            .session_repo()
            .accept_active_turn_steer(session_id, steer)
            .await?;
        if self.store.active_runs().is_active(session_id) {
            let _ = self.store.active_runs().enqueue_steer(
                session_id,
                steer.expected_turn_id,
                history_item_id,
                steer.clone(),
            );
        }
        Ok(())
    }

    pub async fn mark_stale_running_sessions(&self, reason: &str) -> Result<usize, SessionError> {
        let repository = self.store.session_repo();
        let sessions = repository.list_running_sessions_for_recovery().await?;
        let mut cancelled = 0;
        for session in sessions {
            if self.store.active_runs().is_active(session.id) {
                continue;
            }
            let Ok(_process_lease) = self.store.try_acquire_run_process_lease(session.id) else {
                continue;
            };
            if matches!(
                session.status,
                SessionStatus::Running | SessionStatus::AwaitingUser
            ) && self
                .recover_orphaned_running_session(
                    session.id,
                    SessionStatus::Cancelled,
                    RunEvent::SessionInterrupted {
                        session_id: session.id,
                        reason: reason.to_string(),
                    },
                )
                .await?
            {
                cancelled += 1;
            }
        }
        Ok(cancelled)
    }

    async fn terminalize_running_session(
        &self,
        session_id: SessionId,
        status: SessionStatus,
        event: RunEvent,
    ) -> Result<bool, SessionError> {
        let (turn_id, sequence_no) = self.running_session_terminal_position(session_id).await?;
        let terminalized = self
            .store
            .session_repo()
            .terminalize_active_session_with_protocol_event(
                session_id,
                status,
                &event,
                turn_id,
                Some(sequence_no),
            )
            .await?;
        Ok(terminalized)
    }

    async fn recover_orphaned_running_session(
        &self,
        session_id: SessionId,
        status: SessionStatus,
        event: RunEvent,
    ) -> Result<bool, SessionError> {
        let (turn_id, sequence_no) = self.running_session_terminal_position(session_id).await?;
        let terminalized = self
            .store
            .session_repo()
            .recover_orphaned_active_session_with_protocol_event(
                session_id,
                status,
                &event,
                turn_id,
                Some(sequence_no),
            )
            .await?;
        Ok(terminalized)
    }

    async fn running_session_terminal_position(
        &self,
        session_id: SessionId,
    ) -> Result<(TurnId, i64), SessionError> {
        let active_turn_id = self
            .store
            .session_repo()
            .active_turn_for_session(session_id)
            .await?;
        let latest_turn_position = self
            .store
            .protocol_event_store()
            .latest_turn_position_for_session(session_id)?;
        let (turn_id, sequence_no) = match (active_turn_id, latest_turn_position) {
            (Some(turn_id), Some((latest_turn_id, sequence_no))) if latest_turn_id == turn_id => {
                (turn_id, sequence_no)
            }
            (Some(turn_id), _) => (turn_id, 0),
            (None, Some(position)) => position,
            (None, None) => (TurnId::new(), 0),
        };
        Ok((turn_id, sequence_no))
    }

    pub async fn load_state(
        &self,
        session_id: crate::session::SessionId,
    ) -> Result<SessionStateSnapshot, SessionError> {
        Ok(self.store.session_repo().get_state(session_id).await?)
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
        let session = self.store.session_repo().get_session(session_id).await?;
        if self.store.active_runs().is_active(session_id)
            || matches!(
                session.status,
                SessionStatus::Running | SessionStatus::AwaitingUser
            )
        {
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

    pub async fn update_session_memory_mode(
        &self,
        session_id: SessionId,
        mode: SessionMemoryMode,
    ) -> Result<SessionMemoryModeUpdate, SessionError> {
        let session = self.store.session_repo().get_session(session_id).await?;
        if matches!(
            session.status,
            SessionStatus::Running | SessionStatus::AwaitingUser
        ) {
            return Err(SessionError::Message(format!(
                "session {} is {}; memory mode update requires an idle or terminal session",
                session.id,
                session.status.key()
            )));
        }
        Ok(self
            .store
            .session_repo()
            .update_session_memory_mode(session_id, mode)
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
        let session = self.store.session_repo().get_session(session_id).await?;
        if matches!(
            session.status,
            SessionStatus::Running | SessionStatus::AwaitingUser
        ) {
            return Err(SessionError::Message(format!(
                "session {} is running; rollback requires cancelling or completing the active turn first",
                session.id
            )));
        }
        let dropped_turn_ids = self
            .store
            .protocol_event_store()
            .rollback_latest_turns(session_id, num_turns)
            .map_err(|error| SessionError::Message(error.to_string()))?;
        let remaining_history = self.canonical_history_items(session_id).await?;
        let restored_state = latest_state_snapshot_from_history(&remaining_history);
        let session = self
            .store
            .session_repo()
            .reset_state_after_protocol_rollback(session_id, &restored_state)
            .await?;
        Ok(SessionRollbackResult {
            session,
            dropped_turn_ids,
            remaining_history_items: remaining_history.len(),
        })
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

    pub async fn compact_session(
        &self,
        session_id: SessionId,
        _keep_recent: usize,
    ) -> Result<SessionCompactResult, SessionError> {
        let session = self.store.session_repo().get_session(session_id).await?;
        if self.store.active_runs().is_active(session_id)
            || matches!(
                session.status,
                SessionStatus::Running | SessionStatus::AwaitingUser
            )
        {
            return Err(SessionError::Message(format!(
                "session {} is active; stop the run before requesting compaction",
                session.id
            )));
        }
        Err(SessionError::Message(
            "semantic session compaction is unavailable; history was left unchanged. Start a new session, reduce attached context, or split the task instead"
                .to_string(),
        ))
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
        for (session, archived, memory_mode) in sessions {
            summaries.push(
                self.loaded_session_summary_with_projection_state(session, archived, memory_mode)
                    .await?,
            );
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
        for (session, archived, memory_mode) in sessions {
            summaries.push(
                self.loaded_session_summary_with_projection_state(session, archived, memory_mode)
                    .await?,
            );
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
        let (archived, memory_mode) = self
            .store
            .session_repo()
            .session_projection_state(session.id)
            .await?;
        self.loaded_session_summary_with_projection_state(session, archived, memory_mode)
            .await
    }

    async fn loaded_session_summary_with_projection_state(
        &self,
        session: SessionRecord,
        archived: bool,
        memory_mode: SessionMemoryMode,
    ) -> Result<LoadedSessionSummary, SessionError> {
        let is_active = matches!(
            session.status,
            SessionStatus::Running | SessionStatus::AwaitingUser
        );
        let active_turn_position = if is_active {
            self.store
                .protocol_event_store()
                .latest_turn_position_for_session(session.id)
                .map_err(|error| SessionError::Message(error.to_string()))?
        } else {
            None
        };
        Ok(LoadedSessionSummary {
            loaded_status: loaded_status_from_session_status(session.status),
            archived,
            memory_mode,
            active_turn_id: active_turn_position.map(|(turn_id, _)| turn_id),
            active_turn_sequence_no: active_turn_position.map(|(_, sequence_no)| sequence_no),
            pending_permission_requests: 0,
            pending_user_input_requests: if session.status == SessionStatus::AwaitingUser {
                1
            } else {
                0
            },
            session,
        })
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
        if !matches!(
            session.status,
            SessionStatus::Running | SessionStatus::AwaitingUser
        ) {
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
        let root_session_id = repository
            .session_spawn_edge_for_child(session_id)
            .await?
            .map(|edge| edge.root_session_id)
            .unwrap_or(session_id);
        let edges = repository.list_session_spawn_edges(root_session_id).await?;
        let mut branch_session_ids = vec![session_id];
        loop {
            let mut added = false;
            for edge in &edges {
                if branch_session_ids.contains(&edge.parent_session_id)
                    && !branch_session_ids.contains(&edge.child_session_id)
                {
                    branch_session_ids.push(edge.child_session_id);
                    added = true;
                }
            }
            if !added {
                break;
            }
        }

        for branch_session_id in branch_session_ids {
            let session = repository.get_session(branch_session_id).await?;
            if self.store.active_runs().is_active(branch_session_id)
                || matches!(
                    session.status,
                    SessionStatus::Running | SessionStatus::AwaitingUser
                )
                || repository
                    .has_fresh_run_admission(branch_session_id)
                    .await?
            {
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

    pub async fn canonical_transcript(
        &self,
        session_id: SessionId,
    ) -> Result<Transcript, SessionError> {
        let session = self.get_session(session_id).await?;
        let history_items = self.canonical_history_items(session_id).await?;
        if history_items.is_empty() {
            return Err(SessionError::Message(
                "canonical protocol history is empty".to_string(),
            ));
        }
        Ok(transcript_from_history_items(&session, &history_items))
    }

    pub async fn canonical_history_items(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<HistoryItem>, SessionError> {
        self.store
            .protocol_event_store()
            .list_history_items_for_session(session_id)
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
        let items = self.canonical_history_items(session_id).await?;
        let total = items.len();
        let page_items = slice_canonical_page(&items, offset, limit);
        Ok(CanonicalHistoryPage {
            session,
            offset,
            limit,
            total,
            has_more: offset.saturating_add(page_items.len()) < total,
            items: page_items,
        })
    }

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
        let items = self.canonical_turn_items(session_id).await?;
        let total = items.len();
        let page_items = slice_canonical_page(&items, offset, limit);
        Ok(CanonicalTurnPage {
            session,
            offset,
            limit,
            total,
            has_more: offset.saturating_add(page_items.len()) < total,
            items: page_items,
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
        let items = self
            .store
            .protocol_event_store()
            .list_runtime_events_for_session(session_id)
            .map_err(|error| SessionError::Message(error.to_string()))?;
        let total = items.len();
        let page_items = slice_canonical_page(&items, offset, limit);
        Ok(CanonicalRuntimeEventPage {
            session,
            offset,
            limit,
            total,
            has_more: offset.saturating_add(page_items.len()) < total,
            items: page_items,
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
        let session = self.get_session(session_id).await?;
        let state = self.load_state(session_id).await?;
        let history = self
            .canonical_history_page(session_id, history_offset, history_limit)
            .await?;
        let turns = self
            .canonical_turn_page(session_id, turn_offset, turn_limit)
            .await?;
        let active_turn_position = self
            .store
            .protocol_event_store()
            .latest_turn_position_for_session(session_id)
            .map_err(|error| SessionError::Message(error.to_string()))?;
        Ok(CanonicalSessionRead {
            session,
            state,
            history,
            turns,
            active_turn_id: active_turn_position.map(|(turn_id, _)| turn_id),
            active_turn_sequence_no: active_turn_position.map(|(_, sequence_no)| sequence_no),
        })
    }

    pub async fn list_todos(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<crate::session::TodoItem>, SessionError> {
        Ok(self.store.session_repo().list_todos(session_id).await?)
    }
}

fn validate_canonical_page_limit(limit: usize) -> Result<(), SessionError> {
    if limit == 0 {
        return Err(SessionError::Message(
            "canonical item page limit must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

fn slice_canonical_page<T: Clone>(items: &[T], offset: usize, limit: usize) -> Vec<T> {
    items.iter().skip(offset).take(limit).cloned().collect()
}

fn loaded_status_from_session_status(status: SessionStatus) -> LoadedSessionStatus {
    match status {
        SessionStatus::Running | SessionStatus::AwaitingUser => LoadedSessionStatus::Active,
        SessionStatus::Failed => LoadedSessionStatus::SystemError,
        SessionStatus::Idle | SessionStatus::Completed | SessionStatus::Cancelled => {
            LoadedSessionStatus::Idle
        }
    }
}

fn latest_state_snapshot_from_history(items: &[HistoryItem]) -> SessionStateSnapshot {
    items
        .iter()
        .rev()
        .find_map(|item| match &item.payload {
            HistoryItemPayload::SessionState { state } => Some(state.clone()),
            _ => None,
        })
        .unwrap_or_default()
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
        .map(|value| value.trim().to_string())
        .transpose_non_empty("session settings base URL")?;
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

fn normalize_session_interrupt_reason(reason: String) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        "Interrupted by user.".to_string()
    } else {
        reason.to_string()
    }
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
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::config::{AccessMode, ResolvedConfig};
    use crate::protocol::UserInputItem;
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
    ) -> (String, TurnId) {
        let repository = service.store.session_repo();
        let admission_id = repository
            .admit_session_run(session_id)
            .await
            .expect("admit run")
            .expect("run admitted");
        let turn_id = TurnId::new();
        assert!(
            repository
                .activate_admitted_turn(session_id, &admission_id, turn_id)
                .await
                .expect("activate turn")
        );
        (admission_id, turn_id)
    }

    async fn terminalize_admitted_session(
        service: &SessionService,
        session_id: SessionId,
        turn_id: TurnId,
    ) {
        let repository = service.store.session_repo();
        assert!(
            repository
                .terminalize_active_session_with_protocol_event(
                    session_id,
                    SessionStatus::Completed,
                    &RunEvent::SessionCompleted {
                        session_id,
                        finish_reason: None,
                    },
                    turn_id,
                    None,
                )
                .await
                .expect("complete admitted session")
        );
    }

    async fn assert_cancelled_admission(
        service: &SessionService,
        session_id: SessionId,
        admission_id: &str,
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
        assert_eq!(
            repository
                .renew_admitted_run_lease(session_id, admission_id, turn_id)
                .await
                .expect("terminal heartbeat"),
            crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome::GracefulTerminal
        );
        assert_eq!(
            repository
                .corroborated_terminal_status_for_turn(session_id, turn_id)
                .await
                .expect("protocol terminal status"),
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

    async fn create_session_tree(
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

    fn turn_context(session_id: SessionId, workspace: &Workspace) -> crate::protocol::TurnContext {
        crate::protocol::TurnContext {
            session_id,
            cwd: workspace.cwd.clone(),
            workspace_root: workspace.root.clone(),
            provider: "test".to_string(),
            model: "model".to_string(),
            base_url: "http://localhost:1234".to_string(),
            access_mode: AccessMode::Default,
            sandbox: crate::protocol::SandboxProfile::WorkspaceWrite,
            shell_family: crate::config::ShellFamily::PowerShell,
            model_capabilities: crate::protocol::ModelCapabilities {
                supports_tools: true,
                supports_reasoning: false,
                supports_images: false,
                parallel_tool_calls: false,
                context_window: 8192,
                max_output_tokens: 1024,
            },
            route: crate::session::TaskRoute::Code,
            process_phase: crate::session::ProcessPhase::Discover,
            active_contract: crate::protocol::ActiveWorkContractProjection {
                route: crate::session::TaskRoute::Code,
                process_phase: crate::session::ProcessPhase::Discover,
                active_work_kind: None,
                summary: "test".to_string(),
                active_targets: Vec::new(),
                operation_intents: Vec::new(),
                required_verification_commands: Vec::new(),
                allowed_tools: Vec::new(),
                forbidden_tools: Vec::new(),
                projection_id: crate::protocol::ProjectionId::new(),
            },
            allowed_tools: Vec::new(),
            tool_choice: crate::protocol::ToolChoice::Auto,
            images: Vec::new(),
            output_contract: crate::protocol::OutputContract {
                final_answer_required: true,
                structured_schema_name: None,
                history_markdown_projection: true,
            },
            continuation: None,
            turn_decision_projection: None,
        }
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
    async fn active_run_blocks_session_project_delete_and_manual_compaction() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        let token = CancellationToken::new();
        let _lease = service
            .store
            .active_runs()
            .try_start(session.session.id, token)
            .expect("active run");

        assert!(service.delete_session(session.session.id).await.is_err());
        assert!(service.delete_project(workspace.project_id).await.is_err());
        assert!(
            service
                .compact_session(session.session.id, 20)
                .await
                .is_err()
        );
        assert!(service.get_session(session.session.id).await.is_ok());
    }

    #[tokio::test]
    async fn disabled_compaction_preserves_canonical_history() {
        let (service, workspace, _) = service_fixture().await;
        let session = create_session(&service, &workspace).await;
        let turn_id = TurnId::new();
        let user_turn = UserTurn {
            turn_id,
            items: vec![UserInputItem::Text {
                text: "keep this history".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            context: turn_context(session.session.id, &workspace),
        };
        let admission_id = service
            .store
            .session_repo()
            .admit_session_run(session.session.id)
            .await
            .expect("admit run")
            .expect("run admitted");
        service
            .store_user_thread_op_with_protocol_bundle(
                &session,
                &admission_id,
                &user_turn,
                None,
                SessionStateSnapshot::default(),
                turn_id,
                0,
            )
            .await
            .expect("store user");
        service
            .store
            .session_repo()
            .terminalize_active_session_with_protocol_event(
                session.session.id,
                SessionStatus::Completed,
                &RunEvent::SessionCompleted {
                    session_id: session.session.id,
                    finish_reason: None,
                },
                turn_id,
                None,
            )
            .await
            .expect("complete session");
        let before = service
            .canonical_history_items(session.session.id)
            .await
            .expect("before");

        let error = service
            .compact_session(session.session.id, 1)
            .await
            .expect_err("compaction unavailable");
        let after = service
            .canonical_history_items(session.session.id)
            .await
            .expect("after");

        assert!(error.to_string().contains("history was left unchanged"));
        assert_eq!(
            before.iter().map(|item| item.id).collect::<Vec<_>>(),
            after.iter().map(|item| item.id).collect::<Vec<_>>()
        );
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
            context: turn_context(session.session.id, &workspace),
        };
        let admission_id = service
            .store
            .session_repo()
            .admit_session_run(session.session.id)
            .await
            .expect("admit run")
            .expect("run admitted");
        service
            .store_user_thread_op_with_protocol_bundle(
                &session,
                &admission_id,
                &user_turn,
                None,
                SessionStateSnapshot::default(),
                turn_id,
                0,
            )
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
            .update_session_memory_mode(session.session.id, SessionMemoryMode::Disabled)
            .await
            .expect("disable memory");
        service
            .set_session_archived(session.session.id, true)
            .await
            .expect("archive idle session");
        service
            .store
            .session_repo()
            .admit_session_run(session.session.id)
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
        assert_eq!(summary.memory_mode, SessionMemoryMode::Disabled);
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
        assert_eq!(searched_summary.memory_mode, SessionMemoryMode::Disabled);
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
        let (archived, memory_mode) = service
            .store
            .session_repo()
            .session_projection_state(session.session.id)
            .await
            .expect("projection state");
        assert!(!archived);
        assert_eq!(memory_mode, SessionMemoryMode::Disabled);
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

        let recovery_candidates = service
            .store
            .session_repo()
            .list_running_sessions_for_recovery()
            .await
            .expect("recovery candidates");
        assert!(
            recovery_candidates
                .iter()
                .any(|session| session.id == child.session.id),
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
                .admitted_run_status(child.session.id, &child_admission, child_turn)
                .await
                .expect("fresh child admission"),
            Some(SessionStatus::Running)
        );
    }

    #[tokio::test]
    async fn startup_recovery_terminalizes_hidden_child_without_an_owner() {
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
        service
            .store
            .session_repo()
            .set_status_with_protocol_event(
                child.session.id,
                SessionStatus::Running,
                &RunEvent::SessionStarted {
                    session_id: child.session.id,
                    title: child.session.title.clone(),
                },
                TurnId::new(),
                None,
            )
            .await
            .expect("mark unowned child running");

        assert_eq!(
            service
                .mark_stale_running_sessions("stale child recovery")
                .await
                .expect("stale recovery"),
            1
        );
        assert_eq!(
            service
                .get_session(child.session.id)
                .await
                .expect("recovered child session")
                .status,
            SessionStatus::Cancelled
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
        assert_eq!(
            service
                .get_session(session.session.id)
                .await
                .expect("recovered session")
                .status,
            SessionStatus::Cancelled
        );
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
            .admit_session_run(session.session.id)
            .await
            .expect("replacement admission")
            .expect("recovered session is immediately reusable");
        assert_ne!(replacement, crashed_admission);
    }

    #[tokio::test]
    async fn startup_recovery_preserves_a_run_owned_by_another_process() {
        let (owner, recovery, workspace) = cross_process_service_fixture().await;
        let session = create_session(&owner, &workspace).await;
        owner
            .store
            .session_repo()
            .set_status_with_protocol_event(
                session.session.id,
                SessionStatus::Running,
                &RunEvent::SessionStarted {
                    session_id: session.session.id,
                    title: session.session.title.clone(),
                },
                TurnId::new(),
                None,
            )
            .await
            .expect("mark process-owned session running");
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
        for session in [&root.session, &child.session] {
            owner
                .store
                .session_repo()
                .set_status_with_protocol_event(
                    session.id,
                    SessionStatus::Running,
                    &RunEvent::SessionStarted {
                        session_id: session.id,
                        title: session.title.clone(),
                    },
                    TurnId::new(),
                    None,
                )
                .await
                .expect("mark session running");
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
        assert_eq!(
            recovery
                .get_session(root.session.id)
                .await
                .expect("root session")
                .status,
            SessionStatus::Cancelled
        );
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
        let (root, middle, leaf, sibling) = create_session_tree(&owner, &workspace).await;
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
                .cancel_running_session(root.session.id, "cross-process root stop")
                .await
                .expect("root cancellation")
        );

        assert_cancelled_admission(&owner, root.session.id, &root_admission, root_turn).await;
        assert_cancelled_admission(&owner, middle.session.id, &middle_admission, middle_turn).await;
        assert_cancelled_admission(&owner, leaf.session.id, &leaf_admission, leaf_turn).await;
        assert_cancelled_admission(&owner, sibling.session.id, &sibling_admission, sibling_turn)
            .await;
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
                .release_stopped_run_admission(root.session.id, &root_admission)
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
            .try_start(child.session.id, CancellationToken::new())
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
                .release_stopped_run_admission(child.session.id, &child_admission)
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
    async fn cross_process_middle_child_cancel_terminalizes_only_its_descendants() {
        let (owner, canceller, workspace) = cross_process_service_fixture().await;
        let (root, middle, leaf, sibling) = create_session_tree(&owner, &workspace).await;
        let (root_admission, root_turn) = admit_session_turn(&owner, root.session.id).await;
        let (middle_admission, middle_turn) = admit_session_turn(&owner, middle.session.id).await;
        let (leaf_admission, leaf_turn) = admit_session_turn(&owner, leaf.session.id).await;
        let (sibling_admission, sibling_turn) =
            admit_session_turn(&owner, sibling.session.id).await;

        assert!(
            canceller
                .cancel_running_session(middle.session.id, "cross-process subtree stop")
                .await
                .expect("middle cancellation")
        );

        assert_cancelled_admission(&owner, middle.session.id, &middle_admission, middle_turn).await;
        assert_cancelled_admission(&owner, leaf.session.id, &leaf_admission, leaf_turn).await;
        for (session_id, admission_id, turn_id) in [
            (root.session.id, root_admission.as_str(), root_turn),
            (sibling.session.id, sibling_admission.as_str(), sibling_turn),
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
            assert_eq!(
                owner
                    .store
                    .session_repo()
                    .corroborated_terminal_status_for_turn(session_id, turn_id)
                    .await
                    .expect("unaffected protocol"),
                None
            );
        }
    }
}
