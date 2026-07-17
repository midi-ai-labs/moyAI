use std::fs;
use std::io::Read as _;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use camino::{Utf8Path, Utf8PathBuf};
use globset::{Glob, GlobSetBuilder};
use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::config::model::FileGuardConfig;
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::truncate::clip_text_with_ellipsis;
use crate::tool::{ToolName, ToolResult, ToolSpec};
use crate::workspace::traversal::{TraversalEntry, TraversalOptions, walk_page};
use crate::workspace::{AccessKind, PathGuard};

#[derive(Debug, Deserialize)]
pub struct ListInput {
    pub path: Option<Utf8PathBuf>,
    pub limit: Option<usize>,
    pub include_hidden: Option<bool>,
    pub max_depth: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GlobInput {
    pub pattern: String,
    pub path: Option<Utf8PathBuf>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GrepInput {
    pub pattern: String,
    pub path: Option<Utf8PathBuf>,
    pub include_glob: Option<String>,
    pub case_sensitive: Option<bool>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
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
const MAX_DISCOVERY_CANDIDATES: usize = 4_096;
const MIN_DISCOVERY_VISITS: usize = 128;

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
            effect: crate::tool::ToolEffectPolicy::read(),
            description: "List one bounded page of files and directories under a path.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1 },
                    "include_hidden": { "type": "boolean" },
                    "max_depth": { "type": "integer", "minimum": 1 },
                    "cursor": { "type": "string", "description": "Continuation returned by a previous list call with the same path and options." }
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
        PathGuard::revalidate(&guarded)?;
        require_directory(&guarded.absolute)?;

        let limit = bounded_result_limit(input.limit, ctx.config.tool_output.max_results);
        let page = walk_page(
            &guarded.absolute,
            ctx.workspace,
            input.cursor.as_deref(),
            TraversalOptions {
                include_hidden: input.include_hidden.unwrap_or(false),
                max_depth: input.max_depth,
                include_files: true,
                include_directories: true,
                result_limit: limit,
                visit_limit: discovery_visit_limit(limit),
            },
        )?;
        let rendered = render_entries_bounded(
            &page.entries,
            ctx.config.tool_output.max_lines,
            ctx.config.tool_output.max_bytes,
            |entry| {
                let mut label = entry.relative_path.as_str().replace('\\', "/");
                if entry.is_directory {
                    label.push('/');
                }
                label
            },
        );
        let continuation = rendered.continuation.or(page.continuation);

        Ok(ToolResult {
            title: format!("Listed {}", guarded.absolute),
            output_text: rendered.output_text,
            metadata: json!({
                "root": guarded.absolute,
                "entry_count": rendered.rendered_count,
                "visited_entries": page.visited_entries,
                "continuation": continuation,
                "truncated": continuation.is_some(),
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
            _internal_file_lease: None,
        })
    }
}

#[async_trait(?Send)]
impl Tool for GlobTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::Glob,
            effect: crate::tool::ToolEffectPolicy::read(),
            description: "Find one bounded page of files by glob pattern.",
            input_schema: json!({
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1 },
                    "cursor": { "type": "string", "description": "Continuation returned by a previous glob call with the same pattern, path, and options." }
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
        PathGuard::revalidate(&guarded)?;
        require_directory(&guarded.absolute)?;

        let mut builder = GlobSetBuilder::new();
        builder.add(
            Glob::new(&input.pattern)
                .map_err(|error| ToolError::Message(format!("invalid glob pattern: {error}")))?,
        );
        let matcher = builder.build().map_err(|error| {
            ToolError::Message(format!("failed to compile glob pattern: {error}"))
        })?;
        let query_digest = search_query_digest("glob-v1", &[input.pattern.as_str()]);
        let walk_cursor = decode_glob_cursor(input.cursor.as_deref(), &query_digest)?;
        let limit = bounded_result_limit(input.limit, ctx.config.tool_output.max_results);
        let candidate_limit = discovery_candidate_limit(limit);
        let page = walk_page(
            &guarded.absolute,
            ctx.workspace,
            walk_cursor.as_deref(),
            TraversalOptions {
                include_hidden: false,
                max_depth: None,
                include_files: true,
                include_directories: false,
                result_limit: candidate_limit,
                visit_limit: discovery_visit_limit(candidate_limit),
            },
        )?;
        let matching = page
            .entries
            .iter()
            .filter(|entry| {
                glob_matches_path(
                    &matcher,
                    &entry.path,
                    &guarded.absolute,
                    &ctx.workspace.root,
                )
            })
            .take(limit.saturating_add(1))
            .cloned()
            .collect::<Vec<_>>();
        let visible = matching.iter().take(limit).cloned().collect::<Vec<_>>();
        let rendered = render_entries_bounded(
            &visible,
            ctx.config.tool_output.max_lines,
            ctx.config.tool_output.max_bytes,
            |entry| glob_output_label(&entry.path, &ctx.workspace.root),
        );
        let limit_continuation = matching.get(limit).map(|entry| entry.cursor.clone());
        let continuation = rendered
            .continuation
            .or(limit_continuation)
            .or(page.continuation)
            .map(|cursor| encode_glob_cursor(&cursor, &query_digest))
            .transpose()?;

        Ok(ToolResult {
            title: format!("Glob {}", input.pattern),
            output_text: rendered.output_text,
            metadata: json!({
                "match_count": rendered.rendered_count,
                "candidate_count": page.entries.len(),
                "visited_entries": page.visited_entries,
                "continuation": continuation,
                "truncated": continuation.is_some(),
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
            _internal_file_lease: None,
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

fn glob_output_label(path: &Utf8Path, workspace_root: &Utf8Path) -> String {
    path.strip_prefix(workspace_root)
        .unwrap_or(path)
        .as_str()
        .replace('\\', "/")
}

#[async_trait(?Send)]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::Grep,
            effect: crate::tool::ToolEffectPolicy::read(),
            description: "Search one bounded page of text files with a regex pattern.",
            input_schema: json!({
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "include_glob": { "type": "string" },
                    "case_sensitive": { "type": "boolean" },
                    "limit": { "type": "integer", "minimum": 1 },
                    "cursor": { "type": "string", "description": "Continuation returned by a previous grep call with the same query." }
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

        PathGuard::revalidate(&guarded)?;

        let case_sensitive = input.case_sensitive.unwrap_or(false);
        let query_digest = search_query_digest(
            "grep-v3",
            &[
                input.pattern.as_str(),
                if case_sensitive {
                    "case-sensitive"
                } else {
                    "case-insensitive"
                },
                if input.include_glob.is_some() {
                    "include-glob"
                } else {
                    "no-include-glob"
                },
                input.include_glob.as_deref().unwrap_or(""),
            ],
        );
        let pattern = if case_sensitive {
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
        let (walk_cursor, first_line_offset) =
            decode_grep_cursor(input.cursor.as_deref(), &query_digest)?;
        let limit = bounded_result_limit(input.limit, ctx.config.tool_output.max_results)
            .min(ctx.config.tool_output.max_lines.max(1));
        let candidate_limit = discovery_candidate_limit(limit);

        let page = if guarded.absolute.is_file() {
            single_file_page(&guarded.absolute, walk_cursor.as_deref())?
        } else {
            require_directory(&guarded.absolute)?;
            walk_page(
                &guarded.absolute,
                ctx.workspace,
                walk_cursor.as_deref(),
                TraversalOptions {
                    include_hidden: false,
                    max_depth: None,
                    include_files: true,
                    include_directories: false,
                    result_limit: candidate_limit,
                    visit_limit: discovery_visit_limit(candidate_limit),
                },
            )?
        };

        let mut matches = Vec::new();
        let mut matches_bytes = 0usize;
        let mut skipped = GrepSkipSummary::default();
        let mut continuation = None;
        let mut scanned_files = 0usize;
        let output_byte_limit = ctx.config.tool_output.max_bytes.max(1);

        'files: for (file_index, entry) in page.entries.iter().enumerate() {
            if !include_glob
                .as_ref()
                .map(|glob| glob.is_match(entry.path.as_str()))
                .unwrap_or(true)
            {
                continue;
            }
            scanned_files = scanned_files.saturating_add(1);
            match read_grep_candidate(&entry.path, &ctx.config.file_guard)? {
                Ok(text) => {
                    let line_offset = if file_index == 0 {
                        first_line_offset
                    } else {
                        0
                    };
                    for (line_index, line) in text.lines().enumerate().skip(line_offset) {
                        if !regex.is_match(line) {
                            continue;
                        }
                        let mut rendered =
                            format!("{}:{}: {}", entry.path, line_index + 1, truncate_line(line));
                        if rendered.len() > output_byte_limit {
                            rendered = clip_text_with_ellipsis(&rendered, output_byte_limit);
                        }
                        let separator = usize::from(!matches.is_empty());
                        if matches.len() >= limit
                            || matches_bytes
                                .saturating_add(separator)
                                .saturating_add(rendered.len())
                                > output_byte_limit
                        {
                            continuation = Some(encode_grep_cursor(
                                &entry.cursor,
                                line_index,
                                &query_digest,
                            )?);
                            break 'files;
                        }
                        matches_bytes = matches_bytes
                            .saturating_add(separator)
                            .saturating_add(rendered.len());
                        matches.push(rendered);
                        if matches.len() >= limit {
                            continuation = Some(encode_grep_cursor(
                                &entry.cursor,
                                line_index.saturating_add(1),
                                &query_digest,
                            )?);
                            break 'files;
                        }
                    }
                }
                Err(reason) => skipped.record(entry.path.clone(), reason),
            }
        }
        if continuation.is_none() {
            continuation = page
                .continuation
                .as_deref()
                .map(|cursor| encode_grep_cursor(cursor, 0, &query_digest))
                .transpose()?;
        }

        Ok(ToolResult {
            title: format!("Grep {}", input.pattern),
            output_text: matches.join("\n"),
            metadata: json!({
                "match_count": matches.len(),
                "scanned_file_count": scanned_files,
                "visited_entries": page.visited_entries,
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
                "continuation": continuation,
                "truncated": continuation.is_some(),
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
            _internal_file_lease: None,
        })
    }
}

fn single_file_page(
    path: &Utf8Path,
    cursor: Option<&str>,
) -> Result<crate::workspace::traversal::TraversalPage, ToolError> {
    let expected_cursor = single_file_cursor(path)?;
    if cursor.is_some_and(|cursor| cursor != expected_cursor) {
        return Err(ToolError::Message(
            "grep cursor does not identify the requested file".to_string(),
        ));
    }
    let entries = if cursor.is_none() || cursor == Some(expected_cursor.as_str()) {
        vec![TraversalEntry {
            path: path.to_path_buf(),
            relative_path: Utf8PathBuf::from(path.file_name().unwrap_or(path.as_str())),
            is_directory: false,
            depth: 1,
            cursor: expected_cursor,
        }]
    } else {
        Vec::new()
    };
    Ok(crate::workspace::traversal::TraversalPage {
        entries,
        continuation: None,
        truncated: false,
        visited_entries: 1,
    })
}

fn single_file_cursor(path: &Utf8Path) -> Result<String, ToolError> {
    let metadata = fs::metadata(path)?;
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos())
        .ok_or_else(|| {
            ToolError::Message(format!(
                "filesystem does not expose a grep continuation fence for `{path}`"
            ))
        })?;
    let mut hasher = Sha256::new();
    hasher.update(path.as_str().as_bytes());
    hasher.update(metadata.len().to_le_bytes());
    hasher.update(modified_nanos.to_le_bytes());
    Ok(format!("single-file-v1:{:x}", hasher.finalize()))
}

fn search_query_digest(domain: &str, values: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for value in std::iter::once(domain).chain(values.iter().copied()) {
        hasher.update((value.len() as u64).to_le_bytes());
        hasher.update(value.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn decode_glob_cursor(
    cursor: Option<&str>,
    query_digest: &str,
) -> Result<Option<String>, ToolError> {
    let Some(cursor) = cursor.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let payload = cursor
        .strip_prefix("glob-v1:")
        .ok_or_else(|| ToolError::Message("invalid glob cursor version".to_string()))?;
    let (cursor_digest, encoded_cursor) = payload
        .split_once(':')
        .ok_or_else(|| ToolError::Message("invalid glob cursor payload".to_string()))?;
    if cursor_digest != query_digest {
        return Err(ToolError::Message(
            "glob cursor query does not match the requested pattern".to_string(),
        ));
    }
    let walk_cursor = URL_SAFE_NO_PAD
        .decode(encoded_cursor)
        .map_err(|_| ToolError::Message("invalid glob cursor payload".to_string()))?;
    String::from_utf8(walk_cursor)
        .map(Some)
        .map_err(|_| ToolError::Message("glob cursor payload is not UTF-8".to_string()))
}

fn encode_glob_cursor(cursor: &str, query_digest: &str) -> Result<String, ToolError> {
    if cursor.trim().is_empty() {
        return Err(ToolError::Message(
            "glob cursor cannot contain an empty traversal cursor".to_string(),
        ));
    }
    Ok(format!(
        "glob-v1:{query_digest}:{}",
        URL_SAFE_NO_PAD.encode(cursor.as_bytes())
    ))
}

fn decode_grep_cursor(
    cursor: Option<&str>,
    query_digest: &str,
) -> Result<(Option<String>, usize), ToolError> {
    let Some(cursor) = cursor.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok((None, 0));
    };
    let payload = cursor
        .strip_prefix("grep-v3:")
        .ok_or_else(|| ToolError::Message("invalid grep cursor version".to_string()))?;
    let (cursor_digest, payload) = payload
        .split_once(':')
        .ok_or_else(|| ToolError::Message("invalid grep cursor payload".to_string()))?;
    if cursor_digest != query_digest {
        return Err(ToolError::Message(
            "grep cursor query does not match the requested pattern or options".to_string(),
        ));
    }
    let (encoded_cursor, line_offset) = payload
        .rsplit_once(':')
        .ok_or_else(|| ToolError::Message("invalid grep cursor payload".to_string()))?;
    let line_offset = line_offset
        .parse::<usize>()
        .map_err(|_| ToolError::Message("invalid grep cursor line offset".to_string()))?;
    let walk_cursor = URL_SAFE_NO_PAD
        .decode(encoded_cursor)
        .map_err(|_| ToolError::Message("invalid grep cursor payload".to_string()))?;
    let walk_cursor = String::from_utf8(walk_cursor)
        .map_err(|_| ToolError::Message("grep cursor payload is not UTF-8".to_string()))?;
    Ok((Some(walk_cursor), line_offset))
}

fn encode_grep_cursor(
    cursor: &str,
    line_offset: usize,
    query_digest: &str,
) -> Result<String, ToolError> {
    if cursor.trim().is_empty() {
        return Err(ToolError::Message(
            "grep cursor cannot contain an empty traversal cursor".to_string(),
        ));
    }
    Ok(format!(
        "grep-v3:{query_digest}:{}:{line_offset}",
        URL_SAFE_NO_PAD.encode(cursor.as_bytes())
    ))
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
    requested
        .unwrap_or(configured_max.max(1))
        .max(1)
        .min(configured_max.max(1))
}

fn discovery_candidate_limit(result_limit: usize) -> usize {
    result_limit
        .saturating_mul(8)
        .max(MIN_DISCOVERY_VISITS)
        .min(MAX_DISCOVERY_CANDIDATES)
}

fn discovery_visit_limit(candidate_limit: usize) -> usize {
    candidate_limit
        .saturating_mul(8)
        .max(MIN_DISCOVERY_VISITS)
        .min(MAX_DISCOVERY_CANDIDATES)
}

fn require_directory(path: &Utf8Path) -> Result<(), ToolError> {
    if !path.exists() {
        return Err(ToolError::Message(format!("path `{path}` does not exist")));
    }
    if !path.is_dir() {
        return Err(ToolError::Message(format!(
            "path `{path}` is not a directory"
        )));
    }
    Ok(())
}

struct RenderedEntryPage {
    output_text: String,
    rendered_count: usize,
    continuation: Option<String>,
}

fn render_entries_bounded(
    entries: &[TraversalEntry],
    max_lines: usize,
    max_bytes: usize,
    mut render: impl FnMut(&TraversalEntry) -> String,
) -> RenderedEntryPage {
    let max_lines = max_lines.max(1);
    let max_bytes = max_bytes.max(1);
    let mut lines = Vec::new();
    let mut bytes = 0usize;
    let mut continuation = None;
    for entry in entries {
        let mut line = render(entry);
        if line.len() > max_bytes {
            line = clip_text_with_ellipsis(&line, max_bytes);
        }
        let separator = usize::from(!lines.is_empty());
        if lines.len() >= max_lines
            || bytes.saturating_add(separator).saturating_add(line.len()) > max_bytes
        {
            continuation = Some(entry.cursor.clone());
            break;
        }
        bytes = bytes.saturating_add(separator).saturating_add(line.len());
        lines.push(line);
    }
    RenderedEntryPage {
        output_text: lines.join("\n"),
        rendered_count: lines.len(),
        continuation,
    }
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
            ToolError::Message("grep candidate has no supported text encoding".to_string())
        })
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
        bounded_result_limit, compile_include_glob, decode_glob_cursor, decode_grep_cursor,
        encode_glob_cursor, encode_grep_cursor, read_grep_candidate, search_query_digest,
    };

    #[test]
    fn invalid_include_glob_returns_typed_tool_error() {
        let error = compile_include_glob("[").expect_err("invalid glob must be rejected");
        assert!(error.to_string().contains("invalid include_glob"));
    }

    #[test]
    fn requested_result_limit_is_positive_and_bounded() {
        assert_eq!(bounded_result_limit(Some(usize::MAX), 64), 64);
        assert_eq!(bounded_result_limit(Some(12), 64), 12);
        assert_eq!(bounded_result_limit(None, 64), 64);
        assert_eq!(bounded_result_limit(Some(0), 64), 1);
    }

    #[test]
    fn grep_cursor_round_trips_the_walker_and_line_positions() {
        let digest = search_query_digest(
            "grep-v3",
            &["needle", "case-sensitive", "no-include-glob", ""],
        );
        let cursor = encode_grep_cursor("walk-v2:Zm9v", 9, &digest).expect("encode cursor");
        assert_eq!(
            decode_grep_cursor(Some(&cursor), &digest).expect("decode cursor"),
            (Some("walk-v2:Zm9v".to_string()), 9)
        );
    }

    #[test]
    fn search_cursors_reject_a_different_semantic_query() {
        let first_glob = search_query_digest("glob-v1", &["src/**/*.rs"]);
        let other_glob = search_query_digest("glob-v1", &["tests/**/*.rs"]);
        let cursor = encode_glob_cursor("walk-v2:Zm9v", &first_glob).expect("glob cursor");
        assert!(decode_glob_cursor(Some(&cursor), &other_glob).is_err());

        let first_grep = search_query_digest(
            "grep-v3",
            &["needle", "case-sensitive", "include-glob", "*.rs"],
        );
        let other_grep = search_query_digest(
            "grep-v3",
            &["needle", "case-insensitive", "include-glob", "*.rs"],
        );
        let cursor = encode_grep_cursor("walk-v2:Zm9v", 4, &first_grep).expect("grep cursor");
        assert!(decode_grep_cursor(Some(&cursor), &other_grep).is_err());
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
    fn grep_skip_details_are_bounded() {
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
        assert_eq!(skipped.omitted_count(), 25);
    }
}
