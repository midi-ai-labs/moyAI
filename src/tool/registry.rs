use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::error::ToolError;
use crate::tool::context::{ToolContext, ToolServices};
use crate::tool::{ToolResult, ToolSpec};

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
}

impl ToolRegistry {
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
            "todowrite".to_string(),
            Arc::new(crate::tool::todo_write::TodoWriteTool),
        );
        Self { tools }
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

    pub(crate) fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub(crate) fn unknown_tool_message(&self, name: &str) -> String {
        let available = self.available_tool_names().join(", ");
        format!("unknown tool `{name}`. Available tools: {available}. Use `shell` to run commands.")
    }

    fn unknown_tool_error(&self, name: &str) -> ToolError {
        ToolError::Message(self.unknown_tool_message(name))
    }

    pub async fn execute(
        &self,
        name: &str,
        raw_arguments: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| self.unknown_tool_error(name))?;
        tool.execute(raw_arguments, ctx).await
    }
}
