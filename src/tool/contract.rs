use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::edit::ChangeSummary;
use crate::session::ChangeId;
use crate::workspace::AccessKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolName {
    List,
    Glob,
    Grep,
    Read,
    InspectDirectory,
    ApplyPatch,
    Write,
    Shell,
    CurrentTime,
    Skill,
    DoclingConvert,
    McpCall,
    TodoWrite,
    GetGoal,
    CreateGoal,
    UpdateGoal,
    SpawnAgent,
    SendMessage,
    FollowupTask,
    WaitAgent,
    InterruptAgent,
    ListAgents,
    Invalid,
}

impl ToolName {}

impl std::fmt::Display for ToolName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            ToolName::List => "list",
            ToolName::Glob => "glob",
            ToolName::Grep => "grep",
            ToolName::Read => "read",
            ToolName::InspectDirectory => "inspect_directory",
            ToolName::ApplyPatch => "apply_patch",
            ToolName::Write => "write",
            ToolName::Shell => "shell",
            ToolName::CurrentTime => "current_time",
            ToolName::Skill => "skill",
            ToolName::DoclingConvert => "docling_convert",
            ToolName::McpCall => "mcp_call",
            ToolName::TodoWrite => "todowrite",
            ToolName::GetGoal => "get_goal",
            ToolName::CreateGoal => "create_goal",
            ToolName::UpdateGoal => "update_goal",
            ToolName::SpawnAgent => "spawn_agent",
            ToolName::SendMessage => "send_message",
            ToolName::FollowupTask => "followup_task",
            ToolName::WaitAgent => "wait_agent",
            ToolName::InterruptAgent => "interrupt_agent",
            ToolName::ListAgents => "list_agents",
            ToolName::Invalid => "invalid",
        };
        write!(f, "{value}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: ToolName,
    pub description: &'static str,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub title: String,
    pub output_text: String,
    pub metadata: serde_json::Value,
    pub truncated_output_path: Option<Utf8PathBuf>,
    pub recorded_changes: Vec<ChangeId>,
    pub change_summaries: Vec<ChangeSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruncatedToolOutput {
    pub preview_text: String,
    pub truncated_output_path: Option<Utf8PathBuf>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub access: AccessKind,
    pub summary: String,
    #[serde(default)]
    pub details: Vec<String>,
    pub targets: Vec<Utf8PathBuf>,
    pub outside_workspace: bool,
    pub risks: Vec<PermissionRisk>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_task_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRisk {
    DestructiveDelete,
    MoveOrRename,
    Network,
    ExternalConnection,
    ProtectedWorkspaceAuthority,
}

impl PermissionRisk {
    pub fn label(self) -> &'static str {
        match self {
            Self::DestructiveDelete => "delete",
            Self::MoveOrRename => "move/rename",
            Self::Network => "network",
            Self::ExternalConnection => "external connection/setup",
            Self::ProtectedWorkspaceAuthority => "protected workspace authority",
        }
    }
}
