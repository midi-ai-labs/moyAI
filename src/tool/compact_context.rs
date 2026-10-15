use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolEffectPolicy, ToolName, ToolResult, ToolSpec};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactContextInput {
    summary: String,
}

#[derive(Debug, Default)]
pub struct CompactContextTool;

#[async_trait(?Send)]
impl Tool for CompactContextTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::CompactContext,
            effect: ToolEffectPolicy::read(),
            description: "Propose concise continuation notes for the current conversation after substantial stale output has accumulated. Preserve the task and constraints, verified facts and evidence paths, unresolved matters, and next steps. The host adopts notes that reduce input after this tool batch completes, keeping exact user instructions and the most recent response and tool results. Original conversation history is retained. Results of other tools in the same batch are not covered by these notes.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["summary"],
                "properties": {
                    "summary": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Concise continuation notes covering the task and constraints, verified facts and evidence paths, unresolved matters, and next steps."
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        Ok(proposal_result(parse_summary(raw_arguments)?))
    }
}

pub(crate) fn parse_summary(raw_arguments: Value) -> Result<String, ToolError> {
    if !raw_arguments.is_object() {
        return Err(ToolError::Message(
            "compact_context requires an object containing summary".into(),
        ));
    }
    let input = serde_json::from_value::<CompactContextInput>(raw_arguments)?;
    let summary = input.summary.trim().to_string();
    if summary.is_empty() {
        return Err(ToolError::Message(
            "compact_context requires a non-empty summary".to_string(),
        ));
    }
    Ok(summary)
}

fn proposal_result(summary: String) -> ToolResult {
    ToolResult {
        title: "Context compaction proposed".to_string(),
        output_text: "Context compaction proposed for adoption after this tool batch completes. Original conversation history is retained.".to_string(),
        metadata: json!({ "summary": summary }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
        _internal_file_lease: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_context_advertises_one_required_summary_and_read_effect() {
        let spec = CompactContextTool.spec();
        assert_eq!(spec.name, ToolName::CompactContext);
        assert_eq!(spec.effect, ToolEffectPolicy::read());
        assert_eq!(spec.input_schema["type"], "object");
        assert_eq!(spec.input_schema["additionalProperties"], false);
        assert_eq!(spec.input_schema["required"], json!(["summary"]));
        let properties = spec.input_schema["properties"].as_object().unwrap();
        assert_eq!(properties.len(), 1);
        assert_eq!(properties["summary"]["type"], "string");
        assert_eq!(properties["summary"]["minLength"], 1);
    }

    #[test]
    fn compact_context_preserves_continuation_notes_in_the_proposal() {
        let summary = parse_summary(json!({
            "summary": " \nTask: repair compaction.\nEvidence: project_sandbox/check/RESULTS.md\nNext: verify resume.\n "
        }))
        .expect("valid notes");
        let result = proposal_result(summary.clone());
        assert_eq!(
            summary,
            "Task: repair compaction.\nEvidence: project_sandbox/check/RESULTS.md\nNext: verify resume."
        );
        assert_eq!(result.metadata, json!({ "summary": summary }));
        assert!(result.recorded_changes.is_empty());
        assert!(result.change_summaries.is_empty());
        assert!(result.truncated_output_path.is_none());
    }

    #[test]
    fn compact_context_rejects_empty_missing_unknown_and_non_string_input() {
        for arguments in [
            json!({ "summary": "" }),
            json!({ "summary": " \n\t\u{3000} " }),
            json!({}),
            json!({ "summary": "notes", "extra": true }),
            json!({ "summary": null }),
            json!({ "summary": 42 }),
            json!(["notes"]),
            json!("notes"),
        ] {
            assert!(parse_summary(arguments.clone()).is_err(), "{arguments}");
        }
    }
}
