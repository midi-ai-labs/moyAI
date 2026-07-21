use std::io;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Config,
    Workspace,
    Permission,
    Llm,
    Tool,
    Storage,
    Process,
    Patch,
    Runtime,
    Cli,
    Agent,
    Session,
    App,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config io error: {0}")]
    Io(#[from] io::Error),
    #[error("config parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("config parse error in `{path}`: {source}")]
    ParseFile {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("config serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("{0}")]
    Message(String),
    #[error("workspace error while loading config: {0}")]
    Workspace(String),
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace io error: {0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("storage io error: {0}")]
    Io(#[from] io::Error),
    #[error("storage sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("storage json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "canonical history fence changed for session {session_id}: expected append position {expected_append_position:?}, {expected_history_count} history items, and {expected_active_count} active items; observed append position {actual_append_position:?}, {actual_history_count} history items, and {actual_active_count} active items"
    )]
    CanonicalHistoryFenceChanged {
        session_id: crate::session::SessionId,
        expected_append_position: Option<i64>,
        actual_append_position: Option<i64>,
        expected_history_count: usize,
        actual_history_count: usize,
        expected_active_count: usize,
        actual_active_count: usize,
    },
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Error)]
pub enum CliUsageError {
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Error)]
pub enum CliRenderError {
    #[error("render io error: {0}")]
    Io(#[from] io::Error),
    #[error("render json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Error)]
pub enum CliPromptError {
    #[error("prompt io error: {0}")]
    Io(#[from] io::Error),
    #[error("permission confirmation was interrupted")]
    Interrupted,
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRequestLimit {
    SerializedBodyBytes,
    MessageCount,
    ToolCount,
    ToolSchemaBytes,
    ExtraBodyBytes,
    StopSequenceCount,
    StopSequenceBytes,
    ImageCount,
    ImageDecodedBytes,
    ImageBase64Chars,
    ImageWidth,
    ImageHeight,
    ImagePixels,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStreamLimit {
    RawBytes,
    EventCount,
    ToolCallCount,
    ToolCallArgumentBytes,
    DurationMs,
}

impl std::fmt::Display for ProviderStreamLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::RawBytes => "raw bytes",
            Self::EventCount => "event count",
            Self::ToolCallCount => "tool-call count",
            Self::ToolCallArgumentBytes => "tool-call argument bytes",
            Self::DurationMs => "duration milliseconds",
        })
    }
}

impl std::fmt::Display for ProviderRequestLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::SerializedBodyBytes => "serialized body bytes",
            Self::MessageCount => "message count",
            Self::ToolCount => "tool count",
            Self::ToolSchemaBytes => "tool schema bytes",
            Self::ExtraBodyBytes => "extra body bytes",
            Self::StopSequenceCount => "stop-sequence count",
            Self::StopSequenceBytes => "stop-sequence bytes",
            Self::ImageCount => "image count",
            Self::ImageDecodedBytes => "decoded image bytes",
            Self::ImageBase64Chars => "base64 image characters",
            Self::ImageWidth => "image width",
            Self::ImageHeight => "image height",
            Self::ImagePixels => "image pixels",
        })
    }
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("llm http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("llm json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("llm io error: {0}")]
    Io(#[from] io::Error),
    #[error("provider response-start deadline expired after {timeout_ms}ms")]
    ProviderResponseStartTimeout { timeout_ms: u64 },
    #[error("provider stream was idle for {timeout_ms}ms")]
    ProviderStreamIdleTimeout { timeout_ms: u64 },
    #[error("provider request {surface} {actual} exceeds the admitted limit {maximum}")]
    ProviderRequestLimitExceeded {
        surface: ProviderRequestLimit,
        actual: u64,
        maximum: u64,
    },
    #[error("provider request image was rejected: {0}")]
    ProviderRequestImage(#[from] crate::llm::ImageValidationError),
    #[error("provider stream {surface} {actual} exceeds the admitted limit {maximum}")]
    ProviderStreamLimitExceeded {
        surface: ProviderStreamLimit,
        actual: u64,
        maximum: u64,
    },
    #[error(
        "provider rejected the request{status_text}{code_text}{param_text}: {message}",
        status_text = status.map(|value| format!(" with status {value}")).unwrap_or_default(),
        code_text = code.as_ref().map(|value| format!(" ({value})")).unwrap_or_default(),
        param_text = param.as_ref().map(|value| format!(" for {value}")).unwrap_or_default()
    )]
    ProviderRejected {
        status: Option<u16>,
        code: Option<String>,
        param: Option<String>,
        message: String,
    },
    #[error(
        "provider generation failed{response_text}{code_text} with configured max_output_tokens={max_output_tokens}: {message}",
        response_text = response_id.as_ref().map(|value| format!(" for response {value}")).unwrap_or_default(),
        code_text = code.as_ref().map(|value| format!(" ({value})")).unwrap_or_default()
    )]
    ProviderGenerationFailed {
        response_id: Option<String>,
        code: Option<String>,
        message: String,
        max_output_tokens: u32,
    },
    #[error(
        "{operation} expected a complete tool-less text response, but provider finish reason was {finish_reason:?}"
    )]
    ToollessTextFinish {
        operation: String,
        finish_reason: crate::session::FinishReason,
    },
    #[error("{operation} received a tool call in a tool-less text response")]
    ToollessTextShape { operation: String },
    #[error("provider returned an incomplete response: {reason}")]
    IncompleteResponse {
        reason: String,
        usage: Option<crate::session::TokenUsage>,
    },
    #[error("{failure}")]
    ProviderFailure {
        failure: crate::llm::ProviderFailure,
        #[source]
        source: Box<LlmError>,
    },
    #[error("{0}")]
    Message(String),
}

impl LlmError {
    pub fn token_usage(&self) -> Option<&crate::session::TokenUsage> {
        match self {
            Self::IncompleteResponse { usage, .. } => usage.as_ref(),
            Self::ProviderFailure { source, .. } => source.token_usage(),
            _ => None,
        }
    }

    pub fn provider_failure(&self) -> Option<&crate::llm::ProviderFailure> {
        match self {
            Self::ProviderFailure { failure, .. } => Some(failure),
            _ => None,
        }
    }

    /// Returns true only when the provider explicitly identifies the Responses
    /// continuation cursor as the rejected part of the request. This is kept
    /// separate from transport retries: callers may retry once with full
    /// canonical history, never by replaying a stale cursor.
    pub fn rejects_previous_response_id(&self) -> bool {
        if let Self::ProviderFailure { source, .. } = self {
            return source.rejects_previous_response_id();
        }
        if let Self::ProviderGenerationFailed { code, .. } = self {
            return code.as_deref().is_some_and(|value| {
                let value = value.to_ascii_lowercase();
                value.contains("previous_response") || value.contains("response_not_found")
            });
        }
        let Self::ProviderRejected {
            status,
            code,
            param,
            message: _,
        } = self
        else {
            return false;
        };
        if status.is_some_and(|status| !(400..500).contains(&status)) {
            return false;
        }
        let param_is_cursor = param.as_deref().is_some_and(|value| {
            value.eq_ignore_ascii_case("previous_response_id")
                || value.eq_ignore_ascii_case("previous_response")
        });
        let code_is_cursor = code.as_deref().is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("previous_response") || value.contains("response_not_found")
        });
        param_is_cursor || code_is_cursor
    }
}

#[cfg(test)]
mod llm_error_tests {
    use super::LlmError;

    #[test]
    fn previous_response_rejection_is_typed_and_narrow() {
        let rejected = LlmError::ProviderRejected {
            status: Some(400),
            code: Some("invalid_previous_response_id".to_string()),
            param: Some("previous_response_id".to_string()),
            message: "previous response was not found".to_string(),
        };
        assert!(rejected.rejects_previous_response_id());

        let ordinary = LlmError::ProviderRejected {
            status: Some(400),
            code: Some("invalid_request".to_string()),
            param: Some("tools".to_string()),
            message: "tool schema is invalid".to_string(),
        };
        assert!(!ordinary.rejects_previous_response_id());

        let message_only = LlmError::ProviderRejected {
            status: Some(400),
            code: None,
            param: None,
            message: "previous_response_id was not found".to_string(),
        };
        assert!(!message_only.rejects_previous_response_id());

        let streamed_rejection = LlmError::ProviderGenerationFailed {
            response_id: Some("resp_failed".to_string()),
            code: Some("invalid_previous_response_id".to_string()),
            message: "previous response was not found".to_string(),
            max_output_tokens: 8_192,
        };
        assert!(streamed_rejection.rejects_previous_response_id());
    }
}

#[derive(Debug, Error)]
pub enum EditError {
    #[error("edit io error: {0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Sandbox(#[from] crate::tool::sandbox_process::SandboxExecutionError),
    #[error(
        "parent directory `{parent}` for `{path}` does not exist; the file mutation was not applied and no directory was created"
    )]
    MissingParent {
        path: camino::Utf8PathBuf,
        parent: camino::Utf8PathBuf,
    },
    #[error(
        "path `{path}` changed while the edit was being prepared; external content was preserved and the commit was not applied"
    )]
    CommitConflict { path: camino::Utf8PathBuf },
    #[error(
        "path `{path}` changed while the edit was being prepared; external content was preserved at `{preserved_path}` because the target was occupied before it could be restored: {reason}"
    )]
    CommitConflictPreserved {
        path: camino::Utf8PathBuf,
        preserved_path: camino::Utf8PathBuf,
        reason: String,
    },
    #[error(
        "filesystem mutation for `{path}` may be partially committed; preserved content remains at `{preserved_path}` and requires recovery: {reason}"
    )]
    PartialCommit {
        path: camino::Utf8PathBuf,
        preserved_path: camino::Utf8PathBuf,
        reason: String,
    },
    #[error(
        "rollback for `{path}` was skipped because the committed filesystem state changed; external content was preserved and the agent change may remain partially committed"
    )]
    RollbackConflict { path: camino::Utf8PathBuf },
    #[error(
        "rollback for `{path}` could not restore the target because the committed filesystem state changed; external content was preserved at `{preserved_path}` and the agent change may remain partially committed: {reason}"
    )]
    RollbackConflictPreserved {
        path: camino::Utf8PathBuf,
        preserved_path: camino::Utf8PathBuf,
        reason: String,
    },
    #[error(
        "filesystem recovery for `{path}` left files at {preserved_paths:?}; each path requires recovery: {reason}"
    )]
    RecoveryFilesPreserved {
        path: camino::Utf8PathBuf,
        preserved_paths: Vec<camino::Utf8PathBuf>,
        reason: String,
    },
    #[error("{operation} rollback failed: {details}")]
    RollbackFailed { operation: String, details: String },
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Error)]
pub enum PatchError {
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoclingLimitSurface {
    InputBytes,
    ResponseBytes,
}

impl std::fmt::Display for DoclingLimitSurface {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InputBytes => "input bytes",
            Self::ResponseBytes => "response bytes",
        })
    }
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("tool io error: {0}")]
    Io(#[from] io::Error),
    #[error("tool json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("tool config error: {0}")]
    Config(#[from] ConfigError),
    #[error("tool workspace error: {0}")]
    Workspace(#[from] WorkspaceError),
    #[error("tool storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("tool edit error: {0}")]
    Edit(#[from] EditError),
    #[error("tool patch error: {0}")]
    Patch(#[from] PatchError),
    #[error("OS sandbox profile could not be resolved: {0}")]
    SandboxProfile(#[from] crate::tool::os_sandbox::SandboxProfileError),
    #[error("{0}")]
    SandboxExecution(#[from] crate::tool::sandbox_process::SandboxExecutionError),
    #[error("docling {surface} {actual} exceeds the limit {maximum}")]
    DoclingLimitExceeded {
        surface: DoclingLimitSurface,
        actual: u64,
        maximum: u64,
    },
    #[error("{reason}")]
    PermissionDenied {
        reason: String,
        settlement: Option<crate::runtime::ToolSettlementReservation>,
    },
    #[error("permission request aborted by user")]
    PermissionAborted,
    #[error("run interrupted while waiting for permission")]
    RunInterrupted,
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("run admission `{admission_id}` no longer owns session {session_id}")]
    RunSuperseded {
        session_id: crate::session::SessionId,
        admission_id: crate::session::AdmissionId,
    },
    #[error("agent llm error: {0}")]
    Llm(#[from] LlmError),
    #[error("agent tool error: {0}")]
    Tool(#[from] ToolError),
    #[error("agent session error: {0}")]
    Session(#[from] SessionError),
    #[error("agent storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("agent runtime error: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("agent workspace error: {0}")]
    Workspace(#[from] WorkspaceError),
    #[error("provider stopped because the output token limit was reached")]
    ProviderOutputLimit,
    #[error("provider reported an error finish reason")]
    ProviderFinishError,
    #[error(
        "provider finish reason `{finish_reason:?}` did not match the tool-call payload (has_tool_calls={has_tool_calls})"
    )]
    ProviderFinishShape {
        finish_reason: crate::session::FinishReason,
        has_tool_calls: bool,
    },
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Error)]
pub enum AppBootstrapError {
    #[error("bootstrap io error: {0}")]
    Io(#[from] io::Error),
    #[error("bootstrap config error: {0}")]
    Config(#[from] ConfigError),
    #[error("bootstrap workspace error: {0}")]
    Workspace(#[from] WorkspaceError),
    #[error("bootstrap storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("bootstrap llm error: {0}")]
    Llm(#[from] LlmError),
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Error)]
pub enum AppRunError {
    #[error("run cli render error: {0}")]
    CliRender(#[from] CliRenderError),
    #[error("run cli prompt error: {0}")]
    CliPrompt(#[from] CliPromptError),
    #[error("run agent error: {0}")]
    Agent(#[from] AgentError),
    #[error("run session error: {0}")]
    Session(#[from] SessionError),
    #[error("run storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("run runtime error: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("run llm error: {0}")]
    Llm(#[from] LlmError),
    #[error("{0}")]
    Message(String),
}
