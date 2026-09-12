use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::edit::ChangeSummary;
use crate::session::ChangeId;
use crate::workspace::AccessKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolEffectClass {
    /// Reads state or updates agent-internal, model-visible bookkeeping without
    /// mutating the user's workspace or an external system.
    Read,
    Mutation,
    Destructive,
}

impl ToolEffectClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Mutation => "mutation",
            Self::Destructive => "destructive",
        }
    }

    pub fn access_kind(self) -> AccessKind {
        match self {
            Self::Read => AccessKind::Read,
            Self::Mutation | Self::Destructive => AccessKind::Edit,
        }
    }

    pub fn permission_risks(self) -> Vec<PermissionRisk> {
        match self {
            Self::Read => vec![PermissionRisk::Network],
            Self::Mutation => vec![PermissionRisk::Network, PermissionRisk::ExternalMutation],
            Self::Destructive => vec![
                PermissionRisk::Network,
                PermissionRisk::ExternalDestructiveOperation,
            ],
        }
    }
}

impl std::fmt::Display for ToolEffectClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolEffectPolicy {
    Static(ToolEffectClass),
    McpCall,
}

impl ToolEffectPolicy {
    pub const fn read() -> Self {
        Self::Static(ToolEffectClass::Read)
    }

    pub const fn mutation() -> Self {
        Self::Static(ToolEffectClass::Mutation)
    }

    pub const fn destructive() -> Self {
        Self::Static(ToolEffectClass::Destructive)
    }

    pub fn can_resolve_to(
        self,
        effect: ToolEffectClass,
        mcp: Option<&crate::config::McpConfig>,
    ) -> bool {
        match self {
            Self::Static(actual) => actual == effect,
            Self::McpCall => mcp.is_some_and(|config| crate::mcp::can_route_effect(config, effect)),
        }
    }

    pub fn resolve(
        self,
        raw_arguments: &serde_json::Value,
        mcp: &crate::config::McpConfig,
    ) -> ToolEffectClass {
        match self {
            Self::Static(effect) => effect,
            Self::McpCall => crate::mcp::effect_for_raw_call(mcp, raw_arguments),
        }
    }
}

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
    ShellStart,
    ShellStatus,
    ShellStop,
    CurrentTime,
    Skill,
    DoclingConvert,
    McpCall,
    WaitRemoteTasks,
    UpdatePlan,
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
            ToolName::ShellStart => "shell_start",
            ToolName::ShellStatus => "shell_status",
            ToolName::ShellStop => "shell_stop",
            ToolName::CurrentTime => "current_time",
            ToolName::Skill => "skill",
            ToolName::DoclingConvert => "docling_convert",
            ToolName::McpCall => "mcp_call",
            ToolName::WaitRemoteTasks => "wait_remote_tasks",
            ToolName::UpdatePlan => "update_plan",
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

impl ToolName {
    pub fn parse(value: &str) -> Self {
        match value {
            "list" => Self::List,
            "glob" => Self::Glob,
            "grep" => Self::Grep,
            "read" => Self::Read,
            "inspect_directory" => Self::InspectDirectory,
            "apply_patch" => Self::ApplyPatch,
            "write" => Self::Write,
            "shell" => Self::Shell,
            "shell_start" => Self::ShellStart,
            "shell_status" => Self::ShellStatus,
            "shell_stop" => Self::ShellStop,
            "current_time" => Self::CurrentTime,
            "skill" => Self::Skill,
            "docling_convert" => Self::DoclingConvert,
            "mcp_call" => Self::McpCall,
            "wait_remote_tasks" => Self::WaitRemoteTasks,
            "update_plan" => Self::UpdatePlan,
            "get_goal" => Self::GetGoal,
            "create_goal" => Self::CreateGoal,
            "update_goal" => Self::UpdateGoal,
            "spawn_agent" => Self::SpawnAgent,
            "send_message" => Self::SendMessage,
            "followup_task" => Self::FollowupTask,
            "wait_agent" => Self::WaitAgent,
            "interrupt_agent" => Self::InterruptAgent,
            "list_agents" => Self::ListAgents,
            _ => Self::Invalid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PermissionRisk, ToolEffectClass, ToolName};
    use crate::workspace::AccessKind;

    #[test]
    fn wait_remote_tasks_has_one_canonical_tool_name() {
        assert_eq!(ToolName::WaitRemoteTasks.to_string(), "wait_remote_tasks");
        assert_eq!(
            ToolName::parse("wait_remote_tasks"),
            ToolName::WaitRemoteTasks
        );
        assert_eq!(
            serde_json::to_string(&ToolName::WaitRemoteTasks).unwrap(),
            "\"wait_remote_tasks\""
        );
        assert_eq!(
            serde_json::from_str::<ToolName>("\"wait_remote_tasks\"").unwrap(),
            ToolName::WaitRemoteTasks
        );
    }

    #[test]
    fn update_plan_serializes_canonically() {
        assert_eq!(
            serde_json::to_string(&ToolName::UpdatePlan).expect("serialize"),
            "\"update_plan\""
        );
    }

    #[test]
    fn external_permission_shape_is_derived_from_effect_class() {
        assert_eq!(ToolEffectClass::Read.access_kind(), AccessKind::Read);
        assert_eq!(ToolEffectClass::Mutation.access_kind(), AccessKind::Edit);
        assert_eq!(ToolEffectClass::Destructive.access_kind(), AccessKind::Edit);
        assert!(
            ToolEffectClass::Mutation
                .permission_risks()
                .contains(&PermissionRisk::ExternalMutation)
        );
        assert!(
            ToolEffectClass::Destructive
                .permission_risks()
                .contains(&PermissionRisk::ExternalDestructiveOperation)
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: ToolName,
    pub description: &'static str,
    pub input_schema: serde_json::Value,
    pub effect: ToolEffectPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub title: String,
    pub output_text: String,
    pub metadata: serde_json::Value,
    pub truncated_output_path: Option<Utf8PathBuf>,
    pub recorded_changes: Vec<ChangeId>,
    pub change_summaries: Vec<ChangeSummary>,
    /// Keeps a newly-created internal file fenced against orphan cleanup until
    /// the caller has committed `truncated_output_path` to durable storage.
    #[serde(skip)]
    pub(crate) _internal_file_lease: Option<crate::storage::InternalFileProducerLease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruncatedToolOutput {
    pub preview_text: String,
    pub truncated_output_path: Option<Utf8PathBuf>,
    pub truncated: bool,
    #[serde(skip)]
    pub(crate) internal_file_lease: Option<crate::storage::InternalFileProducerLease>,
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
    ConfiguredLocalService,
    ProtectedWorkspaceAuthority,
    ExternalMutation,
    ExternalDestructiveOperation,
    UnclassifiedShell,
}

impl PermissionRisk {
    pub fn label(self) -> &'static str {
        match self {
            Self::DestructiveDelete => "delete",
            Self::MoveOrRename => "move/rename",
            Self::Network => "network",
            Self::ExternalConnection => "external connection/setup",
            Self::ConfiguredLocalService => "configured local service",
            Self::ProtectedWorkspaceAuthority => "protected workspace authority",
            Self::ExternalMutation => "external mutation",
            Self::ExternalDestructiveOperation => "destructive external operation",
            Self::UnclassifiedShell => "unclassified dynamic/indirect shell construct",
        }
    }
}
