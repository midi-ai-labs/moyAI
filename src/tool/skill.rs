use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolName, ToolResult, ToolSpec};

#[derive(Debug, Deserialize)]
pub struct SkillInput {
    pub name: String,
}

#[derive(Debug, Default)]
pub struct SkillTool;

#[async_trait(?Send)]
impl Tool for SkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::Skill,
            description: "Load a local SKILL.md by name when the current task matches an available workspace skill.",
            input_schema: json!({
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The skill name from the available skills listed in the system prompt."
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::from_value::<SkillInput>(raw_arguments)?;
        let Some(loaded) = ctx
            .services
            .skills
            .load(&ctx.workspace.root, &input.name)
            .map_err(ToolError::Message)?
        else {
            let available = ctx
                .services
                .skills
                .snapshot_for_workspace(&ctx.workspace.root)
                .skills
                .into_iter()
                .map(|skill| skill.name)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ToolError::Message(format!(
                "skill `{}` was not found. Available skills: {}",
                input.name,
                if available.is_empty() {
                    "none"
                } else {
                    &available
                }
            )));
        };

        let sampled_files = loaded
            .sampled_files
            .iter()
            .map(|path| {
                path.strip_prefix(&loaded.manifest.base_dir)
                    .unwrap_or(path.as_path())
                    .as_str()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        let output_text = [
            format!("<skill_content name=\"{}\">", loaded.manifest.name),
            format!("# Skill: {}", loaded.manifest.name),
            String::new(),
            loaded.content.trim().to_string(),
            String::new(),
            format!(
                "Base directory for this skill: {}",
                loaded.manifest.base_dir
            ),
            "Relative paths in this skill are resolved from the base directory above.".to_string(),
            "Sampled related files:".to_string(),
            if sampled_files.is_empty() {
                "(none)".to_string()
            } else {
                sampled_files
                    .iter()
                    .map(|file| format!("- {file}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            "</skill_content>".to_string(),
        ]
        .join("\n");

        Ok(ToolResult {
            title: format!("Loaded skill {}", loaded.manifest.name),
            output_text,
            metadata: json!({
                "name": loaded.manifest.name,
                "description": loaded.manifest.description,
                "path": loaded.manifest.path,
                "base_dir": loaded.manifest.base_dir,
                "sampled_files": sampled_files,
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }
}
