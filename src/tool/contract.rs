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
    Skill,
    DoclingConvert,
    McpCall,
    TodoWrite,
    Invalid,
}

impl ToolName {
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "list" => Some(Self::List),
            "glob" => Some(Self::Glob),
            "grep" => Some(Self::Grep),
            "read" => Some(Self::Read),
            "inspect_directory" => Some(Self::InspectDirectory),
            "apply_patch" => Some(Self::ApplyPatch),
            "write" => Some(Self::Write),
            "shell" => Some(Self::Shell),
            "skill" => Some(Self::Skill),
            "docling_convert" => Some(Self::DoclingConvert),
            "mcp_call" => Some(Self::McpCall),
            "todowrite" => Some(Self::TodoWrite),
            _ => None,
        }
    }
}

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
            ToolName::Skill => "skill",
            ToolName::DoclingConvert => "docling_convert",
            ToolName::McpCall => "mcp_call",
            ToolName::TodoWrite => "todowrite",
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
