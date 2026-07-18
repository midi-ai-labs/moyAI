use camino::Utf8PathBuf;

use crate::cli::OutputMode;
use crate::config::AccessMode;
use crate::error::StorageError;
use crate::runtime::{SessionRuntimeEventHub, SessionRuntimeEventSubscription};
use crate::session::{EditorContext, PromptDispatchPart, SessionId, ThreadGoalStatus};

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
    pub run_service: std::sync::Arc<crate::app::RunService>,
    pub session_event_hub: SessionRuntimeEventHub,
}

impl App {
    pub fn subscribe_session_runtime_events(
        &self,
        session_id: SessionId,
    ) -> SessionRuntimeEventSubscription {
        self.session_event_hub.subscribe(session_id)
    }

    pub fn subscribe_session_runtime_events_with_backfill(
        &self,
        session_id: SessionId,
    ) -> Result<SessionRuntimeEventSubscription, StorageError> {
        let subscriber = self.session_event_hub.subscribe(session_id);
        let backfill = self
            .store
            .protocol_event_store()
            .latest_runtime_event_page_for_session(
                session_id,
                crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
            )?;
        Ok(subscriber.with_bounded_backfill_page(backfill.items, backfill.offset))
    }
}

#[derive(Clone)]
pub enum RunConfigInput {
    Layered {
        model: String,
        base_url: String,
        config_override: Option<crate::config::model::PartialResolvedConfig>,
    },
    Resolved(crate::config::ResolvedConfig),
}

impl std::fmt::Debug for RunConfigInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Layered {
                model,
                config_override,
                ..
            } => formatter
                .debug_struct("LayeredRunConfig")
                .field("model", model)
                .field("base_url", &"<redacted provider endpoint>")
                .field("config_override", config_override)
                .finish(),
            Self::Resolved(config) => formatter
                .debug_struct("ResolvedRunConfig")
                .field("model", &config.model.model)
                .field("base_url", &"<redacted provider endpoint>")
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Clone)]
pub struct RunRequest {
    pub prompt: String,
    pub session_id: Option<SessionId>,
    pub continue_last: bool,
    pub title: Option<String>,
    pub cwd: Utf8PathBuf,
    pub config: RunConfigInput,
    pub output_mode: OutputMode,
    pub show_reasoning_summary: bool,
    pub prompt_dispatch: Option<PromptDispatchPart>,
    pub editor_context: Option<EditorContext>,
    pub review_request: Option<ReviewRequest>,
    pub image_paths: Vec<Utf8PathBuf>,
    pub run_control: crate::runtime::RunControl,
    /// Cloneable permission channel inherited by child agent sessions.
    pub agent_confirmation: Option<crate::cli::SharedConfirmationPrompt>,
    /// Internal identity for a child turn. User-owned surface requests always leave this unset.
    pub agent_context: Option<crate::app::AgentRunContext>,
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

#[derive(Clone)]
pub struct SessionSettingsUpdateRequest {
    pub session_id: SessionId,
    pub cwd: Option<Utf8PathBuf>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub access_mode: Option<AccessMode>,
    pub reset_model_parameters: bool,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct SessionTitleUpdateRequest {
    pub session_id: SessionId,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct SessionInterruptRequest {
    pub session_id: SessionId,
}

#[derive(Debug, Clone)]
pub struct SessionGoalGetRequest {
    pub session_id: SessionId,
}

#[derive(Debug, Clone)]
pub struct SessionGoalSetRequest {
    pub session_id: SessionId,
    pub objective: Option<String>,
    pub status: Option<ThreadGoalStatus>,
    pub token_budget: Option<Option<i64>>,
}

#[derive(Debug, Clone)]
pub struct SessionGoalClearRequest {
    pub session_id: SessionId,
}

#[derive(Debug, Clone)]
pub struct SessionIdleAdmissionRequest {
    pub session_id: SessionId,
    pub pending_trigger_turn: bool,
}

impl std::fmt::Debug for SessionSettingsUpdateRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionSettingsUpdateRequest")
            .field("session_id", &self.session_id)
            .field("cwd", &self.cwd)
            .field("model", &self.model)
            .field(
                "base_url",
                &self
                    .base_url
                    .as_ref()
                    .map(|_| "<redacted provider endpoint>"),
            )
            .field("access_mode", &self.access_mode)
            .field("reset_model_parameters", &self.reset_model_parameters)
            .field("temperature", &self.temperature)
            .field("top_p", &self.top_p)
            .field("top_k", &self.top_k)
            .field("max_output_tokens", &self.max_output_tokens)
            .finish()
    }
}

impl std::fmt::Debug for RunRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunRequest")
            .field("prompt_chars", &self.prompt.chars().count())
            .field("session_id", &self.session_id)
            .field("continue_last", &self.continue_last)
            .field("title", &self.title)
            .field("cwd", &self.cwd)
            .field("config", &self.config)
            .field("output_mode", &self.output_mode)
            .field("show_reasoning_summary", &self.show_reasoning_summary)
            .field("prompt_dispatch_present", &self.prompt_dispatch.is_some())
            .field("editor_context_present", &self.editor_context.is_some())
            .field("review_request", &self.review_request)
            .field("image_count", &self.image_paths.len())
            .field("run_control", &self.run_control)
            .field(
                "agent_confirmation_present",
                &self.agent_confirmation.is_some(),
            )
            .field("agent_context", &self.agent_context)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct SessionShowRequest {
    pub session_id: SessionId,
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
pub struct SessionEventsRequest {
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
    SessionTitleUpdate(SessionTitleUpdateRequest),
    SessionInterrupt(SessionInterruptRequest),
    SessionGoalGet(SessionGoalGetRequest),
    SessionGoalSet(SessionGoalSetRequest),
    SessionGoalClear(SessionGoalClearRequest),
    SessionIdleAdmission(SessionIdleAdmissionRequest),
    SessionHistory(SessionHistoryRequest),
    SessionRead(SessionReadRequest),
    SessionRejoin(SessionRejoinRequest),
    SessionRollback(SessionRollbackRequest),
    SessionFork(SessionForkRequest),
    SessionTurns(SessionTurnsRequest),
    SessionEvents(SessionEventsRequest),
    SessionSteer(SessionSteerRequest),
}
