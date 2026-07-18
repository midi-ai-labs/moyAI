use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::error::ToolError;
use crate::tool::context::{ToolContext, ToolServices};
use crate::tool::{ToolEffectClass, ToolResult, ToolSpec};

#[async_trait(?Send)]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn execute(
        &self,
        raw_arguments: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError>;
}

#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    effect_filter: Option<ToolEffectClass>,
}

impl ToolRegistry {
    pub(crate) fn empty() -> Self {
        Self {
            tools: HashMap::new(),
            effect_filter: None,
        }
    }

    pub(crate) fn retain_tools(&mut self, mut predicate: impl FnMut(&str) -> bool) {
        self.tools.retain(|name, _| predicate(name));
    }

    pub(crate) fn retain_effect(
        &mut self,
        effect: ToolEffectClass,
        mcp: Option<&crate::config::McpConfig>,
    ) {
        self.tools
            .retain(|_, tool| tool.spec().effect.can_resolve_to(effect, mcp));
        self.effect_filter = Some(effect);
    }

    pub fn builtin(_services: ToolServices) -> Self {
        let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();
        tools.insert("list".to_string(), Arc::new(crate::tool::search::ListTool));
        tools.insert("glob".to_string(), Arc::new(crate::tool::search::GlobTool));
        tools.insert("grep".to_string(), Arc::new(crate::tool::search::GrepTool));
        tools.insert("read".to_string(), Arc::new(crate::tool::read::ReadTool));
        tools.insert(
            "inspect_directory".to_string(),
            Arc::new(crate::tool::inspect_directory::InspectDirectoryTool),
        );
        tools.insert(
            "apply_patch".to_string(),
            Arc::new(crate::tool::apply_patch::ApplyPatchTool),
        );
        tools.insert("write".to_string(), Arc::new(crate::tool::write::WriteTool));
        tools.insert("shell".to_string(), Arc::new(crate::tool::shell::ShellTool));
        tools.insert(
            "current_time".to_string(),
            Arc::new(crate::tool::current_time::CurrentTimeTool),
        );
        tools.insert("skill".to_string(), Arc::new(crate::tool::skill::SkillTool));
        tools.insert(
            "docling_convert".to_string(),
            Arc::new(crate::tool::docling_convert::DoclingConvertTool),
        );
        tools.insert(
            "mcp_call".to_string(),
            Arc::new(crate::tool::mcp_call::McpCallTool),
        );
        tools.insert(
            "update_plan".to_string(),
            Arc::new(crate::tool::update_plan::UpdatePlanTool),
        );
        insert_goal_tools(&mut tools);
        Self {
            tools,
            effect_filter: None,
        }
    }

    pub fn core_agent() -> Self {
        let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();
        insert_core_agent_tools(&mut tools);
        Self {
            tools,
            effect_filter: None,
        }
    }

    pub fn core_agent_for_config(config: &crate::config::ResolvedConfig) -> Self {
        let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();
        insert_core_agent_tools(&mut tools);
        if config.multi_agent.enabled {
            insert_multi_agent_tools(&mut tools);
        }
        if config.docling.enabled {
            tools.insert(
                "docling_convert".to_string(),
                Arc::new(crate::tool::docling_convert::DoclingConvertTool),
            );
        }
        if config.mcp.enabled {
            tools.insert(
                "mcp_call".to_string(),
                Arc::new(crate::tool::mcp_call::McpCallTool),
            );
        }
        Self {
            tools,
            effect_filter: None,
        }
    }

    pub fn with_config_overlays(&self, config: &crate::config::ResolvedConfig) -> Self {
        let mut tools = self.tools.clone();
        if config.multi_agent.enabled {
            insert_multi_agent_tools(&mut tools);
        } else {
            remove_multi_agent_tools(&mut tools);
        }
        if config.docling.enabled {
            tools.insert(
                "docling_convert".to_string(),
                Arc::new(crate::tool::docling_convert::DoclingConvertTool),
            );
        } else {
            tools.remove("docling_convert");
        }
        if config.mcp.enabled {
            tools.insert(
                "mcp_call".to_string(),
                Arc::new(crate::tool::mcp_call::McpCallTool),
            );
        } else {
            tools.remove("mcp_call");
        }
        Self {
            tools,
            effect_filter: self.effect_filter,
        }
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs = self
            .tools
            .values()
            .map(|tool| tool.spec())
            .collect::<Vec<_>>();
        specs.sort_by_key(|spec| spec.name.to_string());
        specs
    }

    pub(crate) fn available_tool_names(&self) -> Vec<String> {
        let mut names = self.tools.keys().cloned().collect::<Vec<_>>();
        names.sort();
        names
    }

    #[cfg(test)]
    pub(crate) fn replace_tool_for_test(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.spec().name.to_string(), tool);
    }

    pub(crate) fn unknown_tool_message(&self, name: &str) -> String {
        let available = self.available_tool_names().join(", ");
        format!("unknown tool `{name}`; registered tools: {available}")
    }

    fn unknown_tool_error(&self, name: &str) -> ToolError {
        ToolError::Message(self.unknown_tool_message(name))
    }

    pub(crate) fn validate_call_effect(
        &self,
        name: &str,
        raw_arguments: &serde_json::Value,
        mcp: &crate::config::McpConfig,
    ) -> Result<ToolEffectClass, ToolError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| self.unknown_tool_error(name))?;
        let effect = tool.spec().effect.resolve(raw_arguments, mcp);
        if self.effect_filter.is_some_and(|allowed| allowed != effect) {
            return Err(ToolError::Message(format!(
                "tool `{name}` resolves to `{effect}` effect, which is not allowed by the active turn mode"
            )));
        }
        Ok(effect)
    }

    pub async fn execute(
        &self,
        name: &str,
        raw_arguments: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        self.validate_call_effect(name, &raw_arguments, &ctx.config.mcp)?;
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| self.unknown_tool_error(name))?;
        tool.execute(raw_arguments, ctx).await
    }
}

fn insert_core_agent_tools(tools: &mut HashMap<String, Arc<dyn Tool>>) {
    insert_goal_tools(tools);
    tools.insert("list".to_string(), Arc::new(crate::tool::search::ListTool));
    tools.insert("glob".to_string(), Arc::new(crate::tool::search::GlobTool));
    tools.insert("grep".to_string(), Arc::new(crate::tool::search::GrepTool));
    tools.insert("read".to_string(), Arc::new(crate::tool::read::ReadTool));
    tools.insert(
        "inspect_directory".to_string(),
        Arc::new(crate::tool::inspect_directory::InspectDirectoryTool),
    );
    tools.insert(
        "apply_patch".to_string(),
        Arc::new(crate::tool::apply_patch::ApplyPatchTool),
    );
    tools.insert(
        "update_plan".to_string(),
        Arc::new(crate::tool::update_plan::UpdatePlanTool),
    );
    tools.insert("write".to_string(), Arc::new(crate::tool::write::WriteTool));
    tools.insert("shell".to_string(), Arc::new(crate::tool::shell::ShellTool));
    tools.insert(
        "current_time".to_string(),
        Arc::new(crate::tool::current_time::CurrentTimeTool),
    );
}

fn insert_goal_tools(tools: &mut HashMap<String, Arc<dyn Tool>>) {
    tools.insert(
        "get_goal".to_string(),
        Arc::new(crate::tool::goal::GetGoalTool),
    );
    tools.insert(
        "create_goal".to_string(),
        Arc::new(crate::tool::goal::CreateGoalTool),
    );
    tools.insert(
        "update_goal".to_string(),
        Arc::new(crate::tool::goal::UpdateGoalTool),
    );
}

fn insert_multi_agent_tools(tools: &mut HashMap<String, Arc<dyn Tool>>) {
    tools.insert(
        "spawn_agent".to_string(),
        Arc::new(crate::tool::multi_agent::SpawnAgentTool),
    );
    tools.insert(
        "send_message".to_string(),
        Arc::new(crate::tool::multi_agent::SendMessageTool),
    );
    tools.insert(
        "followup_task".to_string(),
        Arc::new(crate::tool::multi_agent::FollowupTaskTool),
    );
    tools.insert(
        "wait_agent".to_string(),
        Arc::new(crate::tool::multi_agent::WaitAgentTool),
    );
    tools.insert(
        "interrupt_agent".to_string(),
        Arc::new(crate::tool::multi_agent::InterruptAgentTool),
    );
    tools.insert(
        "list_agents".to_string(),
        Arc::new(crate::tool::multi_agent::ListAgentsTool),
    );
}

fn remove_multi_agent_tools(tools: &mut HashMap<String, Arc<dyn Tool>>) {
    for name in [
        "spawn_agent",
        "send_message",
        "followup_task",
        "wait_agent",
        "interrupt_agent",
        "list_agents",
    ] {
        tools.remove(name);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn core_agent_registry_exposes_only_minimal_live_smoke_tools() {
        assert_eq!(
            super::ToolRegistry::core_agent().available_tool_names(),
            vec![
                "apply_patch",
                "create_goal",
                "current_time",
                "get_goal",
                "glob",
                "grep",
                "inspect_directory",
                "list",
                "read",
                "shell",
                "update_goal",
                "update_plan",
                "write"
            ]
        );
    }

    #[test]
    fn core_agent_registry_includes_search_surface() {
        let names = super::ToolRegistry::core_agent().available_tool_names();
        assert!(names.contains(&"glob".to_string()));
        assert!(names.contains(&"grep".to_string()));
    }

    #[test]
    fn core_agent_registry_includes_apply_patch_surface() {
        assert!(
            super::ToolRegistry::core_agent()
                .available_tool_names()
                .contains(&"apply_patch".to_string())
        );
    }

    #[test]
    fn core_agent_registry_exposes_only_the_canonical_update_plan_surface() {
        let names = super::ToolRegistry::core_agent().available_tool_names();
        assert!(names.contains(&"update_plan".to_string()));
    }

    #[test]
    fn core_agent_registry_includes_directory_inspection_surface() {
        assert!(
            super::ToolRegistry::core_agent()
                .available_tool_names()
                .contains(&"inspect_directory".to_string())
        );
    }

    #[test]
    fn core_agent_for_config_omits_external_tools_when_disabled() {
        let mut config = crate::config::ResolvedConfig::default();
        config.docling.enabled = false;
        config.mcp.enabled = false;

        let names = super::ToolRegistry::core_agent_for_config(&config).available_tool_names();

        assert!(!names.contains(&"docling_convert".to_string()));
        assert!(!names.contains(&"mcp_call".to_string()));
        assert!(!names.contains(&"spawn_agent".to_string()));
        assert!(!names.contains(&"send_message".to_string()));
        assert!(!names.contains(&"followup_task".to_string()));
        assert!(!names.contains(&"wait_agent".to_string()));
        assert!(!names.contains(&"interrupt_agent".to_string()));
        assert!(!names.contains(&"list_agents".to_string()));
    }

    #[test]
    fn core_agent_for_config_includes_external_tools_when_enabled() {
        let mut config = crate::config::ResolvedConfig::default();
        config.docling.enabled = true;
        config.mcp.enabled = true;

        let names = super::ToolRegistry::core_agent_for_config(&config).available_tool_names();

        assert!(names.contains(&"docling_convert".to_string()));
        assert!(names.contains(&"mcp_call".to_string()));
    }

    #[test]
    fn core_agent_for_config_includes_multi_agent_tools_only_when_enabled() {
        let mut config = crate::config::ResolvedConfig::default();
        config.multi_agent.enabled = true;

        let names = super::ToolRegistry::core_agent_for_config(&config).available_tool_names();

        for name in [
            "spawn_agent",
            "send_message",
            "followup_task",
            "wait_agent",
            "interrupt_agent",
            "list_agents",
        ] {
            assert!(names.contains(&name.to_string()), "missing {name}");
        }
        let default_names = super::ToolRegistry::core_agent().available_tool_names();
        for name in [
            "spawn_agent",
            "send_message",
            "followup_task",
            "wait_agent",
            "interrupt_agent",
            "list_agents",
        ] {
            assert!(
                !default_names.contains(&name.to_string()),
                "the no-config registry must keep {name} disabled"
            );
        }
    }
}
