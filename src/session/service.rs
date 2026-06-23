use std::fs;

use crate::error::SessionError;
use crate::protocol::{
    CompactionMode, HistoryItem, HistoryItemId, HistoryItemPayload, ProtocolEventStore,
    RuntimeEvent, RuntimeEventId, RuntimeEventMsg, SteerTurn, TurnId, TurnItem, TurnItemId,
    TurnItemPayload, TurnTerminalStatus, UserTurn,
};
use crate::runtime::{Clock, SystemClock};
use crate::session::{
    CanonicalHistoryPage, CanonicalRuntimeEventPage, CanonicalSessionRead, CanonicalTurnPage,
    ContinuationContract, IdleTurnAdmission, IdleTurnRejectionReason, LoadedSessionList,
    LoadedSessionStatus, LoadedSessionSummary, MessageMetadata, MessagePart, MessageRole,
    NewMessage, NewPart, NewSession, PartKind, ProjectId, ProjectRecord, ProjectRepository,
    RunEvent, RunningSessionRejoin, SessionCompactResult, SessionContext, SessionForkResult,
    SessionId, SessionMemoryMode, SessionMemoryModeUpdate, SessionRecord, SessionRepository,
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
        let session = match request.selector {
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
            SessionSelector::ById(id) => repository.get_session(id).await?,
            SessionSelector::Latest => repository
                .latest_session(workspace.project_id)
                .await?
                .ok_or_else(|| SessionError::Message("no recent session exists".to_string()))?,
        };

        if session.status == SessionStatus::Running {
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

    pub async fn store_user_thread_op_with_protocol_bundle(
        &self,
        ctx: &SessionContext,
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
        let session = self.store.session_repo().get_session(session_id).await?;
        if session.status != SessionStatus::Running {
            return Ok(false);
        }
        self.terminalize_running_session(
            session_id,
            SessionStatus::Cancelled,
            RunEvent::SessionInterrupted {
                session_id,
                reason: reason.to_string(),
            },
            reason,
        )
        .await?;
        Ok(true)
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

    pub async fn validate_active_turn_steer(
        &self,
        session_id: crate::session::SessionId,
        expected_turn_id: TurnId,
    ) -> Result<TurnId, SessionError> {
        let session = self.store.session_repo().get_session(session_id).await?;
        if session.status != SessionStatus::Running {
            return Err(SessionError::Message(format!(
                "no active running turn to steer for session {}; current status is {}",
                session.id,
                session.status.key()
            )));
        }
        let Some((active_turn_id, _sequence_no)) = self
            .store
            .protocol_event_store()
            .latest_turn_position_for_session(session_id)
            .map_err(|error| SessionError::Message(error.to_string()))?
        else {
            return Err(SessionError::Message(format!(
                "running session {} has no recorded active turn to steer",
                session.id
            )));
        };
        if active_turn_id != expected_turn_id {
            return Err(SessionError::Message(format!(
                "expected active turn id `{}` but current active turn id is `{}`",
                expected_turn_id, active_turn_id
            )));
        }
        Ok(active_turn_id)
    }

    pub async fn store_active_turn_steer(
        &self,
        session_id: crate::session::SessionId,
        steer: &SteerTurn,
    ) -> Result<(), SessionError> {
        self.validate_active_turn_steer(session_id, steer.expected_turn_id)
            .await?;
        let (_, sequence_no) = self
            .store
            .protocol_event_store()
            .latest_turn_position_for_session(session_id)
            .map_err(|error| SessionError::Message(error.to_string()))?
            .ok_or_else(|| {
                SessionError::Message(format!(
                    "running session {} has no recorded active turn to steer",
                    session_id
                ))
            })?;
        let now = SystemClock.now_ms();
        let history_item = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id: steer.expected_turn_id,
            sequence_no,
            created_at_ms: now,
            payload: HistoryItemPayload::SteerTurn {
                expected_turn_id: steer.expected_turn_id,
                content: steer.content_parts(),
                additional_context: steer.additional_context.clone(),
                client_user_message_id: steer.client_user_message_id.clone(),
            },
        };
        let turn_item = TurnItem {
            id: TurnItemId::new(),
            session_id,
            turn_id: steer.expected_turn_id,
            source_item_id: Some(history_item.id),
            sequence_no,
            payload: TurnItemPayload::SteerMessage { text: steer.text() },
        };
        let event = RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id: steer.expected_turn_id,
            sequence_no,
            created_at_ms: now,
            msg: RuntimeEventMsg::SteerInputAccepted {
                item_count: steer.items.len(),
                client_user_message_id: steer.client_user_message_id.clone(),
            },
        };
        self.store
            .protocol_event_store()
            .append_event_bundle(&event, Some(&history_item), Some(&turn_item))
            .map_err(|error| SessionError::Message(error.to_string()))?;
        Ok(())
    }

    pub async fn mark_stale_running_sessions(&self, reason: &str) -> Result<usize, SessionError> {
        let sessions = self
            .store
            .session_repo()
            .list_recent_sessions(10_000)
            .await?;
        let mut cancelled = 0;
        for session in sessions {
            if session.status == SessionStatus::Running
                && self.cancel_running_session(session.id, reason).await?
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
        unfinished_tool_reason: &str,
    ) -> Result<(), SessionError> {
        let (turn_id, sequence_no) = self
            .store
            .protocol_event_store()
            .latest_turn_position_for_session(session_id)?
            .unwrap_or_else(|| (TurnId::new(), 0));
        self.store
            .session_repo()
            .set_status_with_protocol_event(session_id, status, &event, turn_id, Some(sequence_no))
            .await?;
        self.store
            .session_repo()
            .fail_unfinished_tool_calls(session_id, unfinished_tool_reason)
            .await?;
        Ok(())
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
        if session.status == SessionStatus::Running {
            return Err(SessionError::Message(format!(
                "session {} is running; settings update requires an idle or terminal session",
                session.id
            )));
        }
        let normalized = normalize_session_settings_patch(patch)?;
        Ok(self
            .store
            .session_repo()
            .update_session_settings(session_id, &normalized)
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
        if session.status == SessionStatus::Running {
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
        let source = self
            .store
            .session_repo()
            .get_session(source_session_id)
            .await?;
        let source_was_active = matches!(
            source.status,
            SessionStatus::Running | SessionStatus::AwaitingUser
        );
        let title = title
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_string())
            .unwrap_or_else(|| format!("Fork of {}", source.title));
        let forked = self
            .store
            .session_repo()
            .create_session(NewSession {
                project_id: source.project_id,
                title,
                cwd: source.cwd.clone(),
                model: source.model.clone(),
                base_url: source.base_url.clone(),
                access_mode: source.access_mode,
            })
            .await?;
        if !source.model_parameters.is_empty() {
            self.store
                .session_repo()
                .update_session_settings(
                    forked.id,
                    &SessionSettingsPatch {
                        temperature: source.model_parameters.temperature,
                        top_p: source.model_parameters.top_p,
                        top_k: source.model_parameters.top_k,
                        max_output_tokens: source.model_parameters.max_output_tokens,
                        ..SessionSettingsPatch::default()
                    },
                )
                .await?;
        }
        self.store
            .session_repo()
            .update_session_memory_mode(
                forked.id,
                self.store
                    .session_repo()
                    .get_session_memory_mode(source.id)
                    .await?,
            )
            .await?;
        let (copied_history_items, copied_turn_items) = self
            .store
            .protocol_event_store()
            .fork_canonical_items(source.id, forked.id)
            .map_err(|error| SessionError::Message(error.to_string()))?;
        self.store
            .session_repo()
            .copy_session_state_and_todos(source.id, forked.id)
            .await?;
        if source_was_active {
            self.append_interrupted_live_snapshot_marker(
                forked.id,
                "forked from active live session snapshot",
            )
            .await?;
        }
        let forked_session = self.store.session_repo().get_session(forked.id).await?;
        Ok(SessionForkResult {
            source_session: source,
            forked_session,
            copied_history_items,
            copied_turn_items,
            interrupted_live_snapshot: source_was_active,
        })
    }

    async fn append_interrupted_live_snapshot_marker(
        &self,
        session_id: SessionId,
        reason: &str,
    ) -> Result<(), SessionError> {
        let (turn_id, sequence_no) = self
            .store
            .protocol_event_store()
            .latest_turn_position_for_session(session_id)?
            .unwrap_or_else(|| (TurnId::new(), 0));
        let now = SystemClock.now_ms();
        let history_item = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no,
            created_at_ms: now,
            payload: HistoryItemPayload::Error {
                message_id: None,
                message: reason.to_string(),
            },
        };
        let turn_item = TurnItem {
            id: TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: Some(history_item.id),
            sequence_no,
            payload: TurnItemPayload::Terminal {
                status: TurnTerminalStatus::Interrupted,
                summary: reason.to_string(),
            },
        };
        let event = RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no,
            created_at_ms: now,
            msg: RuntimeEventMsg::TurnInterrupted {
                reason: reason.to_string(),
            },
        };
        self.store
            .protocol_event_store()
            .append_event_bundle(&event, Some(&history_item), Some(&turn_item))
            .map_err(|error| SessionError::Message(error.to_string()))?;
        Ok(())
    }

    pub async fn compact_session(
        &self,
        session_id: SessionId,
        keep_recent: usize,
    ) -> Result<SessionCompactResult, SessionError> {
        if keep_recent == 0 {
            return Err(SessionError::Message(
                "session compact --keep-recent must be greater than zero".to_string(),
            ));
        }
        let session = self.store.session_repo().get_session(session_id).await?;
        let history = self.canonical_history_items(session_id).await?;
        if history.len() <= keep_recent {
            return Err(SessionError::Message(format!(
                "session {} has {} canonical history item(s); compact requires more than --keep-recent {}",
                session.id,
                history.len(),
                keep_recent
            )));
        }
        let summarized_count = history.len() - keep_recent;
        let summarized_ids = history
            .iter()
            .take(summarized_count)
            .map(|item| item.id)
            .collect::<Vec<_>>();
        let continuation = compact_continuation_from_state(&self.load_state(session_id).await?);
        let summary = manual_compaction_summary(&session, summarized_count, keep_recent);
        let (turn_id, sequence_no) = self
            .store
            .protocol_event_store()
            .latest_turn_position_for_session(session_id)?
            .unwrap_or_else(|| (TurnId::new(), 0));
        let now = SystemClock.now_ms();
        let history_item = HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no,
            created_at_ms: now,
            payload: HistoryItemPayload::Compaction {
                mode: CompactionMode::Manual,
                summary: summary.clone(),
                replacement_item_ids: summarized_ids,
                continuation,
            },
        };
        let turn_item = TurnItem {
            id: TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: Some(history_item.id),
            sequence_no,
            payload: TurnItemPayload::ContextCompaction {
                summary: summary.clone(),
            },
        };
        let event = RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no,
            created_at_ms: now,
            msg: RuntimeEventMsg::ContextCompacted {
                item_id: history_item.id,
                mode: CompactionMode::Manual,
            },
        };
        self.store
            .protocol_event_store()
            .append_event_bundle(&event, Some(&history_item), Some(&turn_item))
            .map_err(|error| SessionError::Message(error.to_string()))?;
        Ok(SessionCompactResult {
            session,
            compaction_item_id: history_item.id,
            summarized_history_items: summarized_count,
            retained_history_items: keep_recent,
        })
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
            .list_sessions_with_archived(project_id, limit, include_archived)
            .await?;
        let mut summaries = Vec::with_capacity(sessions.len());
        for session in sessions {
            summaries.push(self.loaded_session_summary(session).await?);
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
        Ok(self.store.session_repo().delete_session(session_id).await?)
    }

    pub async fn delete_project(&self, project_id: ProjectId) -> Result<(), SessionError> {
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

fn manual_compaction_summary(
    session: &SessionRecord,
    summarized_history_items: usize,
    retained_history_items: usize,
) -> String {
    let snapshot_kind = if matches!(
        session.status,
        SessionStatus::Running | SessionStatus::AwaitingUser
    ) {
        "Active live-turn manual compaction snapshot"
    } else {
        "Manual compaction snapshot"
    };
    format!(
        "Summarized history items: {summarized_history_items}.\nContinuation invariant: CompactionContinuity.\nRetained recent history items: {retained_history_items}.\n{snapshot_kind} for session `{}`.",
        session.title
    )
}

fn compact_continuation_from_state(state: &SessionStateSnapshot) -> Option<ContinuationContract> {
    let has_payload = !state.active_targets.is_empty()
        || !state.verification.required_commands.is_empty()
        || state.failure.is_some()
        || state.completion.blocked_reason.is_some();
    if !has_payload {
        return None;
    }
    Some(ContinuationContract {
        route: state.route.key().to_string(),
        process_phase: state.process_phase.key().to_string(),
        active_work_kind: Some("manual_compaction_continuity".to_string()),
        active_work_summary: state.completion.blocked_reason.clone().or_else(|| {
            state
                .failure
                .as_ref()
                .map(|failure| failure.summary.clone())
        }),
        target_files: state.active_targets.clone(),
        verification_commands: state.verification.required_commands.clone(),
        failure_kind: state
            .failure
            .as_ref()
            .map(|failure| format!("{:?}", failure.kind)),
        failure_summary: state
            .failure
            .as_ref()
            .map(|failure| failure.summary.clone()),
        completion_blocker: state.completion.blocked_reason.clone(),
        invariant_refs: vec!["CompactionContinuity".to_string()],
        ..ContinuationContract::default()
    })
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
mod tests {}
