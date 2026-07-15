use crate::error::SessionError;
use crate::session::{CanonicalSessionRead, ProjectId, SessionId, SessionRecord, SessionService};

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
) -> Result<CanonicalSessionRead, SessionError> {
    service
        .canonical_session_read(session_id, 0, usize::MAX, 0, usize::MAX)
        .await
}

#[cfg(test)]
mod tests {}
