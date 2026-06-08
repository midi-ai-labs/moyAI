use async_trait::async_trait;

use crate::error::StorageError;

use super::{
    ChangeId, NewSession, ProjectId, ProjectRecord, SessionId, SessionRecord, SessionStateSnapshot,
    TodoItem,
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
    async fn get_state(&self, session_id: SessionId) -> Result<SessionStateSnapshot, StorageError>;
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
