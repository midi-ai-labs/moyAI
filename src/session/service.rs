use crate::error::SessionError;
use crate::protocol::{HistoryItem, ProtocolEventStore, TurnId, TurnItem, UserTurn};
use crate::session::{
    MessageMetadata, MessagePart, MessageRole, NewMessage, NewPart, NewSession, PartKind,
    ProjectId, ProjectRecord, ProjectRepository, RunEvent, SessionContext, SessionId,
    SessionRecord, SessionRepository, SessionSelector, SessionStartRequest, SessionStateSnapshot,
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
            self.terminalize_running_session(
                session.id,
                SessionStatus::Failed,
                RunEvent::SessionFailed {
                    session_id: session.id,
                    message: "Previous run was interrupted.".to_string(),
                },
                "Previous run was interrupted.",
            )
            .await?;
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

    pub async fn canonical_turn_items(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<TurnItem>, SessionError> {
        self.store
            .protocol_event_store()
            .list_turn_items_for_session(session_id)
            .map_err(|error| SessionError::Message(error.to_string()))
    }

    pub async fn list_todos(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<crate::session::TodoItem>, SessionError> {
        Ok(self.store.session_repo().list_todos(session_id).await?)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{HistoryItemPayload, TurnItemPayload, TurnTerminalStatus};
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
}
