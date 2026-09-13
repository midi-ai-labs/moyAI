use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::agent::shared::{MAX_SHARED_PROMPT_BYTES, SharedChildRequest};
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolEffectPolicy, ToolName, ToolResult, ToolSpec};

/// Installed only by the shared execution boundary, never by the ordinary registry.
pub(crate) struct SharedDelegateTool {
    pub environments: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    environment_id: String,
    title: String,
    prompt: String,
}

pub(crate) fn parse_child(
    value: serde_json::Value,
    allowed: &[String],
) -> Result<SharedChildRequest, String> {
    let input: Input = serde_json::from_value(value).map_err(|error| error.to_string())?;
    if !allowed.contains(&input.environment_id) {
        return Err("the target environment is not allowed for this shared execution".into());
    }
    if input.title.trim().is_empty()
        || input.title.len() > 256
        || input.prompt.trim().is_empty()
        || input.prompt.len() > MAX_SHARED_PROMPT_BYTES
    {
        return Err(
            "a child requires a title of at most 256 bytes and a prompt of at most 32 KiB".into(),
        );
    }
    let input_json = json!({"version": 1, "prompt": input.prompt});
    if serde_json::to_vec(&input_json)
        .map_err(|error| error.to_string())?
        .len()
        > 48 * 1024
    {
        return Err("the encoded child input exceeds the 48 KiB checkpoint input bound".into());
    }
    Ok(SharedChildRequest {
        environment_id: input.environment_id,
        title: input.title,
        input: input_json,
    })
}

#[async_trait(?Send)]
impl Tool for SharedDelegateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::SharedDelegate,
            effect: ToolEffectPolicy::mutation(),
            description: "Delegate one child job to an explicitly allowed shared environment, release this job's execution slot, and resume with the child's terminal result. Call this tool alone after stopping all managed shell processes. The prompt is shared with the selected environment; include only information this project permits sharing. The child may fail or be cancelled; inspect its result before continuing.",
            input_schema: json!({
                "type":"object", "additionalProperties":false,
                "required":["environment_id","title","prompt"],
                "properties": {
                    "environment_id":{"type":"string","enum":self.environments},
                    "title":{"type":"string","minLength":1,"maxLength":256},
                    "prompt":{"type":"string","minLength":1,"maxLength":MAX_SHARED_PROMPT_BYTES}
                }
            }),
        }
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _context: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        Err(ToolError::Message("shared_delegate requires a single tool call and a quiescent shared execution; stop managed shell processes and call it alone".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(prompt: String) -> Result<SharedChildRequest, String> {
        parse_child(
            json!({"environment_id":"solver","title":"Solve","prompt":prompt}),
            &["solver".into()],
        )
    }

    #[test]
    fn shared_delegate_bounds_utf8_prompt_by_runner_input_bytes() {
        assert!(child("x".repeat(MAX_SHARED_PROMPT_BYTES)).is_ok());
        assert!(child("x".repeat(MAX_SHARED_PROMPT_BYTES + 1)).is_err());
        let mut text = "あ".repeat(MAX_SHARED_PROMPT_BYTES / 3);
        text.push_str(&"x".repeat(MAX_SHARED_PROMPT_BYTES % 3));
        assert_eq!(text.len(), MAX_SHARED_PROMPT_BYTES);
        assert!(child(text.clone()).is_ok());
        text.push('あ');
        assert!(child(text).is_err());
    }

    #[test]
    fn shared_delegate_bounds_json_escaping_before_creating_a_checkpoint() {
        let overhead = serde_json::to_vec(&json!({"version":1,"prompt":"x"}))
            .unwrap()
            .len();
        let escapes = (48 * 1024 - overhead) / 6;
        let prompt = format!("x{}", "\0".repeat(escapes));
        assert!(prompt.len() < MAX_SHARED_PROMPT_BYTES);
        assert!(child(prompt.clone()).is_ok());
        assert!(child(format!("{prompt}\0")).is_err());
    }
}
