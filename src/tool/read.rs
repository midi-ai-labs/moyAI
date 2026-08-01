use std::io::Read;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use serde_json::json;

use crate::error::ToolError;
use crate::runtime::SystemClock;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::truncate::clip_text_with_ellipsis;
use crate::tool::{ToolName, ToolResult, ToolSpec};
use crate::workspace::traversal::{
    ExactFileCandidateScan, find_exact_workspace_file_candidates,
    find_similar_sibling_file_candidates,
};
use crate::workspace::{AccessKind, PathGuard, Workspace, instruction_file_names};

const MAX_MISSING_READ_DIAGNOSTIC_BYTES: usize = 1_024;

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
            effect: crate::tool::ToolEffectPolicy::read(),
            description: "Read a bounded UTF-8 or Shift_JIS text range with line numbers. A full-content write baseline is recorded only when a UTF-8 read exposes the complete file without output truncation; targeted apply_patch updates match context against the current file and do not require this baseline.",
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
        let resolved =
            crate::tool::internal_output::resolve_path(&ctx, &input.path, AccessKind::Read).await?;
        let permission = resolved.permission();
        ctx.confirm_if_needed(
            AccessKind::Read,
            format!("Read {}", permission.absolute),
            vec![permission.absolute.to_path_buf()],
            !permission.inside_workspace && !permission.trusted_external,
            Vec::new(),
        )
        .await?
        .admit()?;

        let missing_read_request = if permission.inside_workspace {
            input.path.file_name().map(|basename| MissingReadRequest {
                requested: input.path.clone(),
                absolute: permission.absolute.to_path_buf(),
                basename: basename.to_string(),
                suffix: exact_candidate_suffix(&input.path, permission.absolute, ctx.workspace),
                max_diagnostic_bytes: ctx
                    .config
                    .tool_output
                    .max_bytes
                    .min(MAX_MISSING_READ_DIAGNOSTIC_BYTES)
                    .max(1),
            })
        } else {
            None
        };
        let mut opened = match resolved.into_read_file() {
            Ok(opened) => opened,
            Err(error) if is_not_found_error(&error) => {
                let Some(request) = missing_read_request else {
                    return Err(error);
                };
                let (scan, candidate_kind) =
                    match find_similar_sibling_file_candidates(ctx.workspace, &request.absolute) {
                        Ok(scan) if !scan.paths.is_empty() => {
                            (scan, MissingCandidateKind::SimilarSibling)
                        }
                        Ok(_) => {
                            let Ok(scan) = find_exact_workspace_file_candidates(
                                ctx.workspace,
                                &request.basename,
                                request.suffix.as_deref(),
                            ) else {
                                return Err(error);
                            };
                            (scan, MissingCandidateKind::Exact)
                        }
                        Err(_) => return Err(error),
                    };
                return Err(ToolError::Message(render_missing_read_diagnostic(
                    &request.requested,
                    &scan,
                    candidate_kind,
                    request.max_diagnostic_bytes,
                )));
            }
            Err(error) => return Err(error),
        };
        let metadata = require_readable_file_metadata(opened.absolute(), opened.metadata()?)?;
        let size_bytes = metadata.len();
        let extension = normalized_extension(opened.absolute());
        let blocked_extensions =
            normalized_extension_list(&ctx.config.file_guard.blocked_read_extensions);
        let structured_extensions =
            normalized_extension_list(&ctx.config.file_guard.structured_document_extensions);

        if blocked_extensions.iter().any(|value| value == &extension) {
            return Ok(read_blocked_result(
                opened.absolute(),
                size_bytes,
                "checkpoint_or_binary_artifact",
                json!({
                    "extension": extension,
                }),
            ));
        }

        if structured_extensions
            .iter()
            .any(|value| value == &extension)
        {
            return Ok(read_blocked_result(
                opened.absolute(),
                size_bytes,
                "structured_document",
                json!({
                    "extension": extension,
                }),
            ));
        }

        if size_bytes > ctx.config.file_guard.max_inline_read_bytes {
            return Ok(read_blocked_result(
                opened.absolute(),
                size_bytes,
                "large_file",
                json!({
                    "max_inline_read_bytes": ctx.config.file_guard.max_inline_read_bytes,
                }),
            ));
        }

        let (bytes, exceeded_limit) = opened.with_file(|file| {
            read_up_to_limit(file, ctx.config.file_guard.max_inline_read_bytes)
        })?;
        if exceeded_limit {
            return Ok(read_blocked_result(
                opened.absolute(),
                size_bytes.max(bytes.len() as u64),
                "large_file",
                json!({
                    "max_inline_read_bytes": ctx.config.file_guard.max_inline_read_bytes,
                    "size_is_lower_bound": true,
                }),
            ));
        }
        if content_inspector::inspect(&bytes).is_binary() {
            return Ok(read_blocked_result(
                opened.absolute(),
                size_bytes,
                "binary_content",
                json!({
                    "extension": extension,
                }),
            ));
        }

        let content_sha256 = crate::harness::artifact::hash_bytes(&bytes);
        let decoded = crate::tool::text_encoding::decode_text(bytes)
            .map_err(|_| ToolError::Message("file has no supported text encoding".to_string()))?;
        let source_encoding = decoded.encoding;
        let text = decoded.text;
        let lines = text.lines().collect::<Vec<_>>();
        let offset = input.offset.unwrap_or(1).max(1);
        let limit = input.limit.unwrap_or(2_000).max(1);
        let page = bounded_line_page(
            &lines,
            offset,
            limit,
            ctx.config.tool_output.max_lines,
            ctx.config.tool_output.max_bytes,
        )?;

        let baseline = edit_baseline_decision(
            offset,
            page.visible_line_count,
            lines.len(),
            page.has_more,
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
                path: opened.absolute().to_path_buf(),
                read_at_ms: SystemClock::now_ms(),
                mtime_ms,
                size_bytes: Some(size_bytes),
                content_sha256: Some(content_sha256),
            },
            baseline,
        )?;

        let instruction_sources = find_instruction_sources(
            opened.inside_workspace(),
            opened.relative_to_root(),
            ctx.workspace,
        )?;
        Ok(ToolResult {
            title: format!("Read {}", opened.absolute()),
            output_text: page.output_text,
            metadata: json!({
                "path": opened.absolute(),
                "size_bytes": size_bytes,
                "start_line": offset,
                "end_line": page.end_line,
                "total_lines": lines.len(),
                "encoding": source_encoding.label(),
                "truncated": page.has_more,
                "truncation_kind": if page.has_more { Some("line_page") } else { None },
                "next_offset": page.has_more.then(|| page.end_line.saturating_add(1)),
                "edit_baseline": baseline.metadata(),
                "instruction_sources": instruction_sources,
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
            _internal_file_lease: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MissingReadRequest {
    requested: Utf8PathBuf,
    absolute: Utf8PathBuf,
    basename: String,
    suffix: Option<Utf8PathBuf>,
    max_diagnostic_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MissingCandidateKind {
    Exact,
    SimilarSibling,
}

fn exact_candidate_suffix(
    requested: &Utf8Path,
    absolute: &Utf8Path,
    workspace: &Workspace,
) -> Option<Utf8PathBuf> {
    let source = if requested.is_absolute() {
        PathGuard::relative_path_from_root(absolute, workspace.authority_root())?
    } else {
        requested.to_path_buf()
    };
    let mut suffix = Utf8PathBuf::new();
    for component in source.components() {
        match component {
            Utf8Component::Normal(value) => suffix.push(value),
            Utf8Component::CurDir => {}
            Utf8Component::ParentDir | Utf8Component::RootDir | Utf8Component::Prefix(_) => {
                return None;
            }
        }
    }
    (!suffix.as_str().is_empty()).then_some(suffix)
}

fn is_not_found_error(error: &ToolError) -> bool {
    matches!(
        error,
        ToolError::Workspace(crate::error::WorkspaceError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound
    )
}

fn render_missing_read_diagnostic(
    requested: &Utf8Path,
    scan: &ExactFileCandidateScan,
    candidate_kind: MissingCandidateKind,
    max_bytes: usize,
) -> String {
    let max_bytes = max_bytes.max(1);
    let requested_budget = (max_bytes / 3).min(256);
    let requested = clip_text_with_ellipsis(requested.as_str(), requested_budget);
    let mut output = format!("read path not found: `{requested}`.");
    if output.len() > max_bytes {
        return clip_text_with_ellipsis(&output, max_bytes);
    }

    let summary = match (candidate_kind, scan.paths.is_empty()) {
        (_, true) => {
            " No exact basename or suffix candidate was found in the bounded workspace scan."
        }
        (MissingCandidateKind::Exact, false) => {
            " Exact workspace candidates from the bounded scan (not auto-selected):"
        }
        (MissingCandidateKind::SimilarSibling, false) => {
            " Similar filenames in the same directory (not auto-selected):"
        }
    };
    if !append_diagnostic_segment(&mut output, summary, max_bytes) {
        return output;
    }

    let mut included = 0usize;
    for path in &scan.paths {
        let path = path.as_str().replace('\\', "/");
        let candidate = format!("\n- `{path}`");
        if !append_diagnostic_segment(&mut output, &candidate, max_bytes) {
            break;
        }
        included = included.saturating_add(1);
    }
    if !scan.paths.is_empty() && included == 0 {
        let _ = append_diagnostic_segment(
            &mut output,
            " Candidate paths exceeded the diagnostic byte limit.",
            max_bytes,
        );
    }
    if scan.truncated {
        let _ = append_diagnostic_segment(
            &mut output,
            "\nThe workspace scan stopped at its result or visit limit.",
            max_bytes,
        );
    }
    output
}

fn append_diagnostic_segment(output: &mut String, segment: &str, max_bytes: usize) -> bool {
    if output.len().saturating_add(segment.len()) > max_bytes {
        return false;
    }
    output.push_str(segment);
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundedLinePage {
    output_text: String,
    visible_line_count: usize,
    end_line: usize,
    has_more: bool,
}

fn bounded_line_page(
    lines: &[&str],
    offset: usize,
    requested_limit: usize,
    max_lines: usize,
    max_bytes: usize,
) -> Result<BoundedLinePage, ToolError> {
    let offset = offset.max(1);
    let start_index = offset.saturating_sub(1).min(lines.len());
    let line_limit = requested_limit.max(1).min(max_lines.max(1));
    let byte_limit = max_bytes.max(1);
    let mut output_text = String::new();
    let mut visible_line_count = 0usize;

    for (index, line) in lines.iter().enumerate().skip(start_index).take(line_limit) {
        let rendered = format!("{}: {}", index + 1, line);
        if rendered.len() > byte_limit && visible_line_count == 0 {
            return Err(ToolError::Message(format!(
                "line {} requires {} UTF-8 bytes after numbering, exceeding tool_output.max_bytes={byte_limit}; use a targeted search or shell command for this line",
                index + 1,
                rendered.len()
            )));
        }
        let separator_bytes = usize::from(visible_line_count > 0);
        if output_text
            .len()
            .saturating_add(separator_bytes)
            .saturating_add(rendered.len())
            > byte_limit
        {
            break;
        }
        if separator_bytes > 0 {
            output_text.push('\n');
        }
        output_text.push_str(&rendered);
        visible_line_count = visible_line_count.saturating_add(1);
    }

    let end_line = start_index
        .saturating_add(visible_line_count)
        .min(lines.len());
    Ok(BoundedLinePage {
        output_text,
        visible_line_count,
        end_line,
        has_more: end_line < lines.len(),
    })
}

fn read_up_to_limit(reader: &mut impl Read, max_bytes: u64) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let exceeded_limit = bytes.len() as u64 > max_bytes;
    Ok((bytes, exceeded_limit))
}

fn require_readable_file_metadata(
    path: &Utf8Path,
    metadata: std::fs::Metadata,
) -> Result<std::fs::Metadata, ToolError> {
    if metadata.is_dir() {
        return Err(ToolError::Message(format!("path `{path}` is a directory")));
    }
    Ok(metadata)
}

fn read_blocked_result(
    path: &Utf8Path,
    size_bytes: u64,
    blocked_reason: &str,
    extra_metadata: serde_json::Value,
) -> ToolResult {
    let mut metadata = json!({
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
    ToolResult {
        title: format!("Read blocked: {blocked_reason}"),
        output_text: format!("`{path}` was not read inline: {blocked_reason}."),
        metadata,
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
        _internal_file_lease: None,
    }
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

fn find_instruction_sources(
    inside_workspace: bool,
    relative_to_root: &Utf8Path,
    workspace: &Workspace,
) -> Result<Vec<String>, ToolError> {
    if !inside_workspace || relative_to_root.is_absolute() {
        return Ok(Vec::new());
    }

    let mut sources = Vec::new();
    let mut current = relative_to_root.parent();
    while let Some(relative_dir) = current {
        let dir = workspace.authority_root().join(relative_dir);
        for file_name in instruction_file_names() {
            let candidate = dir.join(file_name);
            let candidate_guard = PathGuard::require_path(workspace, &candidate, AccessKind::Read)
                .map_err(|error| {
                    ToolError::Message(format!(
                        "failed to guard instruction source `{candidate}`: {error}"
                    ))
                })?;
            if !candidate_guard.inside_workspace {
                return Err(ToolError::Message(format!(
                    "instruction source `{candidate}` is outside workspace authority"
                )));
            }
            let file = match PathGuard::open_validated_metadata_handle(&candidate_guard) {
                Ok(file) => file,
                Err(crate::error::WorkspaceError::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound =>
                {
                    continue;
                }
                Err(error) => {
                    return Err(ToolError::Message(format!(
                        "failed to open instruction source `{candidate}`: {error}"
                    )));
                }
            };
            let metadata = file.metadata().map_err(|error| {
                ToolError::Message(format!(
                    "failed to inspect instruction source `{candidate}`: {error}"
                ))
            })?;
            if metadata.is_file() {
                sources.push(candidate_guard.absolute.as_str().replace('\\', "/"));
            }
        }
        if relative_dir.as_str().is_empty() {
            break;
        }
        current = relative_dir.parent();
    }
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use crate::config::ResolvedConfig;
    use crate::edit::{EditSafety, FileReadStamp, read_file_with_identity};
    use crate::session::SessionId;
    use crate::workspace::traversal::ExactFileCandidateScan;
    use crate::workspace::{AccessKind, PathGuard, WorkspaceDiscovery};

    use super::{
        MissingCandidateKind, bounded_line_page, edit_baseline_decision, exact_candidate_suffix,
        find_instruction_sources, is_not_found_error, read_up_to_limit,
        record_edit_baseline_if_eligible, render_missing_read_diagnostic,
        require_readable_file_metadata,
    };

    #[cfg(unix)]
    fn link_file(target: &camino::Utf8Path, link: &camino::Utf8Path) {
        std::os::unix::fs::symlink(target, link).expect("instruction redirect fixture");
    }

    #[cfg(windows)]
    fn link_file(target: &camino::Utf8Path, link: &camino::Utf8Path) {
        std::os::windows::fs::symlink_file(target, link).expect("instruction redirect fixture");
    }

    #[test]
    fn instruction_sources_include_only_workspace_ancestors() {
        let temp = tempfile::tempdir().expect("tempdir");
        let container =
            Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 container");
        let root = container.join("workspace");
        let nested = root.join("nested");
        let deeper = nested.join("deeper");
        std::fs::create_dir_all(&deeper).expect("workspace tree");
        let target = deeper.join("target.txt");
        std::fs::write(&target, "target").expect("target fixture");
        let outside_instruction = container.join("AGENTS.md");
        let root_instruction = root.join("AGENTS.md");
        let nested_instruction = nested.join("AGENTS.md");
        let deeper_instruction = deeper.join("AGENTS.md");
        for instruction in [
            &outside_instruction,
            &root_instruction,
            &nested_instruction,
            &deeper_instruction,
        ] {
            std::fs::write(instruction, "instructions").expect("instruction fixture");
        }
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let guarded = PathGuard::require_path(&workspace, &target, AccessKind::Read)
            .expect("guarded workspace target");

        let sources = find_instruction_sources(
            guarded.inside_workspace,
            &guarded.relative_to_root,
            &workspace,
        )
        .expect("instruction sources");

        assert_eq!(
            sources,
            [&deeper_instruction, &nested_instruction, &root_instruction]
                .into_iter()
                .map(|path| path.as_str().replace('\\', "/"))
                .collect::<Vec<_>>()
        );
        assert!(!sources.contains(&outside_instruction.as_str().replace('\\', "/")));
    }

    #[test]
    fn nested_git_read_instruction_sources_stop_at_selected_authority() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root =
            Utf8PathBuf::from_path_buf(temp.path().join("aaa")).expect("utf8 project root");
        let selected = project_root.join("bbb");
        let nested = selected.join("ccc");
        std::fs::create_dir_all(project_root.join(".git")).expect("git marker");
        std::fs::create_dir_all(&nested).expect("nested directory");
        let project_instruction = project_root.join("AGENTS.md");
        let selected_instruction = selected.join("AGENTS.md");
        let nested_instruction = nested.join("AGENTS.md");
        std::fs::write(&project_instruction, "project").expect("project instruction");
        std::fs::write(&selected_instruction, "selected").expect("selected instruction");
        std::fs::write(&nested_instruction, "nested").expect("nested instruction");
        let target = nested.join("target.txt");
        std::fs::write(&target, "target").expect("target");
        let workspace = WorkspaceDiscovery::discover(&selected, &ResolvedConfig::default())
            .expect("nested workspace");
        let guarded = PathGuard::require_path(&workspace, &target, AccessKind::Read)
            .expect("guarded nested target");

        let sources = find_instruction_sources(
            guarded.inside_workspace,
            &guarded.relative_to_root,
            &workspace,
        )
        .expect("instruction sources");

        assert_eq!(
            sources,
            [&nested_instruction, &selected_instruction]
                .into_iter()
                .map(|path| path.as_str().replace('\\', "/"))
                .collect::<Vec<_>>()
        );
        assert!(!sources.contains(&project_instruction.as_str().replace('\\', "/")));
    }

    #[test]
    fn instruction_sources_are_empty_for_trusted_external_and_internal_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let container =
            Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 container");
        let root = container.join("workspace");
        let external = container.join("external");
        std::fs::create_dir_all(&root).expect("workspace root");
        std::fs::create_dir_all(&external).expect("external root");
        std::fs::write(external.join("AGENTS.md"), "external instructions")
            .expect("external instruction fixture");
        let target = external.join("target.txt");
        std::fs::write(&target, "target").expect("external target fixture");
        let mut config = ResolvedConfig::default();
        config.permissions.additional_read_roots = vec![external.clone()];
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let external_guard = PathGuard::require_path(&workspace, &target, AccessKind::Read)
            .expect("trusted external target");
        let internal_guard =
            PathGuard::trusted_internal_path(&target, &external).expect("trusted internal target");

        assert!(!external_guard.inside_workspace);
        assert!(external_guard.trusted_external);
        assert!(
            find_instruction_sources(
                external_guard.inside_workspace,
                &external_guard.relative_to_root,
                &workspace,
            )
            .expect("external instruction sources")
            .is_empty()
        );
        assert!(!internal_guard.inside_workspace);
        assert!(internal_guard.trusted_external);
        assert!(
            find_instruction_sources(
                internal_guard.inside_workspace,
                &internal_guard.relative_to_root,
                &workspace,
            )
            .expect("internal instruction sources")
            .is_empty()
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn instruction_source_redirect_to_an_additional_read_root_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let container =
            Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 container");
        let root = container.join("workspace");
        let nested = root.join("nested");
        let external = container.join("additional-read-root");
        std::fs::create_dir_all(&nested).expect("workspace tree");
        std::fs::create_dir_all(&external).expect("external root");
        let target = nested.join("target.txt");
        let external_instruction = external.join("AGENTS.md");
        std::fs::write(&target, "target").expect("target fixture");
        std::fs::write(&external_instruction, "external instructions")
            .expect("external instruction fixture");
        link_file(&external_instruction, &nested.join("AGENTS.md"));

        let mut config = ResolvedConfig::default();
        config.permissions.additional_read_roots = vec![external];
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let guarded = PathGuard::require_path(&workspace, &target, AccessKind::Read)
            .expect("guarded workspace target");

        let error = find_instruction_sources(
            guarded.inside_workspace,
            &guarded.relative_to_root,
            &workspace,
        )
        .expect_err("external instruction redirect must not gain workspace authority");

        assert!(error.to_string().contains("outside workspace authority"));
    }

    #[test]
    fn instruction_source_directories_are_not_reported() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root =
            Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 workspace root");
        std::fs::create_dir_all(root.join("AGENTS.md")).expect("instruction-named directory");
        let target = root.join("target.txt");
        std::fs::write(&target, "target").expect("target fixture");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let guarded = PathGuard::require_path(&workspace, &target, AccessKind::Read)
            .expect("guarded workspace target");

        let sources = find_instruction_sources(
            guarded.inside_workspace,
            &guarded.relative_to_root,
            &workspace,
        )
        .expect("instruction sources");

        assert!(sources.is_empty());
    }

    #[test]
    fn directory_read_reports_the_typed_user_visible_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let directory =
            Utf8PathBuf::from_path_buf(temp.path().join("directory")).expect("utf8 directory");
        std::fs::create_dir(&directory).expect("directory fixture");

        let error = require_readable_file_metadata(
            &directory,
            std::fs::metadata(&directory).expect("directory metadata"),
        )
        .expect_err("directories are not readable files");

        assert_eq!(
            error.to_string(),
            format!("path `{directory}` is a directory")
        );
    }

    #[test]
    fn missing_read_suffix_is_exact_and_rejects_parent_traversal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root =
            Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 workspace");
        std::fs::create_dir_all(&root).expect("workspace root");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let absolute = root.join("backend/app/repository.rs");

        assert_eq!(
            exact_candidate_suffix(
                camino::Utf8Path::new("app/repository.rs"),
                &absolute,
                &workspace,
            ),
            Some(Utf8PathBuf::from("app/repository.rs"))
        );
        assert_eq!(
            exact_candidate_suffix(&absolute, &absolute, &workspace),
            Some(Utf8PathBuf::from("backend/app/repository.rs"))
        );
        assert!(
            exact_candidate_suffix(
                camino::Utf8Path::new("../app/repository.rs"),
                &absolute,
                &workspace,
            )
            .is_none()
        );
    }

    #[test]
    fn missing_read_diagnostic_keeps_only_whole_exact_candidates_within_byte_limit() {
        let scan = ExactFileCandidateScan {
            paths: vec![
                Utf8PathBuf::from("a/target.rs"),
                Utf8PathBuf::from("nested/target.rs"),
                Utf8PathBuf::from(format!("very-long/{}/target.rs", "x".repeat(256))),
            ],
            truncated: true,
        };

        let rendered = render_missing_read_diagnostic(
            camino::Utf8Path::new("missing/target.rs"),
            &scan,
            MissingCandidateKind::Exact,
            192,
        );

        assert!(rendered.len() <= 192);
        assert!(rendered.contains("not auto-selected"));
        assert!(rendered.contains("- `a/target.rs`"));
        for line in rendered.lines().filter(|line| line.starts_with("- `")) {
            assert!(line.ends_with('`'));
            assert!(
                scan.paths
                    .iter()
                    .any(|path| { line == format!("- `{}`", path.as_str().replace('\\', "/")) })
            );
        }
    }

    #[test]
    fn similar_sibling_diagnostic_is_explicitly_non_resolving() {
        let scan = ExactFileCandidateScan {
            paths: vec![Utf8PathBuf::from(
                "backend/app/infrastructure/repositories/sqlite_repositories.py",
            )],
            truncated: false,
        };

        let rendered = render_missing_read_diagnostic(
            camino::Utf8Path::new(
                "backend/app/infrastructure/repositories/sqlite_run_repository.py",
            ),
            &scan,
            MissingCandidateKind::SimilarSibling,
            1_024,
        );

        assert!(rendered.contains("Similar filenames in the same directory"));
        assert!(rendered.contains("not auto-selected"));
        assert!(rendered.contains("sqlite_repositories.py"));
    }

    #[test]
    fn only_a_typed_not_found_open_error_enables_candidate_diagnostics() {
        let not_found = crate::error::ToolError::Workspace(crate::error::WorkspaceError::Io(
            std::io::Error::from(std::io::ErrorKind::NotFound),
        ));
        let denied = crate::error::ToolError::Workspace(crate::error::WorkspaceError::Io(
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        ));
        let boundary = crate::error::ToolError::Message("outside allowed roots".to_string());

        assert!(is_not_found_error(&not_found));
        assert!(!is_not_found_error(&denied));
        assert!(!is_not_found_error(&boundary));
    }

    #[cfg(windows)]
    #[test]
    fn instruction_sources_stop_at_workspace_root_for_windows_case_aliases() {
        let temp = tempfile::tempdir().expect("tempdir");
        let container =
            Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 container");
        let root = container.join("Workspace");
        let nested = root.join("Nested");
        std::fs::create_dir_all(&nested).expect("workspace tree");
        let target = nested.join("Target.txt");
        std::fs::write(&target, "target").expect("target fixture");
        let outside_instruction = container.join("AGENTS.md");
        let root_instruction = root.join("AGENTS.md");
        let nested_instruction = nested.join("AGENTS.md");
        for instruction in [&outside_instruction, &root_instruction, &nested_instruction] {
            std::fs::write(instruction, "instructions").expect("instruction fixture");
        }
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let case_alias = Utf8PathBuf::from(target.as_str().to_ascii_uppercase());
        let guarded = PathGuard::require_path(&workspace, &case_alias, AccessKind::Read)
            .expect("case-variant workspace target");

        let sources = find_instruction_sources(
            guarded.inside_workspace,
            &guarded.relative_to_root,
            &workspace,
        )
        .expect("instruction sources");

        assert_eq!(sources.len(), 2);
        assert!(PathGuard::same_path_identity(
            camino::Utf8Path::new(sources[0].as_str()),
            &nested_instruction,
        ));
        assert!(PathGuard::same_path_identity(
            camino::Utf8Path::new(sources[1].as_str()),
            &root_instruction,
        ));
        assert!(sources.iter().all(|source| !PathGuard::same_path_identity(
            camino::Utf8Path::new(source.as_str()),
            &outside_instruction,
        )));
    }

    #[test]
    fn bounded_reader_never_materializes_more_than_limit_plus_one() {
        let mut input = std::io::Cursor::new(vec![b'x'; 4 * 1024]);

        let (bytes, exceeded_limit) = read_up_to_limit(&mut input, 32).expect("bounded read");

        assert!(exceeded_limit);
        assert_eq!(bytes.len(), 33);
    }

    #[test]
    fn bounded_line_page_stops_before_the_first_incomplete_line() {
        let page =
            bounded_line_page(&["aaaa", "bbbb", "cccc"], 1, 10, 10, 15).expect("bounded page");

        assert_eq!(page.output_text, "1: aaaa\n2: bbbb");
        assert_eq!(page.visible_line_count, 2);
        assert_eq!(page.end_line, 2);
        assert!(page.has_more);
        assert!(page.output_text.len() <= 15);
    }

    #[test]
    fn bounded_line_page_applies_offset_requested_limit_and_max_lines() {
        let limited =
            bounded_line_page(&["one", "two", "three"], 2, 1, 10, 1_024).expect("limited page");
        assert_eq!(limited.output_text, "2: two");
        assert_eq!(limited.end_line, 2);
        assert!(limited.has_more);

        let max_lines =
            bounded_line_page(&["one", "two", "three"], 1, 10, 2, 1_024).expect("line page");
        assert_eq!(max_lines.output_text, "1: one\n2: two");
        assert_eq!(max_lines.visible_line_count, 2);
        assert_eq!(max_lines.end_line, 2);
        assert!(max_lines.has_more);
    }

    #[test]
    fn bounded_line_page_rejects_a_single_line_that_cannot_be_shown_completely() {
        let error = bounded_line_page(&["0123456789"], 1, 1, 1, 8)
            .expect_err("partial source lines must not be projected");

        assert!(error.to_string().contains("line 1 requires"));
        assert!(error.to_string().contains("tool_output.max_bytes=8"));
    }

    #[test]
    fn bounded_line_page_marks_a_complete_file_as_complete() {
        let page =
            bounded_line_page(&["one", "two"], 1, 2_000, 2_000, 50 * 1024).expect("full page");

        assert_eq!(page.output_text, "1: one\n2: two");
        assert_eq!(page.end_line, 2);
        assert!(!page.has_more);
    }

    fn stamp_for(path: &camino::Utf8Path) -> (FileReadStamp, crate::edit::FileContentIdentity) {
        let (_, identity) = read_file_with_identity(path, 1_024).expect("read identity");
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
        let error = safety
            .assert_fresh_write(session_id, &path, &identity)
            .expect_err("write after partial read must be rejected")
            .to_string();
        assert!(error.contains("no full-content write baseline exists"));
        assert!(error.contains("use apply_patch for a targeted update"));
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
        let error = safety
            .assert_fresh_write(session_id, &path, &identity)
            .expect_err("write after preview-truncated read must be rejected")
            .to_string();
        assert!(error.contains("no full-content write baseline exists"));
        assert!(error.contains("use apply_patch for a targeted update"));
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
