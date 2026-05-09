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
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("llm http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("llm json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("llm io error: {0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Error)]
pub enum EditError {
    #[error("edit io error: {0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Error)]
pub enum PatchError {
    #[error("{0}")]
    Message(String),
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
