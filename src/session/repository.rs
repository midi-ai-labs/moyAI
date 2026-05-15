use async_trait::async_trait;

use crate::error::StorageError;

use super::{
    ChangeId, MessageId, NewMessage, NewPart, NewSession, PartRecord, ProjectId, ProjectRecord,
    SessionId, SessionRecord, SessionStateSnapshot, SessionStatus, TodoItem, Transcript,
};

#[async_trait(?Send)]
pub trait SessionRepository: Send + Sync {
    async fn create_session(&self, draft: NewSession) -> Result<SessionRecord, StorageError>;
    async fn get_session(&self, id: SessionId) -> Result<SessionRecord, StorageError>;
    async fn latest_session(
        &self,
        project_id: ProjectId,
    ) -> Result<Option<SessionRecord>, StorageError>;
    async fn list_sessions(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> Result<Vec<SessionRecord>, StorageError>;
    async fn list_recent_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>, StorageError>;
    async fn delete_session(&self, id: SessionId) -> Result<(), StorageError>;
    async fn update_session_title(&self, id: SessionId, title: &str) -> Result<(), StorageError>;
    async fn set_status(&self, id: SessionId, status: SessionStatus) -> Result<(), StorageError>;
    async fn append_message(
        &self,
        draft: NewMessage,
        parts: Vec<NewPart>,
    ) -> Result<super::MessageRecord, StorageError>;
    async fn append_part(
        &self,
        message_id: MessageId,
        part: NewPart,
    ) -> Result<PartRecord, StorageError>;
    async fn transcript(&self, session_id: SessionId) -> Result<Transcript, StorageError>;
    async fn get_state(&self, session_id: SessionId) -> Result<SessionStateSnapshot, StorageError>;
    async fn update_state(
        &self,
        session_id: SessionId,
        state: &SessionStateSnapshot,
    ) -> Result<(), StorageError>;
    async fn update_todos(
        &self,
        session_id: SessionId,
        todos: &[TodoItem],
    ) -> Result<(), StorageError>;
    async fn list_todos(&self, session_id: SessionId) -> Result<Vec<TodoItem>, StorageError>;
}

#[async_trait(?Send)]
pub trait ProjectRepository: Send + Sync {
    async fn upsert_project(
        &self,
        id: ProjectId,
        root_path: &camino::Utf8Path,
        display_name: &str,
        vcs_kind: &str,
    ) -> Result<ProjectRecord, StorageError>;
    async fn get_project(&self, id: ProjectId) -> Result<ProjectRecord, StorageError>;
    async fn list_projects(&self, limit: usize) -> Result<Vec<ProjectRecord>, StorageError>;
    async fn delete_project(&self, id: ProjectId) -> Result<(), StorageError>;
}

#[async_trait(?Send)]
pub trait ChangeRepository: Send + Sync {
    async fn insert_changes(
        &self,
        session_id: SessionId,
        changes: &[crate::edit::FileChange],
    ) -> Result<Vec<ChangeId>, StorageError>;
}
