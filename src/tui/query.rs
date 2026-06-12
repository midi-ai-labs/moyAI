use crate::error::SessionError;
use crate::protocol::TurnItem;
use crate::session::{
    ProjectId, SessionId, SessionRecord, SessionService, SessionStateSnapshot, TodoItem, Transcript,
};

pub struct SessionView {
    pub session: SessionRecord,
    pub transcript: Transcript,
    pub turn_items: Vec<TurnItem>,
    pub state: SessionStateSnapshot,
    pub todos: Vec<TodoItem>,
}

pub async fn recent_sessions(
    service: &SessionService,
    project_id: ProjectId,
    limit: usize,
) -> Result<Vec<SessionRecord>, SessionError> {
    service.list_sessions(project_id, limit).await
}

pub async fn search_sessions(
    service: &SessionService,
    project_id: ProjectId,
    query: &str,
    include_archived: bool,
    limit: usize,
) -> Result<Vec<SessionRecord>, SessionError> {
    let query = query.trim();
    if query.is_empty() {
        return service
            .list_sessions_with_archived(project_id, limit, include_archived)
            .await;
    }
    service
        .search_sessions(project_id, query, limit, include_archived)
        .await
}

pub async fn global_recent_sessions(
    service: &SessionService,
    limit: usize,
) -> Result<Vec<SessionRecord>, SessionError> {
    service.list_recent_sessions(limit).await
}

pub async fn latest_session(
    service: &SessionService,
    project_id: ProjectId,
) -> Result<Option<SessionRecord>, SessionError> {
    service.latest_session(project_id).await
}

pub async fn session_view(
    service: &SessionService,
    session_id: SessionId,
) -> Result<SessionView, SessionError> {
    let session = service.get_session(session_id).await?;
    let turn_items = service.canonical_turn_items(session_id).await?;
    let transcript = service.canonical_transcript(session_id).await?;
    Ok(SessionView {
        session,
        transcript,
        turn_items,
        state: service.load_state(session_id).await?,
        todos: service.list_todos(session_id).await?,
    })
}

#[cfg(test)]
mod tests {}
