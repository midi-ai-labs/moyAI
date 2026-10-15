use std::io::{Read, Write};

use async_trait::async_trait;
use camino::Utf8Path;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::ToolError;
use crate::storage::{InternalFileProducerLease, StoragePaths};
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolEffectPolicy, ToolName, ToolResult, ToolSpec};
use crate::workspace::{AccessKind, GuardedPath, PathGuard, Workspace};

const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;

/// A shared-run capability; this tool is never installed in a local tool registry.
pub(crate) struct SharedPublishArtifactTool {
    pub job_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: String,
    name: Option<String>,
}

fn invalid(message: &str) -> ToolError {
    ToolError::Message(message.into())
}

fn portable_relative(path: &str) -> Result<String, ToolError> {
    let normalized = path.replace('\\', "/");
    if normalized.is_empty()
        || normalized.len() > 240
        || normalized
            .chars()
            .any(|c| c.is_control() || ":*?\"<>|".contains(c))
        || normalized.split('/').any(|part| {
            let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
            part.is_empty()
                || part.len() > 100
                || matches!(part, "." | "..")
                || part.ends_with([' ', '.'])
                || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || ["COM", "LPT"].iter().any(|prefix| {
                    stem.strip_prefix(prefix).is_some_and(|suffix| {
                        matches!(
                            suffix,
                            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                        )
                    })
                })
        })
    {
        return Err(invalid(
            "artifact paths and names must be safe relative file paths (at most 240 bytes)",
        ));
    }
    Ok(normalized)
}

fn resolve(workspace: &Workspace, relative: &str) -> Result<GuardedPath, ToolError> {
    let guarded = PathGuard::require_path(
        workspace,
        &workspace.authority_root().join(Utf8Path::new(relative)),
        AccessKind::Read,
    )?;
    if !guarded.inside_workspace {
        return Err(invalid(
            "shared artifacts must be inside the assigned environment",
        ));
    }
    Ok(guarded)
}

fn read_snapshot(guarded: &GuardedPath) -> Result<Vec<u8>, ToolError> {
    // Keep the validated handle throughout the read: no path reopen after validation.
    let file = PathGuard::open_validated_metadata_handle(guarded)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid("a shared artifact must be a regular file"));
    }
    if metadata.len() > MAX_ARTIFACT_BYTES as u64 {
        return Err(invalid("a shared artifact cannot exceed 8 MiB"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_ARTIFACT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(invalid("a shared artifact cannot exceed 8 MiB"));
    }
    Ok(bytes)
}

fn persist_snapshot(
    paths: &StoragePaths,
    name: &str,
    source_path: &str,
    bytes: &[u8],
) -> Result<ToolResult, ToolError> {
    let lease = InternalFileProducerLease::acquire(paths)?;
    std::fs::create_dir_all(&paths.truncation_dir)?;
    let path = paths
        .truncation_dir
        .join(format!("{}.bin", ulid::Ulid::new()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    let sha256 = format!("{:x}", Sha256::digest(bytes));
    Ok(ToolResult {
        title: format!("Shared artifact: {name}"),
        output_text: format!(
            "Saved shared artifact {name} ({} bytes, SHA-256 {sha256}). This snapshot is attached when the shared execution reports its result or child handoff.",
            bytes.len()
        ),
        metadata: json!({"artifact":{"name":name,"source_path":source_path,"sha256":sha256,"byte_length":bytes.len()}}),
        truncated_output_path: Some(path),
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
        _internal_file_lease: Some(lease),
    })
}

#[async_trait(?Send)]
impl Tool for SharedPublishArtifactTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::SharedPublishArtifact,
            effect: ToolEffectPolicy::read(),
            description: include_str!("../../assets/prompts/shared_publish_artifact.md"),
            input_schema: json!({"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string","minLength":1,"maxLength":240},"name":{"type":"string","minLength":1,"maxLength":240}}}),
        }
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input: Input = serde_json::from_value(arguments)?;
        let path = portable_relative(&input.path)?;
        let name = portable_relative(input.name.as_deref().unwrap_or(&path))?;
        ctx.run_mutation_fence.assert_owned().await?;
        ctx.services
            .store
            .session_repo()
            .require_active_shared_tool(
                &self.job_id,
                ctx.session.session.id,
                ctx.run_mutation_fence.turn_id(),
            )?;
        let guarded = resolve(ctx.workspace, &path)?;
        let admission = ctx
            .confirm_if_needed(
                AccessKind::Read,
                format!("Save shared artifact {name}"),
                vec![guarded.absolute.clone()],
                false,
                Vec::new(),
            )
            .await?;
        admission.admit()?;
        let bytes = read_snapshot(&guarded)?;
        ctx.run_mutation_fence.assert_owned().await?;
        // Agent commits each tool result before dispatching the next call, including
        // multi-call model responses. Count canonical references, never a second counter.
        ctx.services
            .store
            .session_repo()
            .require_shared_artifact_capacity(
                &self.job_id,
                ctx.session.session.id,
                ctx.run_mutation_fence.turn_id(),
                &ctx.services.storage_paths,
                bytes.len(),
            )?;
        admission.admit()?;
        persist_snapshot(&ctx.services.storage_paths, &name, &path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    #[test]
    fn shared_artifact_rejects_escape_and_nonportable_names() {
        for path in [
            "",
            "/tmp/a",
            "../a",
            "a/../b",
            "C:\\a",
            "\\\\server\\file",
            "a:stream",
            "CON.txt",
            "LPT¹",
            "a/",
            "a//b",
            "a. ",
            "a\n",
        ] {
            assert!(portable_relative(path).is_err(), "{path:?}");
        }
        assert_eq!(
            portable_relative("output\\解答.bin").unwrap(),
            "output/解答.bin"
        );
    }

    #[test]
    fn shared_artifact_binary_snapshot_is_immutable_and_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let workspace = crate::workspace::WorkspaceDiscovery::discover_fixed_root(
            &root,
            &crate::config::ResolvedConfig::default(),
        )
        .unwrap();
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).unwrap();
        let paths = StoragePaths {
            database_path: data_dir.join("unused.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let source = root.join("solution.bin");
        let bytes = [0, 255, 128, 1, 13, 10];
        std::fs::write(&source, bytes).unwrap();
        let result = persist_snapshot(
            &paths,
            "solution.bin",
            "solution.bin",
            &read_snapshot(&resolve(&workspace, "solution.bin").unwrap()).unwrap(),
        )
        .unwrap();
        std::fs::write(&source, "changed").unwrap();
        assert_eq!(
            std::fs::read(result.truncated_output_path.as_ref().unwrap()).unwrap(),
            bytes
        );
        assert_eq!(
            result.metadata["artifact"]["sha256"],
            format!("{:x}", Sha256::digest(bytes))
        );
        assert!(result._internal_file_lease.is_some());
        assert!(read_snapshot(&resolve(&workspace, ".").unwrap()).is_err());
        let large = std::fs::File::create(&source).unwrap();
        large.set_len(MAX_ARTIFACT_BYTES as u64).unwrap();
        assert_eq!(
            read_snapshot(&resolve(&workspace, "solution.bin").unwrap())
                .unwrap()
                .len(),
            MAX_ARTIFACT_BYTES
        );
        large.set_len(MAX_ARTIFACT_BYTES as u64 + 1).unwrap();
        assert!(read_snapshot(&resolve(&workspace, "solution.bin").unwrap()).is_err());
    }
}
