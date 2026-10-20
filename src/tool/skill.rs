use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::config::ToolOutputConfig;
use crate::error::ToolError;
use crate::skill::{BUILTIN_SECURITY_REVIEW_NAME, BuiltinSkill, builtin_skill};
use crate::storage::StoragePaths;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::truncate::ToolTruncator;
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
            effect: crate::tool::ToolEffectPolicy::read(),
            description: "Load an available built-in or workspace skill by name when the current task matches it.",
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
        if let Some(skill) = builtin_skill(&input.name) {
            return load_builtin_skill(
                skill,
                &ctx.services.truncator,
                &ctx.config.tool_output,
                &ctx.services.storage_paths,
            );
        }
        let Some(loaded) = ctx
            .services
            .skills
            .load(&ctx.workspace.root, &input.name)
            .map_err(ToolError::Message)?
        else {
            let local_skills = ctx
                .services
                .skills
                .snapshot_for_workspace(&ctx.workspace.root);
            let available = std::iter::once(BUILTIN_SECURITY_REVIEW_NAME.to_string())
                .chain(
                    local_skills
                        .skills
                        .into_iter()
                        .filter(|skill| builtin_skill(&skill.name).is_none())
                        .map(|skill| skill.name),
                )
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ToolError::Message(format!(
                "skill `{}` was not found. Available skills: {}",
                input.name, available
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
        let chunks = [
            format!(
                "<skill_content name=\"{}\" base_dir=\"{}\">",
                loaded.manifest.name, loaded.manifest.base_dir
            ),
            loaded.content.trim().to_string(),
            format!(
                "<sampled_files>{}</sampled_files>",
                if sampled_files.is_empty() {
                    "none".to_string()
                } else {
                    sampled_files.join(", ")
                }
            ),
            "</skill_content>".to_string(),
        ];
        let preview = ctx.services.truncator.preview_chunks(
            chunks,
            "\n",
            &ctx.config.tool_output,
            &ctx.services.storage_paths,
        )?;

        Ok(ToolResult {
            title: format!("Loaded skill {}", loaded.manifest.name),
            output_text: preview.preview_text,
            metadata: json!({
                "name": loaded.manifest.name,
                "description": loaded.manifest.description,
                "path": loaded.manifest.path,
                "base_dir": loaded.manifest.base_dir,
                "sampled_files": sampled_files,
                "truncated": preview.truncated,
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
            _internal_file_lease: preview.internal_file_lease,
        })
    }
}

fn load_builtin_skill(
    skill: BuiltinSkill,
    truncator: &ToolTruncator,
    limits: &ToolOutputConfig,
    paths: &StoragePaths,
) -> Result<ToolResult, ToolError> {
    let chunks = [
        format!("<skill_content name=\"{}\" source=\"builtin\">", skill.name),
        skill.content.trim().to_string(),
        "</skill_content>".to_string(),
    ];
    let preview = truncator.preview_chunks(chunks, "\n", limits, paths)?;
    Ok(ToolResult {
        title: format!("Loaded skill {}", skill.name),
        output_text: preview.preview_text,
        metadata: json!({
            "source": "builtin",
            "name": skill.name,
            "description": skill.description,
            "truncated": preview.truncated,
        }),
        truncated_output_path: preview.truncated_output_path,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
        _internal_file_lease: preview.internal_file_lease,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage_paths(root: &std::path::Path) -> StoragePaths {
        let root = camino::Utf8PathBuf::from_path_buf(root.to_path_buf()).expect("UTF-8 root");
        StoragePaths {
            database_path: root.join("test.db"),
            truncation_dir: root.join("truncated"),
            data_dir: root,
        }
    }

    #[test]
    fn builtin_load_reports_its_source_without_inventing_filesystem_authority() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = storage_paths(temp.path());
        let skill = builtin_skill(BUILTIN_SECURITY_REVIEW_NAME).expect("built-in skill");
        let result = load_builtin_skill(
            skill,
            &ToolTruncator,
            &ToolOutputConfig {
                max_lines: 100,
                max_bytes: 16 * 1024,
                max_results: 10,
            },
            &paths,
        )
        .expect("load embedded skill without workspace files");

        assert_eq!(result.metadata["source"], "builtin");
        assert_eq!(result.metadata["name"], BUILTIN_SECURITY_REVIEW_NAME);
        assert_eq!(result.metadata["truncated"], false);
        assert!(result.metadata.get("path").is_none());
        assert!(result.metadata.get("base_dir").is_none());
        assert!(result.output_text.contains(skill.content.trim()));
        assert!(!result.output_text.contains("base_dir="));
        assert!(result.recorded_changes.is_empty());
        assert!(result.truncated_output_path.is_none());
        assert!(!paths.data_dir.join("test.db").exists());
    }

    #[test]
    fn builtin_load_preserves_the_full_body_when_normal_output_limits_truncate_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = storage_paths(temp.path());
        let skill = builtin_skill(BUILTIN_SECURITY_REVIEW_NAME).expect("built-in skill");
        let result = load_builtin_skill(
            skill,
            &ToolTruncator,
            &ToolOutputConfig {
                max_lines: 2,
                max_bytes: 40,
                max_results: 10,
            },
            &paths,
        )
        .expect("bounded embedded skill");

        assert_eq!(result.metadata["source"], "builtin");
        assert_eq!(result.metadata["truncated"], true);
        assert!(result._internal_file_lease.is_some());
        let stored = std::fs::read_to_string(
            result
                .truncated_output_path
                .as_ref()
                .expect("spooled output"),
        )
        .expect("read complete skill output");
        assert!(stored.contains(skill.content.trim()));
        assert!(stored.ends_with("</skill_content>"));
    }
}
