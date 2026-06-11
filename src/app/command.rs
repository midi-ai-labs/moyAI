use camino::Utf8PathBuf;
use tokio_util::sync::CancellationToken;

use crate::cli::OutputMode;
use crate::config::AccessMode;
use crate::session::{EditorContext, PromptDispatchPart, SessionId};

#[derive(Debug, Clone)]
pub enum ReviewRequest {
    Uncommitted,
    Branch { base_ref: String },
}

#[derive(Clone)]
pub struct App {
    pub config: crate::config::ResolvedConfig,
    pub workspace: crate::workspace::Workspace,
    pub store: crate::storage::StoreBundle,
    pub session_service: crate::session::SessionService,
    pub run_service: crate::app::RunService,
}

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub prompt: String,
    pub session_id: Option<SessionId>,
    pub continue_last: bool,
    pub title: Option<String>,
    pub cwd: Utf8PathBuf,
    pub model: String,
    pub base_url: String,
    pub config_override: Option<crate::config::model::PartialResolvedConfig>,
    pub output_mode: OutputMode,
    pub show_reasoning: bool,
    pub prompt_dispatch: Option<PromptDispatchPart>,
    pub editor_context: Option<EditorContext>,
    pub review_request: Option<ReviewRequest>,
    pub image_paths: Vec<Utf8PathBuf>,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone)]
pub struct SessionListRequest {
    pub project_id: crate::session::ProjectId,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct SessionLoadedRequest {
    pub project_id: crate::session::ProjectId,
    pub limit: usize,
    pub include_archived: bool,
}

#[derive(Debug, Clone)]
pub struct SessionSearchRequest {
    pub project_id: crate::session::ProjectId,
    pub query: String,
    pub limit: usize,
    pub include_archived: bool,
}

#[derive(Debug, Clone)]
pub struct SessionArchiveRequest {
    pub session_id: SessionId,
    pub archived: bool,
}

#[derive(Debug, Clone)]
pub struct SessionSettingsUpdateRequest {
    pub session_id: SessionId,
    pub cwd: Option<Utf8PathBuf>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub access_mode: Option<AccessMode>,
}

#[derive(Debug, Clone)]
pub struct SessionShowRequest {
    pub session_id: SessionId,
    pub show_reasoning: bool,
}

#[derive(Debug, Clone)]
pub struct SessionHistoryRequest {
    pub session_id: SessionId,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct SessionReadRequest {
    pub session_id: SessionId,
    pub history_offset: usize,
    pub history_limit: usize,
    pub turn_offset: usize,
    pub turn_limit: usize,
}

#[derive(Debug, Clone)]
pub struct SessionRejoinRequest {
    pub session_id: SessionId,
    pub history_offset: usize,
    pub history_limit: usize,
    pub turn_offset: usize,
    pub turn_limit: usize,
}

#[derive(Debug, Clone)]
pub struct SessionRollbackRequest {
    pub session_id: SessionId,
    pub num_turns: usize,
    pub history_offset: usize,
    pub history_limit: usize,
    pub turn_offset: usize,
    pub turn_limit: usize,
}

#[derive(Debug, Clone)]
pub struct SessionForkRequest {
    pub source_session_id: SessionId,
    pub title: Option<String>,
    pub history_offset: usize,
    pub history_limit: usize,
    pub turn_offset: usize,
    pub turn_limit: usize,
}

#[derive(Debug, Clone)]
pub struct SessionTurnsRequest {
    pub session_id: SessionId,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct SessionSteerRequest {
    pub session_id: SessionId,
    pub prompt: String,
    pub cwd: Utf8PathBuf,
    pub image_paths: Vec<Utf8PathBuf>,
    pub client_user_message_id: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AppCommand {
    Run(RunRequest),
    SessionArchive(SessionArchiveRequest),
    SessionList(SessionListRequest),
    SessionLoaded(SessionLoadedRequest),
    SessionSearch(SessionSearchRequest),
    SessionShow(SessionShowRequest),
    SessionSettingsUpdate(SessionSettingsUpdateRequest),
    SessionHistory(SessionHistoryRequest),
    SessionRead(SessionReadRequest),
    SessionRejoin(SessionRejoinRequest),
    SessionRollback(SessionRollbackRequest),
    SessionFork(SessionForkRequest),
    SessionTurns(SessionTurnsRequest),
    SessionSteer(SessionSteerRequest),
}
