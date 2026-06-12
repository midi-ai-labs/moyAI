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

const SESSION_SERVICE_FIXTURE_MODEL: &str = "qwen/qwen3.6-35b-a3b";
const SESSION_SERVICE_FIXTURE_BASE_URL: &str = "http://127.0.0.1:1234";

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
        } else if session.status != SessionStatus::Idle {
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

pub(crate) fn session_service_current_provider_profile_fixture_passes() -> bool {
    SESSION_SERVICE_FIXTURE_MODEL == "qwen/qwen3.6-35b-a3b"
        && SESSION_SERVICE_FIXTURE_BASE_URL == "http://127.0.0.1:1234"
        && stale_running_cleanup_records_protocol_terminal_fixture_passes()
}

pub(crate) fn stale_running_cleanup_records_protocol_terminal_fixture_passes() -> bool {
    std::thread::spawn(|| {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => return false,
        };
        runtime.block_on(async {
            let temp = match tempfile::tempdir() {
                Ok(temp) => temp,
                Err(_) => return false,
            };
            let data_dir = match camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) {
                Ok(path) => path,
                Err(_) => return false,
            };
            let paths = crate::storage::StoragePaths {
                data_dir: data_dir.clone(),
                database_path: data_dir.join("moyai.sqlite3"),
                truncation_dir: data_dir.join("truncation"),
            };
            let store = match crate::storage::SqliteStore::open(&paths) {
                Ok(store) => store,
                Err(_) => return false,
            };
            if store.migrate().is_err() {
                return false;
            }
            let service = SessionService::new(StoreBundle::new(store));
            let project_id = ProjectId::new();
            if service
                .store
                .project_repo()
                .upsert_project(
                    project_id,
                    camino::Utf8Path::new("C:/workspace"),
                    "workspace",
                    "none",
                )
                .await
                .is_err()
            {
                return false;
            }
            let repo = service.store.session_repo();
            let running = match repo
                .create_session(NewSession {
                    project_id,
                    title: "Running".to_string(),
                    cwd: "C:/workspace".into(),
                    model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                    base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                    access_mode: crate::config::AccessMode::Default,
                })
                .await
            {
                Ok(session) => session,
                Err(_) => return false,
            };
            if repo
                .set_status_with_protocol_event(
                    running.id,
                    SessionStatus::Running,
                    &RunEvent::SessionStarted {
                        session_id: running.id,
                        title: running.title.clone(),
                    },
                    TurnId::new(),
                    Some(0),
                )
                .await
                .is_err()
            {
                return false;
            }
            if service
                .mark_stale_running_sessions("desktop restart")
                .await
                .ok()
                != Some(1)
            {
                return false;
            }
            let history_items = match service.canonical_history_items(running.id).await {
                Ok(items) => items,
                Err(_) => return false,
            };
            let turn_items = match service.canonical_turn_items(running.id).await {
                Ok(items) => items,
                Err(_) => return false,
            };
            history_items.iter().any(|item| {
                matches!(
                    &item.payload,
                    crate::protocol::HistoryItemPayload::Error { message, .. }
                        if message == "desktop restart"
                )
            }) && turn_items.iter().any(|item| {
                matches!(
                    &item.payload,
                    crate::protocol::TurnItemPayload::Terminal {
                        status: crate::protocol::TurnTerminalStatus::Interrupted,
                        summary,
                    } if summary == "desktop restart"
                )
            })
        })
    })
    .join()
    .unwrap_or(false)
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
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::protocol::{
        AdditionalContextEntry, AdditionalContextKind, ContentPart, HistoryItem, HistoryItemId,
        HistoryItemPayload, ProtocolEventStore, RuntimeEventMsg, SteerTurn, TurnId, TurnItem,
        TurnItemId, TurnItemPayload, TurnTerminalStatus, UserInputItem,
    };
    use crate::session::{NewSession, ProjectRepository, SessionRepository};
    use crate::storage::{SqliteStore, StoragePaths, StoreBundle};

    fn test_service() -> (tempfile::TempDir, SessionService) {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("open sqlite");
        store.migrate().expect("migrate sqlite");
        (temp, SessionService::new(StoreBundle::new(store)))
    }

    #[tokio::test]
    async fn stale_running_sessions_are_cancelled_on_desktop_restart_cleanup() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        let repo = service.store.session_repo();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let running = repo
            .create_session(NewSession {
                project_id,
                title: "Running".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create running session");
        repo.set_status_with_protocol_event(
            running.id,
            SessionStatus::Running,
            &RunEvent::SessionStarted {
                session_id: running.id,
                title: running.title.clone(),
            },
            TurnId::new(),
            Some(0),
        )
        .await
        .expect("mark running");
        let idle = repo
            .create_session(NewSession {
                project_id,
                title: "Idle".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create idle session");

        let cancelled = service
            .mark_stale_running_sessions("desktop restart")
            .await
            .expect("cleanup stale running sessions");

        assert_eq!(cancelled, 1);
        assert_eq!(
            repo.get_session(running.id)
                .await
                .expect("reload running")
                .status,
            SessionStatus::Cancelled
        );
        assert_eq!(
            repo.get_session(idle.id).await.expect("reload idle").status,
            SessionStatus::Idle
        );
        let history_items = service
            .canonical_history_items(running.id)
            .await
            .expect("canonical history items");
        assert!(history_items.iter().any(|item| {
            matches!(
                &item.payload,
                HistoryItemPayload::Error { message, .. } if message == "desktop restart"
            )
        }));
        let turn_items = service
            .canonical_turn_items(running.id)
            .await
            .expect("canonical turn items");
        assert!(turn_items.iter().any(|item| {
            matches!(
                &item.payload,
                TurnItemPayload::Terminal {
                    status: TurnTerminalStatus::Interrupted,
                    summary,
                } if summary == "desktop restart"
            )
        }));
    }

    #[tokio::test]
    async fn session_interrupt_terminalizes_running_session_with_protocol_event() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        let repo = service.store.session_repo();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let running = repo
            .create_session(NewSession {
                project_id,
                title: "Interrupt target".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create running session");
        let active_turn_id = TurnId::new();
        repo.set_status_with_protocol_event(
            running.id,
            SessionStatus::Running,
            &RunEvent::SessionStarted {
                session_id: running.id,
                title: running.title.clone(),
            },
            active_turn_id,
            Some(0),
        )
        .await
        .expect("mark running");
        let idle = repo
            .create_session(NewSession {
                project_id,
                title: "Idle".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create idle session");

        let interrupted = service
            .interrupt_running_session(running.id, "  user stop requested  ".to_string())
            .await
            .expect("interrupt running session");

        assert_eq!(interrupted.status, SessionStatus::Cancelled);
        let history_items = service
            .canonical_history_items(running.id)
            .await
            .expect("canonical history items");
        assert!(history_items.iter().any(|item| {
            matches!(
                &item.payload,
                HistoryItemPayload::Error { message, .. } if message == "user stop requested"
            )
        }));
        let turn_items = service
            .canonical_turn_items(running.id)
            .await
            .expect("canonical turn items");
        assert!(turn_items.iter().any(|item| {
            matches!(
                &item.payload,
                TurnItemPayload::Terminal {
                    status: TurnTerminalStatus::Interrupted,
                    summary,
                } if summary == "user stop requested"
            )
        }));
        assert!(matches!(
            service
                .interrupt_running_session(idle.id, String::new())
                .await,
            Err(SessionError::Message(message))
                if message.contains("interrupt requires a running session")
        ));
    }

    #[tokio::test]
    async fn idle_turn_admission_matches_codex_rejection_order() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        let repo = service.store.session_repo();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let idle = repo
            .create_session(NewSession {
                project_id,
                title: "Idle".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create idle session");
        let running = repo
            .create_session(NewSession {
                project_id,
                title: "Running".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create running session");
        repo.set_status_with_protocol_event(
            running.id,
            SessionStatus::Running,
            &RunEvent::SessionStarted {
                session_id: running.id,
                title: running.title.clone(),
            },
            TurnId::new(),
            Some(0),
        )
        .await
        .expect("mark running");

        let admitted = service
            .evaluate_idle_turn_admission(idle.id, false, false)
            .await
            .expect("idle admission");
        assert!(admitted.admitted);
        assert_eq!(admitted.rejection_reason, None);

        let pending = service
            .evaluate_idle_turn_admission(running.id, true, true)
            .await
            .expect("pending trigger rejection");
        assert!(!pending.admitted);
        assert_eq!(
            pending.rejection_reason,
            Some(IdleTurnRejectionReason::PendingTriggerTurn)
        );

        let plan = service
            .evaluate_idle_turn_admission(idle.id, false, true)
            .await
            .expect("plan rejection");
        assert!(!plan.admitted);
        assert_eq!(
            plan.rejection_reason,
            Some(IdleTurnRejectionReason::PlanMode)
        );

        let busy = service
            .evaluate_idle_turn_admission(running.id, false, false)
            .await
            .expect("busy rejection");
        assert!(!busy.admitted);
        assert_eq!(busy.rejection_reason, Some(IdleTurnRejectionReason::Busy));
        assert_eq!(
            repo.get_session(running.id)
                .await
                .expect("reload running")
                .status,
            SessionStatus::Running
        );
    }

    #[tokio::test]
    async fn start_or_resume_rejects_running_session_without_terminalizing_it() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let repo = service.store.session_repo();
        let running = repo
            .create_session(NewSession {
                project_id,
                title: "Running".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create running session");
        repo.set_status_with_protocol_event(
            running.id,
            SessionStatus::Running,
            &RunEvent::SessionStarted {
                session_id: running.id,
                title: running.title.clone(),
            },
            TurnId::new(),
            Some(0),
        )
        .await
        .expect("mark running");

        let result = service
            .start_or_resume(
                SessionStartRequest {
                    selector: SessionSelector::ById(running.id),
                    title: None,
                    cwd: camino::Utf8PathBuf::from("C:/workspace"),
                    model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                    base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                    access_mode: crate::config::AccessMode::Default,
                },
                Workspace {
                    project_id,
                    root: camino::Utf8PathBuf::from("C:/workspace"),
                    cwd: camino::Utf8PathBuf::from("C:/workspace"),
                    vcs: crate::workspace::VcsKind::None,
                    ignore: crate::workspace::IgnorePlan::default_with(Vec::new()),
                    protected_paths: Vec::new(),
                    path_policy: crate::workspace::PathPolicy {
                        workspace_root: camino::Utf8PathBuf::from("C:/workspace"),
                        additional_read_roots: Vec::new(),
                        additional_write_roots: Vec::new(),
                    },
                },
            )
            .await;

        assert!(matches!(
            result,
            Err(SessionError::Message(message))
                if message.contains("already running")
                    && message.contains("active-turn steer/rejoin")
        ));
        assert_eq!(
            repo.get_session(running.id)
                .await
                .expect("reload running")
                .status,
            SessionStatus::Running
        );
    }

    #[tokio::test]
    async fn loaded_sessions_project_active_running_without_terminalizing() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let repo = service.store.session_repo();
        let running = repo
            .create_session(NewSession {
                project_id,
                title: "Loaded running".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create running session");
        let active_turn_id = TurnId::new();
        repo.set_status_with_protocol_event(
            running.id,
            SessionStatus::Running,
            &RunEvent::SessionStarted {
                session_id: running.id,
                title: running.title.clone(),
            },
            active_turn_id,
            Some(7),
        )
        .await
        .expect("mark running");

        let loaded = service
            .loaded_sessions(project_id, 10, false)
            .await
            .expect("loaded sessions");
        let summary = loaded
            .sessions
            .iter()
            .find(|summary| summary.session.id == running.id)
            .expect("running session summary");

        assert_eq!(summary.loaded_status, LoadedSessionStatus::Active);
        assert_eq!(summary.active_turn_id, Some(active_turn_id));
        assert!(summary.active_turn_sequence_no.is_some());
        assert_eq!(
            repo.get_session(running.id)
                .await
                .expect("reload running")
                .status,
            SessionStatus::Running
        );
    }

    #[tokio::test]
    async fn rejoin_running_session_returns_canonical_read_without_terminalizing() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let repo = service.store.session_repo();
        let running = repo
            .create_session(NewSession {
                project_id,
                title: "Rejoin running".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create running session");
        let active_turn_id = TurnId::new();
        repo.set_status_with_protocol_event(
            running.id,
            SessionStatus::Running,
            &RunEvent::SessionStarted {
                session_id: running.id,
                title: running.title.clone(),
            },
            active_turn_id,
            Some(11),
        )
        .await
        .expect("mark running");

        let rejoin = service
            .rejoin_running_session(running.id, 0, 25, 0, 25)
            .await
            .expect("rejoin running session");

        assert_eq!(rejoin.summary.loaded_status, LoadedSessionStatus::Active);
        assert_eq!(rejoin.summary.active_turn_id, Some(active_turn_id));
        assert_eq!(rejoin.read.active_turn_id, Some(active_turn_id));
        assert_eq!(
            repo.get_session(running.id)
                .await
                .expect("reload running")
                .status,
            SessionStatus::Running
        );
    }

    #[tokio::test]
    async fn fork_running_session_creates_interrupted_snapshot_without_terminalizing_source() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let repo = service.store.session_repo();
        let running = repo
            .create_session(NewSession {
                project_id,
                title: "Fork active source".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create running session");
        let active_turn_id = TurnId::new();
        repo.set_status_with_protocol_event(
            running.id,
            SessionStatus::Running,
            &RunEvent::SessionStarted {
                session_id: running.id,
                title: running.title.clone(),
            },
            active_turn_id,
            Some(0),
        )
        .await
        .expect("mark running");

        let fork = service
            .fork_session(running.id, Some("Forked live snapshot".to_string()))
            .await
            .expect("fork running session");

        assert!(fork.interrupted_live_snapshot);
        assert_eq!(
            repo.get_session(running.id)
                .await
                .expect("reload source")
                .status,
            SessionStatus::Running
        );
        assert_eq!(fork.forked_session.status, SessionStatus::Idle);
        let fork_history = service
            .canonical_history_items(fork.forked_session.id)
            .await
            .expect("fork history");
        assert!(fork_history.iter().any(|item| {
            matches!(
                &item.payload,
                HistoryItemPayload::Error { message, .. }
                    if message == "forked from active live session snapshot"
            )
        }));
        let fork_turn_items = service
            .canonical_turn_items(fork.forked_session.id)
            .await
            .expect("fork turn items");
        assert!(fork_turn_items.iter().any(|item| {
            matches!(
                &item.payload,
                TurnItemPayload::Terminal {
                    status: TurnTerminalStatus::Interrupted,
                    summary,
                } if summary == "forked from active live session snapshot"
            )
        }));
    }

    #[tokio::test]
    async fn active_turn_steer_admission_requires_running_expected_turn() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let repo = service.store.session_repo();
        let running = repo
            .create_session(NewSession {
                project_id,
                title: "Running steer".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create running session");
        let active_turn_id = TurnId::new();
        repo.set_status_with_protocol_event(
            running.id,
            SessionStatus::Running,
            &RunEvent::SessionStarted {
                session_id: running.id,
                title: running.title.clone(),
            },
            active_turn_id,
            Some(0),
        )
        .await
        .expect("mark running");

        assert_eq!(
            service
                .validate_active_turn_steer(running.id, active_turn_id)
                .await
                .expect("active turn steer admission"),
            active_turn_id
        );
        assert!(matches!(
            service
                .validate_active_turn_steer(running.id, TurnId::new())
                .await,
            Err(SessionError::Message(message))
                if message.contains("expected active turn id")
                    && message.contains("current active turn id")
        ));

        let idle = repo
            .create_session(NewSession {
                project_id,
                title: "Idle steer".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create idle session");
        assert!(matches!(
            service
                .validate_active_turn_steer(idle.id, active_turn_id)
                .await,
            Err(SessionError::Message(message))
                if message.contains("no active running turn to steer")
                    && message.contains("idle")
        ));
    }

    #[tokio::test]
    async fn session_settings_update_is_typed_metadata_and_rejects_running_sessions() {
        let (_temp, service) = test_service();
        let workspace = tempfile::tempdir().expect("workspace");
        let next_workspace = tempfile::tempdir().expect("next workspace");
        let cwd = camino::Utf8PathBuf::from_path_buf(workspace.path().to_path_buf())
            .expect("utf8 workspace");
        let next_cwd = camino::Utf8PathBuf::from_path_buf(next_workspace.path().to_path_buf())
            .expect("utf8 next workspace");
        let project_id = ProjectId::new();
        service
            .store
            .project_repo()
            .upsert_project(project_id, &cwd, "workspace", "none")
            .await
            .expect("create project");
        let repo = service.store.session_repo();
        let session = repo
            .create_session(NewSession {
                project_id,
                title: "Settings target".to_string(),
                cwd: cwd.clone(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create session");

        let update = service
            .update_session_settings(
                session.id,
                SessionSettingsPatch {
                    cwd: Some(next_cwd.clone()),
                    model: Some(" qwen/updated ".to_string()),
                    base_url: Some(" http://127.0.0.1:4321 ".to_string()),
                    access_mode: Some(crate::config::AccessMode::AutoReview),
                    temperature: Some(0.2),
                    top_p: Some(0.8),
                    top_k: Some(40),
                    max_output_tokens: Some(4096),
                    ..SessionSettingsPatch::default()
                },
            )
            .await
            .expect("update settings");
        assert!(update.changed);
        assert_eq!(update.session.cwd, next_cwd);
        assert_eq!(update.session.model, "qwen/updated");
        assert_eq!(update.session.base_url, "http://127.0.0.1:4321");
        assert_eq!(
            update.session.access_mode,
            crate::config::AccessMode::AutoReview
        );
        assert_eq!(update.session.model_parameters.temperature, Some(0.2));
        assert_eq!(update.session.model_parameters.top_p, Some(0.8));
        assert_eq!(update.session.model_parameters.top_k, Some(40));
        assert_eq!(
            update.session.model_parameters.max_output_tokens,
            Some(4096)
        );

        let reset = service
            .update_session_settings(
                session.id,
                SessionSettingsPatch {
                    reset_model_parameters: true,
                    ..SessionSettingsPatch::default()
                },
            )
            .await
            .expect("reset model parameters");
        assert!(reset.changed);
        assert!(reset.session.model_parameters.is_empty());

        repo.set_status_with_protocol_event(
            session.id,
            SessionStatus::Running,
            &RunEvent::SessionStarted {
                session_id: session.id,
                title: session.title.clone(),
            },
            TurnId::new(),
            Some(0),
        )
        .await
        .expect("mark running");
        assert!(matches!(
            service
                .update_session_settings(
                    session.id,
                    SessionSettingsPatch {
                    cwd: None,
                    model: Some("qwen/while-running".to_string()),
                    base_url: None,
                    access_mode: Some(crate::config::AccessMode::FullAccess),
                    temperature: Some(0.1),
                    ..SessionSettingsPatch::default()
                },
            )
            .await,
            Err(SessionError::Message(message))
                if message.contains("settings update requires an idle or terminal session")
        ));
        let stored = repo.get_session(session.id).await.expect("stored session");
        assert_eq!(stored.model, "qwen/updated");
        assert_eq!(stored.access_mode, crate::config::AccessMode::AutoReview);
        assert!(stored.model_parameters.is_empty());
        assert_eq!(stored.status, SessionStatus::Running);
    }

    #[tokio::test]
    async fn session_title_update_is_metadata_and_preserves_running_status() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let repo = service.store.session_repo();
        let session = repo
            .create_session(NewSession {
                project_id,
                title: "Original title".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create session");
        let active_turn_id = TurnId::new();
        repo.set_status_with_protocol_event(
            session.id,
            SessionStatus::Running,
            &RunEvent::SessionStarted {
                session_id: session.id,
                title: session.title.clone(),
            },
            active_turn_id,
            Some(0),
        )
        .await
        .expect("mark running");
        let history_before = service
            .canonical_history_items(session.id)
            .await
            .expect("history before")
            .len();

        let update = service
            .update_session_title(session.id, "  Renamed while running  ".to_string())
            .await
            .expect("update title");

        assert!(update.changed);
        assert_eq!(update.session.title, "Renamed while running");
        assert_eq!(update.session.status, SessionStatus::Running);
        assert_eq!(
            service
                .canonical_history_items(session.id)
                .await
                .expect("history after")
                .len(),
            history_before
        );
        let unchanged = service
            .update_session_title(session.id, "Renamed while running".to_string())
            .await
            .expect("unchanged title");
        assert!(!unchanged.changed);
        assert!(matches!(
            service.update_session_title(session.id, "   ".to_string()).await,
            Err(SessionError::Message(message)) if message.contains("title must not be empty")
        ));
    }

    #[tokio::test]
    async fn session_memory_mode_update_is_metadata_and_rejects_active_sessions() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let repo = service.store.session_repo();
        let session = repo
            .create_session(NewSession {
                project_id,
                title: "Memory target".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create session");

        let update = service
            .update_session_memory_mode(session.id, SessionMemoryMode::Disabled)
            .await
            .expect("disable memory");

        assert!(update.changed);
        assert_eq!(update.mode, SessionMemoryMode::Disabled);
        assert_eq!(
            repo.get_session_memory_mode(session.id)
                .await
                .expect("memory mode"),
            SessionMemoryMode::Disabled
        );
        assert!(
            service
                .canonical_history_items(session.id)
                .await
                .expect("history")
                .is_empty()
        );
        let unchanged = service
            .update_session_memory_mode(session.id, SessionMemoryMode::Disabled)
            .await
            .expect("unchanged memory");
        assert!(!unchanged.changed);

        repo.set_status_with_protocol_event(
            session.id,
            SessionStatus::Running,
            &RunEvent::SessionStarted {
                session_id: session.id,
                title: session.title.clone(),
            },
            TurnId::new(),
            Some(0),
        )
        .await
        .expect("mark running");
        assert!(matches!(
            service
                .update_session_memory_mode(session.id, SessionMemoryMode::Enabled)
                .await,
            Err(SessionError::Message(message))
                if message.contains("memory mode update requires an idle or terminal session")
        ));
        assert_eq!(
            repo.get_session_memory_mode(session.id)
                .await
                .expect("memory mode after failed update"),
            SessionMemoryMode::Disabled
        );
    }

    #[tokio::test]
    async fn session_rollback_drops_latest_canonical_turn_and_restores_state() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let repo = service.store.session_repo();
        let session = repo
            .create_session(NewSession {
                project_id,
                title: "Rollback target".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create session");
        let first_turn = TurnId::new();
        let second_turn = TurnId::new();
        let mut first_state = SessionStateSnapshot::default();
        first_state.active_targets = vec![camino::Utf8PathBuf::from("src/first.rs")];
        let mut second_state = SessionStateSnapshot::default();
        second_state.active_targets = vec![camino::Utf8PathBuf::from("src/second.rs")];

        append_state_history_item(&service, session.id, first_turn, 0, first_state.clone());
        append_state_history_item(&service, session.id, second_turn, 0, second_state);
        repo.reset_state_after_protocol_rollback(
            session.id,
            &SessionStateSnapshot {
                active_targets: vec![camino::Utf8PathBuf::from("src/second.rs")],
                ..SessionStateSnapshot::default()
            },
        )
        .await
        .expect("seed latest state");

        let rollback = service
            .rollback_session(session.id, 1)
            .await
            .expect("rollback latest turn");
        assert_eq!(rollback.dropped_turn_ids, vec![second_turn]);
        assert_eq!(rollback.remaining_history_items, 1);
        assert_eq!(rollback.session.status, SessionStatus::Idle);
        let history = service
            .canonical_history_items(session.id)
            .await
            .expect("history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].turn_id, first_turn);
        let state = service.load_state(session.id).await.expect("state");
        assert_eq!(state.active_targets, first_state.active_targets);
        assert!(
            service
                .canonical_turn_items(session.id)
                .await
                .expect("turn items")
                .iter()
                .all(|item| item.turn_id == first_turn)
        );
    }

    #[tokio::test]
    async fn session_fork_copies_canonical_items_and_state_without_reusing_item_ids() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let repo = service.store.session_repo();
        let source = repo
            .create_session(NewSession {
                project_id,
                title: "Fork source".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::AutoReview,
            })
            .await
            .expect("create source session");
        service
            .update_session_settings(
                source.id,
                SessionSettingsPatch {
                    temperature: Some(0.3),
                    top_p: Some(0.7),
                    top_k: Some(32),
                    max_output_tokens: Some(2048),
                    ..SessionSettingsPatch::default()
                },
            )
            .await
            .expect("seed source model parameters");
        let turn_id = TurnId::new();
        let mut state = SessionStateSnapshot::default();
        state.active_targets = vec![camino::Utf8PathBuf::from("src/forked.rs")];
        append_state_history_item(&service, source.id, turn_id, 0, state.clone());
        repo.reset_state_after_protocol_rollback(source.id, &state)
            .await
            .expect("seed source state");

        let fork = service
            .fork_session(source.id, Some("Forked snapshot".to_string()))
            .await
            .expect("fork source session");
        assert_eq!(fork.source_session.id, source.id);
        assert_ne!(fork.forked_session.id, source.id);
        assert_eq!(fork.forked_session.title, "Forked snapshot");
        assert_eq!(
            fork.forked_session.access_mode,
            crate::config::AccessMode::AutoReview
        );
        assert_eq!(fork.forked_session.model_parameters.temperature, Some(0.3));
        assert_eq!(fork.forked_session.model_parameters.top_p, Some(0.7));
        assert_eq!(fork.forked_session.model_parameters.top_k, Some(32));
        assert_eq!(
            fork.forked_session.model_parameters.max_output_tokens,
            Some(2048)
        );
        assert!(!fork.interrupted_live_snapshot);
        assert_eq!(fork.copied_history_items, 1);
        assert_eq!(fork.copied_turn_items, 1);

        let source_history = service
            .canonical_history_items(source.id)
            .await
            .expect("source history");
        let fork_history = service
            .canonical_history_items(fork.forked_session.id)
            .await
            .expect("fork history");
        assert_eq!(fork_history.len(), source_history.len());
        assert_eq!(fork_history[0].turn_id, source_history[0].turn_id);
        assert_ne!(fork_history[0].id, source_history[0].id);
        assert_eq!(fork_history[0].session_id, fork.forked_session.id);

        let fork_turns = service
            .canonical_turn_items(fork.forked_session.id)
            .await
            .expect("fork turns");
        assert_eq!(fork_turns.len(), 1);
        assert_eq!(fork_turns[0].source_item_id, Some(fork_history[0].id));
        assert_eq!(fork_turns[0].session_id, fork.forked_session.id);
        assert_eq!(
            service
                .load_state(fork.forked_session.id)
                .await
                .expect("fork state")
                .active_targets,
            state.active_targets
        );

        repo.set_status_with_protocol_event(
            source.id,
            SessionStatus::Running,
            &RunEvent::SessionStarted {
                session_id: source.id,
                title: source.title.clone(),
            },
            TurnId::new(),
            Some(0),
        )
        .await
        .expect("mark source running");
        let active_fork = service
            .fork_session(source.id, None)
            .await
            .expect("fork active source");
        assert!(active_fork.interrupted_live_snapshot);
        assert_eq!(
            repo.get_session(source.id)
                .await
                .expect("reload source")
                .status,
            SessionStatus::Running
        );
    }

    #[tokio::test]
    async fn session_compact_appends_manual_compaction_without_deleting_history() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let repo = service.store.session_repo();
        let session = repo
            .create_session(NewSession {
                project_id,
                title: "Manual compact target".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create session");
        let turn_id = TurnId::new();
        for sequence_no in 1..=4 {
            let history_id = HistoryItemId::new();
            let history_item = HistoryItem {
                id: history_id,
                session_id: session.id,
                turn_id,
                sequence_no,
                created_at_ms: sequence_no,
                payload: HistoryItemPayload::Message {
                    message_id: None,
                    role: MessageRole::User,
                    content: vec![ContentPart::Text {
                        text: format!("history item {sequence_no}"),
                    }],
                },
            };
            let turn_item = TurnItem {
                id: TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: Some(history_id),
                sequence_no,
                payload: TurnItemPayload::UserMessage {
                    text: format!("history item {sequence_no}"),
                },
            };
            service
                .store
                .protocol_event_store()
                .append_history_turn_bundle(&history_item, &turn_item)
                .expect("append history item");
        }
        let mut state = SessionStateSnapshot::default();
        state.active_targets = vec![camino::Utf8PathBuf::from("src/current.rs")];
        repo.reset_state_after_protocol_rollback(session.id, &state)
            .await
            .expect("seed state");

        let compact = service
            .compact_session(session.id, 1)
            .await
            .expect("manual compact");

        assert_eq!(compact.summarized_history_items, 3);
        assert_eq!(compact.retained_history_items, 1);
        let history = service
            .canonical_history_items(session.id)
            .await
            .expect("history after compact");
        assert_eq!(history.len(), 5);
        assert!(history.iter().any(|item| {
            matches!(
                &item.payload,
                HistoryItemPayload::Compaction {
                    mode: CompactionMode::Manual,
                    summary,
                    replacement_item_ids,
                    continuation: Some(continuation),
                } if item.id == compact.compaction_item_id
                    && summary.contains("CompactionContinuity")
                    && replacement_item_ids.len() == 3
                    && continuation.target_files
                        == vec![camino::Utf8PathBuf::from("src/current.rs")]
            )
        }));
        let turns = service
            .canonical_turn_items(session.id)
            .await
            .expect("turns after compact");
        assert!(turns.iter().any(|item| {
            matches!(
                &item.payload,
                TurnItemPayload::ContextCompaction { summary }
                    if item.source_item_id == Some(compact.compaction_item_id)
                        && summary.contains("Manual compaction snapshot")
            )
        }));

        repo.set_status_with_protocol_event(
            session.id,
            SessionStatus::Running,
            &RunEvent::SessionStarted {
                session_id: session.id,
                title: session.title.clone(),
            },
            turn_id,
            Some(10),
        )
        .await
        .expect("mark running");
        let active_compact = service
            .compact_session(session.id, 1)
            .await
            .expect("active live manual compact");
        assert_eq!(active_compact.session.status, SessionStatus::Running);
        let active_history = service
            .canonical_history_items(session.id)
            .await
            .expect("active compact history");
        assert!(active_history.iter().any(|item| {
            matches!(
                &item.payload,
                HistoryItemPayload::Compaction { summary, .. }
                    if item.id == active_compact.compaction_item_id
                        && summary.contains("Active live-turn manual compaction snapshot")
            )
        }));
        assert_eq!(
            repo.get_session(session.id)
                .await
                .expect("running after active compact")
                .status,
            SessionStatus::Running
        );
    }

    #[tokio::test]
    async fn active_turn_steer_is_stored_as_canonical_history_and_turn_items() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let repo = service.store.session_repo();
        let running = repo
            .create_session(NewSession {
                project_id,
                title: "Running steer mailbox".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create running session");
        let active_turn_id = TurnId::new();
        repo.set_status_with_protocol_event(
            running.id,
            SessionStatus::Running,
            &RunEvent::SessionStarted {
                session_id: running.id,
                title: running.title.clone(),
            },
            active_turn_id,
            Some(0),
        )
        .await
        .expect("mark running");
        let steer = SteerTurn {
            expected_turn_id: active_turn_id,
            items: vec![UserInputItem::Text {
                text: "Please steer the running turn before continuing.".to_string(),
            }],
            additional_context: BTreeMap::from([(
                "desktop.composer".to_string(),
                AdditionalContextEntry {
                    value: "submitted while running".to_string(),
                    kind: AdditionalContextKind::Application,
                },
            )]),
            client_user_message_id: Some("client-steer-1".to_string()),
        };

        service
            .store_active_turn_steer(running.id, &steer)
            .await
            .expect("store active steer");

        let history_items = service
            .canonical_history_items(running.id)
            .await
            .expect("history items");
        assert!(history_items.iter().any(|item| {
            matches!(
                &item.payload,
                HistoryItemPayload::SteerTurn {
                    expected_turn_id,
                    content,
                    additional_context,
                    client_user_message_id,
                } if *expected_turn_id == active_turn_id
                    && matches!(
                        content.as_slice(),
                        [crate::protocol::ContentPart::Text { text }]
                            if text.contains("running turn")
                    )
                    && additional_context.contains_key("desktop.composer")
                    && client_user_message_id.as_deref() == Some("client-steer-1")
            )
        }));
        let turn_items = service
            .canonical_turn_items(running.id)
            .await
            .expect("turn items");
        assert!(turn_items.iter().any(|item| {
            matches!(
                &item.payload,
                TurnItemPayload::SteerMessage { text } if text.contains("running turn")
            )
        }));
        let runtime_events = service
            .store
            .protocol_event_store()
            .list_runtime_events(running.id, active_turn_id)
            .expect("runtime events");
        assert!(runtime_events.iter().any(|event| {
            matches!(
                &event.msg,
                RuntimeEventMsg::SteerInputAccepted {
                    item_count: 1,
                    client_user_message_id,
                } if client_user_message_id.as_deref() == Some("client-steer-1")
            )
        }));
    }

    #[tokio::test]
    async fn canonical_item_pages_preserve_offset_limit_and_totals() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let session = service
            .store
            .session_repo()
            .create_session(NewSession {
                project_id,
                title: "Canonical pages".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create session");
        let turn_id = TurnId::new();
        for sequence_no in 1..=3 {
            let history_id = HistoryItemId::new();
            let history_item = HistoryItem {
                id: history_id,
                session_id: session.id,
                turn_id,
                sequence_no,
                created_at_ms: sequence_no,
                payload: HistoryItemPayload::UserTurn {
                    message_id: None,
                    content: vec![ContentPart::Text {
                        text: format!("request {sequence_no}"),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                    turn_context: None,
                },
            };
            let turn_item = TurnItem {
                id: TurnItemId::new(),
                session_id: session.id,
                turn_id,
                source_item_id: Some(history_id),
                sequence_no,
                payload: TurnItemPayload::UserMessage {
                    text: format!("request {sequence_no}"),
                },
            };
            service
                .store
                .protocol_event_store()
                .append_history_turn_bundle(&history_item, &turn_item)
                .expect("append canonical item bundle");
        }

        let history_page = service
            .canonical_history_page(session.id, 1, 1)
            .await
            .expect("history page");
        assert_eq!(history_page.total, 3);
        assert_eq!(history_page.offset, 1);
        assert_eq!(history_page.limit, 1);
        assert!(history_page.has_more);
        assert_eq!(history_page.items.len(), 1);
        assert_eq!(history_page.items[0].sequence_no, 2);

        let turn_page = service
            .canonical_turn_page(session.id, 2, 10)
            .await
            .expect("turn page");
        assert_eq!(turn_page.total, 3);
        assert_eq!(turn_page.items.len(), 1);
        assert_eq!(turn_page.items[0].sequence_no, 3);
        assert!(!turn_page.has_more);

        assert!(matches!(
            service.canonical_history_page(session.id, 0, 0).await,
            Err(SessionError::Message(message))
                if message.contains("limit must be greater than zero")
        ));
    }

    #[tokio::test]
    async fn canonical_runtime_event_pages_preserve_offset_limit_and_totals() {
        let (_temp, service) = test_service();
        let project_id = ProjectId::new();
        service
            .store
            .project_repo()
            .upsert_project(
                project_id,
                camino::Utf8Path::new("C:/workspace"),
                "workspace",
                "none",
            )
            .await
            .expect("create project");
        let session = service
            .store
            .session_repo()
            .create_session(NewSession {
                project_id,
                title: "Runtime event pages".to_string(),
                cwd: "C:/workspace".into(),
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("create session");
        let first_turn = TurnId::new();
        let second_turn = TurnId::new();
        let first_event = RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id: session.id,
            turn_id: first_turn,
            sequence_no: 1,
            created_at_ms: 1,
            msg: RuntimeEventMsg::ThreadConfigured {
                model: SESSION_SERVICE_FIXTURE_MODEL.to_string(),
                base_url: SESSION_SERVICE_FIXTURE_BASE_URL.to_string(),
            },
        };
        let second_event = RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id: session.id,
            turn_id: second_turn,
            sequence_no: 2,
            created_at_ms: 2,
            msg: RuntimeEventMsg::Warning {
                message: "observer only".to_string(),
            },
        };
        service
            .store
            .protocol_event_store()
            .append_runtime_event(&first_event)
            .expect("append first event");
        service
            .store
            .protocol_event_store()
            .append_runtime_event(&second_event)
            .expect("append second event");

        let page = service
            .canonical_runtime_event_page(session.id, 1, 1)
            .await
            .expect("runtime event page");
        assert_eq!(page.total, 2);
        assert_eq!(page.offset, 1);
        assert_eq!(page.limit, 1);
        assert!(!page.has_more);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, second_event.id);
        assert_eq!(page.items[0].turn_id, second_turn);

        assert!(matches!(
            service
                .canonical_runtime_event_page(session.id, 0, 0)
                .await,
            Err(SessionError::Message(message))
                if message.contains("limit must be greater than zero")
        ));
    }
}

#[cfg(test)]
fn append_state_history_item(
    service: &SessionService,
    session_id: SessionId,
    turn_id: TurnId,
    sequence_no: i64,
    state: SessionStateSnapshot,
) {
    let history_id = HistoryItemId::new();
    let history_item = HistoryItem {
        id: history_id,
        session_id,
        turn_id,
        sequence_no,
        created_at_ms: sequence_no,
        payload: HistoryItemPayload::SessionState { state },
    };
    let turn_item = TurnItem {
        id: TurnItemId::new(),
        session_id,
        turn_id,
        source_item_id: Some(history_id),
        sequence_no,
        payload: TurnItemPayload::State {
            summary: "state snapshot".to_string(),
        },
    };
    service
        .store
        .protocol_event_store()
        .append_history_turn_bundle(&history_item, &turn_item)
        .expect("append state history item");
}
