use std::fs;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use serde_json::json;

use crate::error::ToolError;
use crate::runtime::SystemClock;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{ToolName, ToolResult, ToolSpec};
use crate::tool::{structured_document_guidance, structured_document_suggested_tools};
use crate::workspace::{AccessKind, instruction_file_names};

#[derive(Debug, Deserialize)]
pub struct ReadInput {
    pub path: Utf8PathBuf,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, Default)]
pub struct ReadTool;

#[async_trait(?Send)]
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::Read,
            description: "Read a UTF-8 or Shift_JIS text file with line numbers. Directories, binary files, large files, checkpoints, and structured documents are redirected to safer workflows. A write baseline is recorded only for UTF-8 files when one read shows every line from the beginning and its tool output is not truncated.",
            input_schema: json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer" },
                    "limit": { "type": "integer" }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: serde_json::Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::from_value::<ReadInput>(raw_arguments)?;
        let guarded =
            crate::tool::internal_output::resolve_path(&ctx, &input.path, AccessKind::Read).await?;
        ctx.confirm_if_needed(
            AccessKind::Read,
            format!("Read {}", guarded.absolute),
            vec![guarded.absolute.clone()],
            !guarded.inside_workspace && !guarded.trusted_external,
            Vec::new(),
        )
        .await?
        .admit()?;

        if guarded.absolute.is_dir() {
            return Ok(corrective_result(
                "Read redirected to directory inspection",
                &format!(
                    "`{}` is a directory. Use `inspect_directory` for a metadata-first tree and extension summary, or `list` when you only need a flat entry list.",
                    guarded.absolute
                ),
                json!({
                    "corrective_result": true,
                    "path": guarded.absolute,
                    "blocked_reason": "directory",
                    "suggested_tools": ["inspect_directory", "list"],
                    "edit_baseline": EditBaselineDecision::not_recorded("not_read_inline").metadata(),
                }),
            ));
        }

        let metadata = fs::metadata(&guarded.absolute)?;
        let size_bytes = metadata.len();
        let extension = normalized_extension(&guarded.absolute);
        let blocked_extensions =
            normalized_extension_list(&ctx.config.file_guard.blocked_read_extensions);
        let structured_extensions =
            normalized_extension_list(&ctx.config.file_guard.structured_document_extensions);

        if blocked_extensions.iter().any(|value| value == &extension) {
            return Ok(read_blocked_result(
                &guarded.absolute,
                size_bytes,
                "checkpoint_or_binary_artifact",
                "This file matches the configured blocked extension list for large artifacts such as model checkpoints. Do not read it inline. Use `inspect_directory` to keep working from metadata only.",
                json!({
                    "extension": extension,
                    "suggested_tools": ["inspect_directory"],
                }),
            ));
        }

        if structured_extensions
            .iter()
            .any(|value| value == &extension)
        {
            let suggested_tools = structured_document_suggested_tools(ctx.config);
            return Ok(read_blocked_result(
                &guarded.absolute,
                size_bytes,
                "structured_document",
                &format!(
                    "This file is a structured document. Do not read it inline. {}",
                    structured_document_guidance(ctx.config)
                ),
                json!({
                    "extension": extension,
                    "suggested_tools": suggested_tools,
                }),
            ));
        }

        if size_bytes > ctx.config.file_guard.max_inline_read_bytes {
            return Ok(read_blocked_result(
                &guarded.absolute,
                size_bytes,
                "large_file",
                "This file exceeds the inline read limit. Do not keep retrying `read`. Use `inspect_directory` to stay metadata-first, or use a more specialized tool path if the user explicitly needs processing.",
                json!({
                    "max_inline_read_bytes": ctx.config.file_guard.max_inline_read_bytes,
                    "suggested_tools": ["inspect_directory"],
                }),
            ));
        }

        let bytes = fs::read(&guarded.absolute)?;
        if content_inspector::inspect(&bytes).is_binary() {
            let suggested_tools = structured_document_suggested_tools(ctx.config);
            return Ok(read_blocked_result(
                &guarded.absolute,
                size_bytes,
                "binary_content",
                &format!(
                    "This file is binary. Do not read it inline. {}",
                    structured_document_guidance(ctx.config)
                ),
                json!({
                    "extension": extension,
                    "suggested_tools": suggested_tools,
                }),
            ));
        }

        let content_sha256 = crate::harness::artifact::hash_bytes(&bytes);
        let decoded = crate::tool::text_encoding::decode_text(bytes).map_err(|_| {
            ToolError::Message(
                "file is neither valid UTF-8 nor valid Shift_JIS text after guard checks"
                    .to_string(),
            )
        })?;
        let source_encoding = decoded.encoding;
        let text = decoded.text;
        let lines = text.lines().collect::<Vec<_>>();
        let offset = input.offset.unwrap_or(1).max(1);
        let limit = input.limit.unwrap_or(2_000).max(1);
        let slice = lines
            .iter()
            .enumerate()
            .skip(offset - 1)
            .take(limit)
            .map(|(index, line)| format!("{}: {}", index + 1, line))
            .collect::<Vec<_>>();
        let output = slice.join("\n");
        let preview = ctx.services.truncator.preview(
            output,
            &ctx.config.tool_output,
            &ctx.services.storage_paths,
        )?;

        let baseline = edit_baseline_decision(
            offset,
            slice.len(),
            lines.len(),
            preview.truncated,
            source_encoding == crate::tool::text_encoding::TextEncoding::Utf8,
        );

        let mtime_ms = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_millis() as i64);
        record_edit_baseline_if_eligible(
            &ctx.services.edit_safety,
            ctx.session.session.id,
            crate::edit::FileReadStamp {
                path: guarded.absolute.clone(),
                read_at_ms: SystemClock::now_ms(),
                mtime_ms,
                size_bytes: Some(size_bytes),
                content_sha256: Some(content_sha256),
            },
            baseline,
        )?;

        let instruction_sources = find_instruction_sources(&guarded.absolute, &ctx.workspace.root);
        Ok(ToolResult {
            title: format!("Read {}", guarded.absolute),
            output_text: preview.preview_text,
            metadata: json!({
                "path": guarded.absolute,
                "size_bytes": size_bytes,
                "start_line": offset,
                "end_line": (offset - 1) + slice.len(),
                "total_lines": lines.len(),
                "encoding": source_encoding.label(),
                "truncated": preview.truncated,
                "edit_baseline": baseline.metadata(),
                "instruction_sources": instruction_sources,
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }
}

fn read_blocked_result(
    path: &Utf8Path,
    size_bytes: u64,
    blocked_reason: &str,
    message: &str,
    extra_metadata: serde_json::Value,
) -> ToolResult {
    let mut metadata = json!({
        "corrective_result": true,
        "path": path,
        "size_bytes": size_bytes,
        "blocked_reason": blocked_reason,
        "edit_baseline": EditBaselineDecision::not_recorded("not_read_inline").metadata(),
    });
    if let (Some(target), Some(extra)) = (metadata.as_object_mut(), extra_metadata.as_object()) {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
    corrective_result(
        &format!("Read blocked: {blocked_reason}"),
        &format!("`{path}` was not read inline.\n{message}"),
        metadata,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EditBaselineDecision {
    recorded: bool,
    reason: &'static str,
}

impl EditBaselineDecision {
    const fn recorded() -> Self {
        Self {
            recorded: true,
            reason: "complete_visible_file",
        }
    }

    const fn not_recorded(reason: &'static str) -> Self {
        Self {
            recorded: false,
            reason,
        }
    }

    fn metadata(self) -> serde_json::Value {
        json!({
            "recorded": self.recorded,
            "reason": self.reason,
        })
    }
}

fn edit_baseline_decision(
    start_line: usize,
    visible_line_count: usize,
    total_lines: usize,
    preview_truncated: bool,
    source_is_utf8: bool,
) -> EditBaselineDecision {
    if !source_is_utf8 {
        return EditBaselineDecision::not_recorded("non_utf8_source");
    }
    if start_line != 1 || visible_line_count != total_lines {
        return EditBaselineDecision::not_recorded("partial_line_range");
    }
    if preview_truncated {
        return EditBaselineDecision::not_recorded("preview_truncated");
    }
    EditBaselineDecision::recorded()
}

fn record_edit_baseline_if_eligible(
    edit_safety: &crate::edit::EditSafety,
    session_id: crate::session::SessionId,
    stamp: crate::edit::FileReadStamp,
    decision: EditBaselineDecision,
) -> Result<(), crate::error::EditError> {
    if decision.recorded {
        edit_safety.record_read(session_id, stamp)?;
    }
    Ok(())
}

fn corrective_result(title: &str, output_text: &str, metadata: serde_json::Value) -> ToolResult {
    ToolResult {
        title: title.to_string(),
        output_text: output_text.to_string(),
        metadata,
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    }
}

fn normalized_extension(path: &Utf8Path) -> String {
    path.extension()
        .map(|value| value.trim_start_matches('.').to_ascii_lowercase())
        .unwrap_or_default()
}

fn normalized_extension_list(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn find_instruction_sources(path: &camino::Utf8Path, root: &camino::Utf8Path) -> Vec<String> {
    let mut sources = Vec::new();
    let mut current = path.parent();
    while let Some(dir) = current {
        for file_name in instruction_file_names() {
            let candidate = dir.join(file_name);
            if candidate.exists() {
                sources.push(candidate.as_str().replace('\\', "/"));
            }
        }
        if dir == root {
            break;
        }
        current = dir.parent();
    }
    sources
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use crate::edit::{EditSafety, FileReadStamp, read_file_with_identity};
    use crate::session::SessionId;

    use super::{edit_baseline_decision, record_edit_baseline_if_eligible};

    fn stamp_for(path: &camino::Utf8Path) -> (FileReadStamp, crate::edit::FileContentIdentity) {
        let (_, identity) = read_file_with_identity(path).expect("read identity");
        (
            FileReadStamp {
                path: path.to_path_buf(),
                read_at_ms: 0,
                mtime_ms: identity.mtime_ms,
                size_bytes: Some(identity.size_bytes),
                content_sha256: Some(identity.content_sha256.clone()),
            },
            identity,
        )
    }

    #[test]
    fn partial_read_does_not_grant_write_baseline() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("partial.txt")).expect("utf8 path");
        std::fs::write(&path, "one\ntwo\nthree\n").expect("seed file");
        let safety = EditSafety::default();
        let session_id = SessionId::new();
        let (stamp, identity) = stamp_for(&path);
        let decision = edit_baseline_decision(2, 1, 3, false, true);

        record_edit_baseline_if_eligible(&safety, session_id, stamp, decision)
            .expect("apply baseline decision");

        assert!(!decision.recorded);
        assert_eq!(decision.reason, "partial_line_range");
        assert!(safety.get_stamp(session_id, &path).is_none());
        assert!(
            safety
                .assert_fresh_write(session_id, &path, &identity)
                .expect_err("write after partial read must be rejected")
                .to_string()
                .contains("was not read in this session")
        );
    }

    #[test]
    fn preview_truncated_read_does_not_grant_write_baseline() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(temp.path().join("truncated.txt")).expect("utf8 path");
        std::fs::write(&path, "one\ntwo\nthree\n").expect("seed file");
        let safety = EditSafety::default();
        let session_id = SessionId::new();
        let (stamp, identity) = stamp_for(&path);
        let decision = edit_baseline_decision(1, 3, 3, true, true);

        record_edit_baseline_if_eligible(&safety, session_id, stamp, decision)
            .expect("apply baseline decision");

        assert!(!decision.recorded);
        assert_eq!(decision.reason, "preview_truncated");
        assert!(safety.get_stamp(session_id, &path).is_none());
        assert!(
            safety
                .assert_fresh_write(session_id, &path, &identity)
                .expect_err("write after preview-truncated read must be rejected")
                .to_string()
                .contains("was not read in this session")
        );
    }

    #[test]
    fn complete_visible_read_grants_write_baseline_and_reports_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("complete.txt")).expect("utf8 path");
        std::fs::write(&path, "one\ntwo\n").expect("seed file");
        let safety = EditSafety::default();
        let session_id = SessionId::new();
        let (stamp, identity) = stamp_for(&path);
        let decision = edit_baseline_decision(1, 2, 2, false, true);

        record_edit_baseline_if_eligible(&safety, session_id, stamp, decision)
            .expect("record complete baseline");

        assert!(decision.recorded);
        assert_eq!(decision.metadata()["recorded"].as_bool(), Some(true));
        assert_eq!(
            decision.metadata()["reason"].as_str(),
            Some("complete_visible_file")
        );
        safety
            .assert_fresh_write(session_id, &path, &identity)
            .expect("complete visible read permits fresh write");
    }

    #[test]
    fn shift_jis_read_never_grants_a_utf8_write_baseline() {
        let decision = edit_baseline_decision(1, 10, 10, false, false);
        assert!(!decision.recorded);
        assert_eq!(decision.reason, "non_utf8_source");
    }
}
