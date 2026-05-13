use crate::error::SessionError;
use crate::protocol::{HistoryItem, ProtocolEventStore, TurnItem, UserTurn};
use crate::session::{
    EditorContext, ImagePart, MessageMetadata, MessagePart, MessageRole, NewMessage, NewPart,
    NewSession, PartKind, ProjectId, ProjectRecord, ProjectRepository, PromptDispatchPart,
    SessionContext, SessionId, SessionRecord, SessionRepository, SessionSelector,
    SessionStartRequest, SessionStateSnapshot, SessionStatus, Transcript, UserMessageMeta,
    transcript_from_history_items,
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
            repository
                .set_status(session.id, SessionStatus::Failed)
                .await?;
            repository
                .fail_unfinished_tool_calls(session.id, "Previous run was interrupted.")
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

    pub async fn store_user_turn(
        &self,
        ctx: &SessionContext,
        prompt: &str,
        requested_model: Option<String>,
    ) -> Result<crate::session::MessageRecord, SessionError> {
        self.store_user_turn_with_context(
            ctx,
            prompt,
            requested_model,
            None,
            None,
            SessionStateSnapshot::default(),
        )
        .await
    }

    pub async fn store_user_turn_with_dispatch(
        &self,
        ctx: &SessionContext,
        prompt: &str,
        requested_model: Option<String>,
        prompt_dispatch: Option<PromptDispatchPart>,
    ) -> Result<crate::session::MessageRecord, SessionError> {
        self.store_user_turn_with_context(
            ctx,
            prompt,
            requested_model,
            prompt_dispatch,
            None,
            SessionStateSnapshot::default(),
        )
        .await
    }

    pub async fn store_user_turn_with_context(
        &self,
        ctx: &SessionContext,
        prompt: &str,
        requested_model: Option<String>,
        prompt_dispatch: Option<PromptDispatchPart>,
        editor_context: Option<EditorContext>,
        initial_state: SessionStateSnapshot,
    ) -> Result<crate::session::MessageRecord, SessionError> {
        self.store_user_turn_with_context_and_images(
            ctx,
            prompt,
            requested_model,
            prompt_dispatch,
            editor_context,
            initial_state,
            Vec::new(),
        )
        .await
    }

    pub async fn store_user_turn_with_context_and_images(
        &self,
        ctx: &SessionContext,
        prompt: &str,
        requested_model: Option<String>,
        prompt_dispatch: Option<PromptDispatchPart>,
        editor_context: Option<EditorContext>,
        initial_state: SessionStateSnapshot,
        images: Vec<ImagePart>,
    ) -> Result<crate::session::MessageRecord, SessionError> {
        let repository = self.store.session_repo();
        repository.update_todos(ctx.session.id, &[]).await?;
        repository
            .update_state(ctx.session.id, &initial_state)
            .await?;
        let mut parts = vec![NewPart {
            kind: PartKind::Text,
            payload: MessagePart::Text(crate::session::TextPart {
                text: prompt.to_string(),
            }),
        }];
        for image in images {
            parts.push(NewPart {
                kind: PartKind::Image,
                payload: MessagePart::Image(image),
            });
        }
        if let Some(prompt_dispatch) = prompt_dispatch {
            parts.push(NewPart {
                kind: PartKind::PromptDispatch,
                payload: MessagePart::PromptDispatch(prompt_dispatch),
            });
        }
        let message = repository
            .append_message(
                NewMessage {
                    session_id: ctx.session.id,
                    parent_message_id: None,
                    role: MessageRole::User,
                    metadata: MessageMetadata::User(UserMessageMeta {
                        cwd: ctx.workspace.cwd.clone(),
                        requested_model,
                        editor_context,
                    }),
                },
                parts,
            )
            .await?;
        repository
            .set_status(ctx.session.id, SessionStatus::Running)
            .await?;
        Ok(message)
    }

    pub async fn store_user_thread_op(
        &self,
        ctx: &SessionContext,
        turn: &UserTurn,
        requested_model: Option<String>,
        initial_state: SessionStateSnapshot,
    ) -> Result<crate::session::MessageRecord, SessionError> {
        self.store_user_turn_with_context_and_images(
            ctx,
            &turn.text(),
            requested_model,
            turn.prompt_dispatch.clone(),
            turn.editor_context.clone(),
            initial_state,
            turn.images(),
        )
        .await
    }

    pub async fn mark_interrupted_running_sessions(
        &self,
        session_id: crate::session::SessionId,
    ) -> Result<(), SessionError> {
        self.store
            .session_repo()
            .set_status(session_id, SessionStatus::Failed)
            .await?;
        Ok(())
    }

    pub async fn load_state(
        &self,
        session_id: crate::session::SessionId,
    ) -> Result<SessionStateSnapshot, SessionError> {
        Ok(self.store.session_repo().get_state(session_id).await?)
    }

    pub async fn persist_state(
        &self,
        session_id: crate::session::SessionId,
        state: &SessionStateSnapshot,
    ) -> Result<(), SessionError> {
        self.store
            .session_repo()
            .update_state(session_id, state)
            .await?;
        Ok(())
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

    pub async fn transcript(&self, session_id: SessionId) -> Result<Transcript, SessionError> {
        Ok(self.store.session_repo().transcript(session_id).await?)
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
