use std::fs;

use crate::error::SessionError;
use crate::protocol::{
    HistoryItem, HistoryItemId, HistoryItemPayload, ProtocolEventStore, RuntimeEvent,
    RuntimeEventId, RuntimeEventMsg, SteerTurn, TurnId, TurnItem, TurnItemId, TurnItemPayload,
    UserTurn,
};
use crate::runtime::{Clock, SystemClock};
use crate::session::{
    CanonicalHistoryPage, CanonicalRuntimeEventPage, CanonicalSessionRead, CanonicalTurnPage,
    LoadedSessionList, LoadedSessionStatus, LoadedSessionSummary, MessageMetadata, MessagePart,
    MessageRole, NewMessage, NewPart, NewSession, PartKind, ProjectId, ProjectRecord,
    ProjectRepository, RunEvent, RunningSessionRejoin, SessionContext, SessionForkResult,
    SessionId, SessionRecord, SessionRepository, SessionRollbackResult, SessionSelector,
    SessionSettingsPatch, SessionSettingsUpdate, SessionStartRequest, SessionStateSnapshot,
    SessionStatus, Transcript, UserMessageMeta, transcript_from_history_items,
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
        if matches!(
            source.status,
            SessionStatus::Running | SessionStatus::AwaitingUser
        ) {
            return Err(SessionError::Message(format!(
                "session {} is {}; fork currently requires an idle or terminal canonical snapshot",
                source.id,
                source.status.key()
            )));
        }
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
        let (copied_history_items, copied_turn_items) = self
            .store
            .protocol_event_store()
            .fork_canonical_items(source.id, forked.id)
            .map_err(|error| SessionError::Message(error.to_string()))?;
        self.store
            .session_repo()
            .copy_session_state_and_todos(source.id, forked.id)
            .await?;
        let forked_session = self.store.session_repo().get_session(forked.id).await?;
        Ok(SessionForkResult {
            source_session: source,
            forked_session,
            copied_history_items,
            copied_turn_items,
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
    Ok(SessionSettingsPatch {
        cwd: patch.cwd,
        model,
        base_url,
        access_mode: patch.access_mode,
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
                    },
                )
                .await,
            Err(SessionError::Message(message))
                if message.contains("settings update requires an idle or terminal session")
        ));
        let stored = repo.get_session(session.id).await.expect("stored session");
        assert_eq!(stored.model, "qwen/updated");
        assert_eq!(stored.access_mode, crate::config::AccessMode::AutoReview);
        assert_eq!(stored.status, SessionStatus::Running);
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
        assert!(matches!(
            service.fork_session(source.id, None).await,
            Err(SessionError::Message(message))
                if message.contains("fork currently requires an idle or terminal canonical snapshot")
        ));
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
