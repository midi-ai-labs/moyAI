use std::cmp::Reverse;
use std::fs;
use std::io::Read as _;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;

use crate::config::model::FileGuardConfig;
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::truncate::clip_text_with_ellipsis;
use crate::tool::{ToolName, ToolResult, ToolSpec};
use crate::workspace::{AccessKind, PathGuard};

#[derive(Debug, Deserialize)]
pub struct ListInput {
    pub path: Option<Utf8PathBuf>,
    pub limit: Option<usize>,
    pub include_hidden: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct GlobInput {
    pub pattern: String,
    pub path: Option<Utf8PathBuf>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct GrepInput {
    pub pattern: String,
    pub path: Option<Utf8PathBuf>,
    pub include_glob: Option<String>,
    pub case_sensitive: Option<bool>,
    pub limit: Option<usize>,
}

#[derive(Debug, Default)]
pub struct ListTool;

#[derive(Debug, Default)]
pub struct GlobTool;

#[derive(Debug, Default)]
pub struct GrepTool;

const GREP_BINARY_SAMPLE_BYTES: u64 = 8 * 1024;
const GREP_MAX_SKIP_DETAILS: usize = 64;
const GREP_MAX_SKIP_DETAIL_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GrepSkipReason {
    BlockedExtension,
    StructuredDocument,
    LargeFile,
    BinaryContent,
}

impl GrepSkipReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::BlockedExtension => "blocked_extension",
            Self::StructuredDocument => "structured_document",
            Self::LargeFile => "large_file",
            Self::BinaryContent => "binary_content",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::BlockedExtension => 0,
            Self::StructuredDocument => 1,
            Self::LargeFile => 2,
            Self::BinaryContent => 3,
        }
    }
}

#[derive(Debug)]
struct SkippedGrepCandidate {
    path: Utf8PathBuf,
    reason: GrepSkipReason,
}

#[derive(Debug, Default)]
struct GrepSkipSummary {
    total: usize,
    reason_counts: [usize; 4],
    details: Vec<SkippedGrepCandidate>,
    detail_bytes: usize,
}

impl GrepSkipSummary {
    fn record(&mut self, path: Utf8PathBuf, reason: GrepSkipReason) {
        self.total = self.total.saturating_add(1);
        self.reason_counts[reason.index()] = self.reason_counts[reason.index()].saturating_add(1);

        let rendered_bytes = path
            .as_str()
            .len()
            .saturating_add(reason.as_str().len())
            .saturating_add(6);
        if self.details.len() < GREP_MAX_SKIP_DETAILS
            && self.detail_bytes.saturating_add(rendered_bytes) <= GREP_MAX_SKIP_DETAIL_BYTES
        {
            self.details.push(SkippedGrepCandidate { path, reason });
            self.detail_bytes = self.detail_bytes.saturating_add(rendered_bytes);
        }
    }

    fn omitted_count(&self) -> usize {
        self.total.saturating_sub(self.details.len())
    }

    fn reason_count(&self, reason: GrepSkipReason) -> usize {
        self.reason_counts[reason.index()]
    }
}

#[async_trait(?Send)]
impl Tool for ListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::List,
            description: "List files and directories under a path",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "limit": { "type": "integer" },
                    "include_hidden": { "type": "boolean" }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: serde_json::Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::from_value::<ListInput>(raw_arguments)?;
        let requested = input.path.unwrap_or_else(|| Utf8PathBuf::from("."));
        let guarded = PathGuard::require_path(ctx.workspace, &requested, AccessKind::List)?;
        ctx.confirm_if_needed(
            AccessKind::List,
            format!("List {}", guarded.absolute),
            vec![guarded.absolute.clone()],
            !guarded.inside_workspace && !guarded.trusted_external,
            Vec::new(),
        )
        .await?
        .admit()?;
        if !guarded.absolute.exists() {
            return Ok(missing_directory_result(&guarded.absolute));
        }
        if guarded.absolute.is_file() {
            return Ok(file_listing_redirect_result(&guarded.absolute));
        }
        if !guarded.absolute.is_dir() {
            return Err(ToolError::Message(format!(
                "`{}` is not a directory",
                guarded.absolute
            )));
        }

        let limit = bounded_result_limit(input.limit, ctx.config.tool_output.max_results);
        let entries = collect_entries(
            &guarded.absolute,
            ctx.workspace,
            input.include_hidden.unwrap_or(false),
        )?;
        let mut lines = Vec::new();
        for entry in entries.iter().take(limit) {
            lines.push(entry.clone());
        }
        let output_text = lines.join("\n");
        let preview = ctx.services.truncator.preview(
            output_text,
            &ctx.config.tool_output,
            &ctx.services.storage_paths,
        )?;

        Ok(ToolResult {
            title: format!("Listed {}", guarded.absolute),
            output_text: preview.preview_text,
            metadata: json!({
                "root": guarded.absolute,
                "entry_count": entries.len(),
                "truncated": preview.truncated
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }
}

fn missing_directory_result(path: &Utf8Path) -> ToolResult {
    ToolResult {
        title: format!("Directory `{path}` does not exist yet"),
        output_text: format!(
            "`{path}` does not exist yet. Do not keep retrying `list` on the same missing path. If the user already named a file under this path, create it directly with `write`; the `write` tool creates missing parent directories automatically. If you need discovery first, list the nearest existing parent directory instead."
        ),
        metadata: json!({
            "corrective_result": true,
            "missing_directory": true,
            "path": path,
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    }
}

fn file_listing_redirect_result(path: &Utf8Path) -> ToolResult {
    ToolResult {
        title: format!("`{path}` is a file"),
        output_text: format!(
            "`{path}` is a file, not a directory. Use `read` to inspect its contents, or list its parent directory if you need surrounding files."
        ),
        metadata: json!({
            "corrective_result": true,
            "path_is_file": true,
            "path": path,
        }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    }
}

#[async_trait(?Send)]
impl Tool for GlobTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::Glob,
            description: "Find files by glob pattern",
            input_schema: json!({
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
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
        let input = serde_json::from_value::<GlobInput>(raw_arguments)?;
        let requested = input.path.unwrap_or_else(|| Utf8PathBuf::from("."));
        let guarded = PathGuard::require_path(ctx.workspace, &requested, AccessKind::Search)?;
        ctx.confirm_if_needed(
            AccessKind::Search,
            format!("Glob {}", guarded.absolute),
            vec![guarded.absolute.clone()],
            !guarded.inside_workspace && !guarded.trusted_external,
            Vec::new(),
        )
        .await?
        .admit()?;

        let mut builder = GlobSetBuilder::new();
        builder.add(
            Glob::new(&input.pattern)
                .map_err(|error| ToolError::Message(format!("invalid glob pattern: {error}")))?,
        );
        let matcher = builder.build().map_err(|error| {
            ToolError::Message(format!("failed to compile glob pattern: {error}"))
        })?;
        let mut matches = collect_file_metadata(&guarded.absolute, ctx.workspace)?;
        matches.retain(|(path, _)| {
            let path = Utf8Path::new(path);
            glob_matches_path(&matcher, path, &guarded.absolute, &ctx.workspace.root)
        });
        matches.sort_by_key(|(_, modified)| Reverse(*modified));
        let limit = bounded_result_limit(input.limit, ctx.config.tool_output.max_results);
        let lines = matches
            .iter()
            .take(limit)
            .map(|(path, _)| glob_output_label(path, &ctx.workspace.root))
            .collect::<Vec<_>>();
        let preview = ctx.services.truncator.preview(
            lines.join("\n"),
            &ctx.config.tool_output,
            &ctx.services.storage_paths,
        )?;

        Ok(ToolResult {
            title: format!("Glob {}", input.pattern),
            output_text: preview.preview_text,
            metadata: json!({
                "match_count": matches.len(),
                "truncated": preview.truncated
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }
}

fn glob_matches_path(
    matcher: &globset::GlobSet,
    path: &Utf8Path,
    search_root: &Utf8Path,
    workspace_root: &Utf8Path,
) -> bool {
    matcher.is_match(path.as_str())
        || path
            .strip_prefix(search_root)
            .ok()
            .is_some_and(|relative| matcher.is_match(relative.as_str()))
        || path
            .strip_prefix(workspace_root)
            .ok()
            .is_some_and(|relative| matcher.is_match(relative.as_str()))
}

fn glob_output_label(path: &str, workspace_root: &Utf8Path) -> String {
    let path = Utf8Path::new(path);
    path.strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string()
}

#[async_trait(?Send)]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::Grep,
            description: "Search file contents with a regex pattern",
            input_schema: json!({
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "include_glob": { "type": "string" },
                    "case_sensitive": { "type": "boolean" },
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
        let input = serde_json::from_value::<GrepInput>(raw_arguments)?;
        let requested = input.path.unwrap_or_else(|| Utf8PathBuf::from("."));
        let guarded =
            crate::tool::internal_output::resolve_path(&ctx, &requested, AccessKind::Search)
                .await?;
        ctx.confirm_if_needed(
            AccessKind::Search,
            format!("Grep {}", guarded.absolute),
            vec![guarded.absolute.clone()],
            !guarded.inside_workspace && !guarded.trusted_external,
            Vec::new(),
        )
        .await?
        .admit()?;

        let pattern = if input.case_sensitive.unwrap_or(false) {
            input.pattern.clone()
        } else {
            format!("(?i:{})", input.pattern)
        };
        let regex = Regex::new(&pattern)
            .map_err(|error| ToolError::Message(format!("invalid regex pattern: {error}")))?;
        let include_glob = input
            .include_glob
            .as_deref()
            .map(compile_include_glob)
            .transpose()?;

        let mut files = collect_file_metadata(&guarded.absolute, ctx.workspace)?;
        files.sort_by_key(|(_, modified)| Reverse(*modified));
        let limit = bounded_result_limit(input.limit, ctx.config.tool_output.max_results);
        let mut matches = Vec::new();
        let mut skipped = GrepSkipSummary::default();
        for (path, _) in files {
            if matches.len() >= limit {
                break;
            }
            if include_glob
                .as_ref()
                .map(|glob| glob.is_match(path.as_str()))
                .unwrap_or(true)
            {
                let path = Utf8PathBuf::from(path);
                match read_grep_candidate(path.as_path(), &ctx.config.file_guard)? {
                    Ok(text) => {
                        for (line_index, line) in text.lines().enumerate() {
                            if regex.is_match(line) {
                                matches.push(format!(
                                    "{}:{}: {}",
                                    path,
                                    line_index + 1,
                                    truncate_line(line)
                                ));
                                if matches.len() >= limit {
                                    break;
                                }
                            }
                        }
                    }
                    Err(reason) => skipped.record(path, reason),
                }
            }
            if matches.len() >= limit {
                break;
            }
        }

        let output_text = render_grep_output(&matches, &skipped);
        let preview = ctx.services.truncator.preview(
            output_text,
            &ctx.config.tool_output,
            &ctx.services.storage_paths,
        )?;

        Ok(ToolResult {
            title: format!("Grep {}", input.pattern),
            output_text: preview.preview_text,
            metadata: json!({
                "total_matches": matches.len(),
                "skipped_file_count": skipped.total,
                "skipped_file_detail_count": skipped.details.len(),
                "skipped_file_details_omitted": skipped.omitted_count(),
                "skipped_reason_counts": {
                    "blocked_extension": skipped.reason_count(GrepSkipReason::BlockedExtension),
                    "structured_document": skipped.reason_count(GrepSkipReason::StructuredDocument),
                    "large_file": skipped.reason_count(GrepSkipReason::LargeFile),
                    "binary_content": skipped.reason_count(GrepSkipReason::BinaryContent),
                },
                "skipped_files": skipped.details.iter().map(|candidate| json!({
                    "path": candidate.path,
                    "reason": candidate.reason.as_str(),
                })).collect::<Vec<_>>(),
                "truncated": preview.truncated
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }
}

fn compile_include_glob(value: &str) -> Result<globset::GlobSet, ToolError> {
    let mut builder = GlobSetBuilder::new();
    let glob = Glob::new(value)
        .map_err(|error| ToolError::Message(format!("invalid include_glob: {error}")))?;
    builder.add(glob);
    builder
        .build()
        .map_err(|error| ToolError::Message(format!("invalid include_glob: {error}")))
}

fn bounded_result_limit(requested: Option<usize>, configured_max: usize) -> usize {
    requested.unwrap_or(configured_max).min(configured_max)
}

fn read_grep_candidate(
    path: &Utf8Path,
    guard: &FileGuardConfig,
) -> Result<Result<String, GrepSkipReason>, ToolError> {
    let metadata = fs::metadata(path)?;
    let extension = normalized_extension(path);
    if normalized_extension_list(&guard.blocked_read_extensions)
        .iter()
        .any(|blocked| blocked == &extension)
    {
        return Ok(Err(GrepSkipReason::BlockedExtension));
    }
    if normalized_extension_list(&guard.structured_document_extensions)
        .iter()
        .any(|structured| structured == &extension)
    {
        return Ok(Err(GrepSkipReason::StructuredDocument));
    }
    if metadata.len() > guard.max_inline_read_bytes {
        return Ok(Err(GrepSkipReason::LargeFile));
    }

    let mut file = fs::File::open(path)?;
    let mut sample = Vec::new();
    file.by_ref()
        .take(GREP_BINARY_SAMPLE_BYTES)
        .read_to_end(&mut sample)?;
    if content_inspector::inspect(&sample).is_binary() {
        return Ok(Err(GrepSkipReason::BinaryContent));
    }

    let max_bytes = guard.max_inline_read_bytes.min(usize::MAX as u64) as usize;
    let mut bytes = sample;
    file.take(max_bytes.saturating_sub(bytes.len()).saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Ok(Err(GrepSkipReason::LargeFile));
    }
    if content_inspector::inspect(&bytes).is_binary() {
        return Ok(Err(GrepSkipReason::BinaryContent));
    }
    crate::tool::text_encoding::decode_text(bytes)
        .map(|decoded| Ok(decoded.text))
        .map_err(|_| {
            ToolError::Message("grep candidate is neither UTF-8 nor Shift_JIS".to_string())
        })
}

fn render_grep_output(matches: &[String], skipped: &GrepSkipSummary) -> String {
    let mut sections = Vec::new();
    if !matches.is_empty() {
        sections.push(matches.join("\n"));
    }
    if skipped.total > 0 {
        let mut lines = vec![format!(
            "Skipped {} file(s) before full-text search because of file guards:",
            skipped.total
        )];
        lines.push(format!(
            "- reasons: blocked_extension={}, structured_document={}, large_file={}, binary_content={}",
            skipped.reason_count(GrepSkipReason::BlockedExtension),
            skipped.reason_count(GrepSkipReason::StructuredDocument),
            skipped.reason_count(GrepSkipReason::LargeFile),
            skipped.reason_count(GrepSkipReason::BinaryContent),
        ));
        lines.extend(
            skipped
                .details
                .iter()
                .map(|candidate| format!("- {} ({})", candidate.path, candidate.reason.as_str())),
        );
        if skipped.omitted_count() > 0 {
            lines.push(format!(
                "- {} additional skipped file detail(s) omitted by the bounded output policy",
                skipped.omitted_count()
            ));
        }
        sections.push(lines.join("\n"));
    }
    sections.join("\n\n")
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

fn collect_entries(
    root: &Utf8Path,
    workspace: &crate::workspace::Workspace,
    include_hidden: bool,
) -> Result<Vec<String>, ToolError> {
    let ignore = workspace.ignore.compile()?;
    let mut builder = WalkBuilder::new(root);
    builder.hidden(!include_hidden);
    builder.git_ignore(workspace.ignore.use_gitignore);
    let mut entries = Vec::new();
    for entry in builder.build() {
        let entry = entry.map_err(|error| ToolError::Message(error.to_string()))?;
        let path = Utf8PathBuf::from_path_buf(entry.path().to_path_buf())
            .map_err(|_| ToolError::Message("path is not valid UTF-8".to_string()))?;
        if path == root {
            continue;
        }
        if workspace
            .protected_paths
            .iter()
            .any(|value| path.starts_with(value))
        {
            continue;
        }
        if workspace
            .ignore
            .matches_compiled(&ignore, &workspace.root, &path)
        {
            continue;
        }
        let label = if entry
            .file_type()
            .map(|value| value.is_dir())
            .unwrap_or(false)
        {
            format!("{}/", path.strip_prefix(root).unwrap_or(&path))
        } else {
            path.strip_prefix(root).unwrap_or(&path).to_string()
        };
        entries.push(label);
    }
    entries.sort();
    Ok(entries)
}

fn collect_file_metadata(
    root: &Utf8Path,
    workspace: &crate::workspace::Workspace,
) -> Result<Vec<(String, i64)>, ToolError> {
    let ignore = workspace.ignore.compile()?;
    let mut builder = WalkBuilder::new(root);
    builder.hidden(false);
    builder.git_ignore(workspace.ignore.use_gitignore);
    let mut entries = Vec::new();
    for entry in builder.build() {
        let entry = entry.map_err(|error| ToolError::Message(error.to_string()))?;
        if !entry
            .file_type()
            .map(|value| value.is_file())
            .unwrap_or(false)
        {
            continue;
        }
        let path = Utf8PathBuf::from_path_buf(entry.path().to_path_buf())
            .map_err(|_| ToolError::Message("path is not valid UTF-8".to_string()))?;
        if workspace
            .protected_paths
            .iter()
            .any(|value| path.starts_with(value))
        {
            continue;
        }
        if workspace
            .ignore
            .matches_compiled(&ignore, &workspace.root, &path)
        {
            continue;
        }
        let modified = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_secs() as i64)
            .unwrap_or_default();
        entries.push((path.to_string(), modified));
    }
    Ok(entries)
}

fn truncate_line(line: &str) -> String {
    const LIMIT: usize = 2_000;
    if line.len() <= LIMIT {
        line.to_string()
    } else {
        clip_text_with_ellipsis(line, LIMIT + 3)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use camino::Utf8PathBuf;

    use super::{
        GREP_MAX_SKIP_DETAIL_BYTES, GREP_MAX_SKIP_DETAILS, GrepSkipReason, GrepSkipSummary,
        bounded_result_limit, compile_include_glob, read_grep_candidate, render_grep_output,
    };

    #[test]
    fn invalid_include_glob_returns_typed_tool_error() {
        let error = compile_include_glob("[").expect_err("invalid glob must be rejected");
        assert!(error.to_string().contains("invalid include_glob"));
    }

    #[test]
    fn requested_result_limit_cannot_exceed_the_configured_output_bound() {
        assert_eq!(bounded_result_limit(Some(usize::MAX), 64), 64);
        assert_eq!(bounded_result_limit(Some(12), 64), 12);
        assert_eq!(bounded_result_limit(None, 64), 64);
        assert_eq!(bounded_result_limit(Some(0), 64), 0);
    }

    #[test]
    fn grep_candidates_apply_file_guards_before_full_text_search() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");
        let mut guard = crate::config::ResolvedConfig::default().file_guard;
        guard.max_inline_read_bytes = 8;

        let blocked = root.join("weights.bin");
        fs::write(&blocked, b"text").expect("write blocked fixture");
        assert_eq!(
            read_grep_candidate(&blocked, &guard).expect("guard result"),
            Err(GrepSkipReason::BlockedExtension)
        );

        let large = root.join("large.txt");
        fs::write(&large, b"0123456789").expect("write large fixture");
        assert_eq!(
            read_grep_candidate(&large, &guard).expect("guard result"),
            Err(GrepSkipReason::LargeFile)
        );

        let binary = root.join("binary.txt");
        fs::write(&binary, [0_u8, 1, 2]).expect("write binary fixture");
        assert_eq!(
            read_grep_candidate(&binary, &guard).expect("guard result"),
            Err(GrepSkipReason::BinaryContent)
        );

        let text = root.join("source.txt");
        fs::write(&text, b"needle").expect("write text fixture");
        assert_eq!(
            read_grep_candidate(&text, &guard).expect("guard result"),
            Ok("needle".to_string())
        );
    }

    #[test]
    fn grep_candidate_decodes_shift_jis_text() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("sjis.txt")).expect("utf8 path");
        let (bytes, _, had_errors) = encoding_rs::SHIFT_JIS.encode("検索対象です");
        assert!(!had_errors);
        fs::write(&path, bytes.as_ref()).expect("write Shift_JIS fixture");

        assert_eq!(
            read_grep_candidate(&path, &crate::config::ResolvedConfig::default().file_guard)
                .expect("grep candidate"),
            Ok("検索対象です".to_string())
        );
    }

    #[test]
    fn grep_skip_details_are_bounded_while_totals_and_reasons_remain_exact() {
        let mut skipped = GrepSkipSummary::default();
        for index in 0..(GREP_MAX_SKIP_DETAILS + 25) {
            skipped.record(
                Utf8PathBuf::from(format!("fixtures/{index:04}/artifact.bin")),
                GrepSkipReason::BinaryContent,
            );
        }

        assert_eq!(skipped.total, GREP_MAX_SKIP_DETAILS + 25);
        assert!(skipped.details.len() <= GREP_MAX_SKIP_DETAILS);
        assert!(skipped.detail_bytes <= GREP_MAX_SKIP_DETAIL_BYTES);
        assert_eq!(
            skipped.reason_count(GrepSkipReason::BinaryContent),
            GREP_MAX_SKIP_DETAILS + 25
        );
        assert_eq!(skipped.omitted_count(), 25);

        let rendered = render_grep_output(&[], &skipped);
        assert!(rendered.contains("additional skipped file detail(s) omitted"));
        assert!(rendered.contains(&format!("binary_content={}", GREP_MAX_SKIP_DETAILS + 25)));
    }

    #[test]
    fn grep_skip_detail_byte_budget_rejects_an_oversized_path() {
        let mut skipped = GrepSkipSummary::default();
        skipped.record(
            Utf8PathBuf::from("x".repeat(GREP_MAX_SKIP_DETAIL_BYTES + 1)),
            GrepSkipReason::LargeFile,
        );

        assert_eq!(skipped.total, 1);
        assert!(skipped.details.is_empty());
        assert_eq!(skipped.omitted_count(), 1);
    }
}
