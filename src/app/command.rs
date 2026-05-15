use camino::Utf8PathBuf;
use tokio_util::sync::CancellationToken;

use crate::cli::OutputMode;
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
pub struct SessionShowRequest {
    pub session_id: SessionId,
    pub show_reasoning: bool,
}

#[derive(Debug, Clone)]
pub enum AppCommand {
    Run(RunRequest),
    SessionList(SessionListRequest),
    SessionShow(SessionShowRequest),
}
