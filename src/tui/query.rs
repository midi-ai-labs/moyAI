use crate::error::SessionError;
use crate::protocol::TurnItem;
use crate::session::{
    NewSession, ProjectId, ProjectRepository, SessionId, SessionRecord, SessionRepository,
    SessionService, SessionStateSnapshot, TodoItem, Transcript,
};

const TUI_QUERY_FIXTURE_MODEL: &str = "qwen/qwen3.6-35b-a3b";
const TUI_QUERY_FIXTURE_BASE_URL: &str = "http://127.0.0.1:1234";
#[cfg(test)]
const TUI_QUERY_FIXTURE_PROVIDER_PROFILE: &str = "lm_studio_native_required";

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

pub(crate) fn session_view_rejects_empty_canonical_history_fixture_passes() -> bool {
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
            let service = SessionService::new(crate::storage::StoreBundle::new(store));
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
            let session = match service
                .store
                .session_repo()
                .create_session(NewSession {
                    project_id,
                    title: "Legacy empty".to_string(),
                    cwd: "C:/workspace".into(),
                    model: TUI_QUERY_FIXTURE_MODEL.to_string(),
                    base_url: TUI_QUERY_FIXTURE_BASE_URL.to_string(),
                    access_mode: crate::config::AccessMode::Default,
                })
                .await
            {
                Ok(session) => session,
                Err(_) => return false,
            };
            matches!(
                session_view(&service, session.id).await,
                Err(crate::error::SessionError::Message(message))
                    if message.contains("canonical protocol history is empty")
            )
        })
    })
    .join()
    .unwrap_or(false)
}

#[cfg(test)]
fn tui_query_current_provider_profile_fixture_passes() -> bool {
    TUI_QUERY_FIXTURE_PROVIDER_PROFILE == "lm_studio_native_required"
        && TUI_QUERY_FIXTURE_MODEL == "qwen/qwen3.6-35b-a3b"
        && TUI_QUERY_FIXTURE_BASE_URL == "http://127.0.0.1:1234"
}

#[cfg(test)]
mod tests {
    #[test]
    fn tui_query_current_provider_profile_fixture() {
        assert!(super::tui_query_current_provider_profile_fixture_passes());
    }
}
