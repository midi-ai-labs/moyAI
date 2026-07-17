use async_trait::async_trait;
use serde_json::json;

use crate::context::current_time::CurrentTimeSnapshot;
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolName, ToolResult, ToolSpec};

#[derive(Debug, Default)]
pub struct CurrentTimeTool;

#[async_trait(?Send)]
impl Tool for CurrentTimeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::CurrentTime,
            effect: crate::tool::ToolEffectPolicy::read(),
            description: "Return the current local and UTC time for date-sensitive work.",
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: serde_json::Value,
        _ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        if !raw_arguments.is_null()
            && !raw_arguments
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        {
            return Err(ToolError::Message(
                "current_time takes no arguments".to_string(),
            ));
        }
        let snapshot = CurrentTimeSnapshot::now();
        Ok(ToolResult {
            title: "Current time".to_string(),
            output_text: format!(
                "local: {}\nutc: {}\ntimezone: {}\nunix_ms: {}",
                snapshot.local, snapshot.utc, snapshot.timezone, snapshot.unix_ms
            ),
            metadata: json!(snapshot),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
            _internal_file_lease: None,
        })
    }
}
