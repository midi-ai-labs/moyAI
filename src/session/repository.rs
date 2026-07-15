use async_trait::async_trait;

use crate::error::StorageError;

use super::{
    ChangeId, NewSession, ProjectId, ProjectRecord, SessionId, SessionRecord, SessionSettingsPatch,
    SessionSettingsUpdate, SessionTitleUpdate,
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
    async fn list_sessions_with_archived(
        &self,
        project_id: ProjectId,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<SessionRecord>, StorageError>;
    async fn list_recent_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>, StorageError>;
    async fn search_sessions(
        &self,
        project_id: ProjectId,
        query: &str,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<SessionRecord>, StorageError>;
    async fn set_session_archived(
        &self,
        id: SessionId,
        archived: bool,
    ) -> Result<SessionRecord, StorageError>;
    async fn update_session_settings(
        &self,
        id: SessionId,
        patch: &SessionSettingsPatch,
    ) -> Result<SessionSettingsUpdate, StorageError>;
    async fn update_session_title(
        &self,
        id: SessionId,
        title: &str,
    ) -> Result<SessionTitleUpdate, StorageError>;
    async fn delete_session(&self, id: SessionId) -> Result<(), StorageError>;
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
        changes: &[crate::edit::FileChange],
    ) -> Result<Vec<ChangeId>, StorageError>;
}
