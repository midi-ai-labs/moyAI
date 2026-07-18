use std::io::Read as _;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use camino::{Utf8Path, Utf8PathBuf};
use globset::{Glob, GlobSetBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::config::model::FileGuardConfig;
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::internal_output::ResolvedSearchPath;
use crate::tool::registry::Tool;
use crate::tool::truncate::clip_text_with_ellipsis;
use crate::tool::{ToolName, ToolResult, ToolSpec};
use crate::workspace::traversal::{
    TraversalEntry, TraversalOptions, TraversalRegistry, walk_guarded_page,
};
use crate::workspace::{AccessKind, GuardedPath, PathGuard, Workspace};

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

enum GrepSearchRoot {
    Normal(GuardedPath),
    Internal(Utf8PathBuf),
}

impl GrepSearchRoot {
    fn absolute(&self) -> &Utf8Path {
        match self {
            Self::Normal(guarded) => &guarded.absolute,
            Self::Internal(absolute) => absolute,
        }
    }

    fn normal_guard(&self) -> Option<&GuardedPath> {
        match self {
            Self::Normal(guarded) => Some(guarded),
            Self::Internal(_) => None,
        }
    }
}

const GREP_BINARY_SAMPLE_BYTES: u64 = 8 * 1024;
const GREP_MAX_SKIP_DETAILS: usize = 64;
const GREP_MAX_SKIP_DETAIL_BYTES: usize = 16 * 1024;
const MAX_DISCOVERY_CANDIDATES: usize = 4_096;
const MIN_DISCOVERY_VISITS: usize = 128;
const GREP_CURSOR_PREFIX: &str = "grep-v5:";
const GREP_CURSOR_REGISTRY_DOMAIN: &str = "grep-v5";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepCursorPayload {
    query_digest: String,
    position: GrepCursorPosition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum GrepCursorPosition {
    InFile {
        traversal_cursor: String,
        candidate_path: Utf8PathBuf,
        line_offset: usize,
        content_sha256: [u8; 32],
    },
    BetweenFiles {
        traversal_cursor: String,
    },
}

impl GrepCursorPosition {
    fn traversal_cursor(&self) -> &str {
        match self {
            Self::InFile {
                traversal_cursor, ..
            }
            | Self::BetweenFiles { traversal_cursor } => traversal_cursor,
        }
    }

    fn in_file_fence(&self) -> Option<GrepInFileFence<'_>> {
        match self {
            Self::InFile {
                candidate_path,
                line_offset,
                content_sha256,
                ..
            } => Some(GrepInFileFence {
                candidate_path,
                line_offset: *line_offset,
                content_sha256,
            }),
            Self::BetweenFiles { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct GrepInFileFence<'a> {
    candidate_path: &'a Utf8Path,
    line_offset: usize,
    content_sha256: &'a [u8; 32],
}

#[derive(Debug, PartialEq, Eq)]
struct GrepCandidateText {
    text: String,
    content_sha256: [u8; 32],
}

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
        let limit = bounded_result_limit(input.limit, ctx.config.tool_output.max_results);
        let page = walk_guarded_page(
            &guarded,
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
        let page = walk_guarded_page(
            &guarded,
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
                let projected_workspace_relative =
                    glob_workspace_relative_path(&guarded, &entry.relative_path);
                glob_matches_path(
                    &matcher,
                    &entry.path,
                    &entry.relative_path,
                    &ctx.workspace.root,
                    projected_workspace_relative.as_deref(),
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
            |entry| {
                let projected_workspace_relative =
                    glob_workspace_relative_path(&guarded, &entry.relative_path);
                glob_output_label(
                    &entry.path,
                    &ctx.workspace.root,
                    projected_workspace_relative.as_deref(),
                )
            },
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
    relative_to_search_root: &Utf8Path,
    workspace_root: &Utf8Path,
    projected_workspace_relative: Option<&Utf8Path>,
) -> bool {
    matcher.is_match(path.as_str())
        || matcher.is_match(relative_to_search_root.as_str())
        || path
            .strip_prefix(workspace_root)
            .ok()
            .is_some_and(|relative| matcher.is_match(relative.as_str()))
        || projected_workspace_relative.is_some_and(|relative| matcher.is_match(relative.as_str()))
}

fn glob_workspace_relative_path(
    guarded_root: &GuardedPath,
    relative_to_search_root: &Utf8Path,
) -> Option<Utf8PathBuf> {
    guarded_root
        .inside_workspace
        .then(|| guarded_root.relative_to_root.join(relative_to_search_root))
}

fn glob_output_label(
    path: &Utf8Path,
    workspace_root: &Utf8Path,
    projected_workspace_relative: Option<&Utf8Path>,
) -> String {
    let label = if let Ok(relative) = path.strip_prefix(workspace_root) {
        relative
    } else if let Some(relative) = projected_workspace_relative {
        relative
    } else {
        path
    };
    label.as_str().replace('\\', "/")
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
        let resolved =
            crate::tool::internal_output::resolve_path(&ctx, &requested, AccessKind::Search)
                .await?;
        let permission = resolved.permission();
        ctx.confirm_if_needed(
            AccessKind::Search,
            format!("Grep {}", permission.absolute),
            vec![permission.absolute.to_path_buf()],
            !permission.inside_workspace && !permission.trusted_external,
            Vec::new(),
        )
        .await?
        .admit()?;

        let (search_root, mut internal_file) = match resolved.into_search_path() {
            ResolvedSearchPath::Normal(guarded) => {
                PathGuard::revalidate(&guarded)?;
                (GrepSearchRoot::Normal(guarded), None)
            }
            ResolvedSearchPath::Internal(opened) => {
                let (absolute, file) = opened.into_parts();
                (GrepSearchRoot::Internal(absolute), Some(file))
            }
        };

        let case_sensitive = input.case_sensitive.unwrap_or(false);
        let query_digest = search_query_digest(
            "grep-v5",
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
        let cursor_position = decode_grep_cursor(
            input.cursor.as_deref(),
            &query_digest,
            &ctx.workspace.traversal_registry,
        )?;
        let walk_cursor = cursor_position
            .as_ref()
            .map(GrepCursorPosition::traversal_cursor);
        let limit = bounded_result_limit(input.limit, ctx.config.tool_output.max_results)
            .min(ctx.config.tool_output.max_lines.max(1));
        let candidate_limit = discovery_candidate_limit(limit);

        let page = if internal_file.is_some() || search_root.absolute().is_file() {
            single_file_page(search_root.absolute(), walk_cursor)?
        } else {
            let guarded = search_root.normal_guard().ok_or_else(|| {
                ToolError::Message(
                    "internal grep paths must resolve to an authorized held file".to_string(),
                )
            })?;
            walk_guarded_page(
                guarded,
                ctx.workspace,
                walk_cursor,
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
        let in_file_fence = cursor_position
            .as_ref()
            .and_then(GrepCursorPosition::in_file_fence);
        if in_file_fence.is_some() && page.entries.is_empty() {
            return Err(ToolError::Message(
                "grep in-file continuation candidate is no longer available; restart the search"
                    .to_string(),
            ));
        }

        let mut matches = Vec::new();
        let mut matches_bytes = 0usize;
        let mut skipped = GrepSkipSummary::default();
        let mut continuation = None;
        let mut scanned_files = 0usize;
        let output_byte_limit = ctx.config.tool_output.max_bytes.max(1);

        'files: for (file_index, entry) in page.entries.iter().enumerate() {
            let resumed_in_file = (file_index == 0).then_some(in_file_fence).flatten();
            if let Some(fence) = resumed_in_file {
                validate_grep_resume_candidate(&entry.path, fence)?;
            }
            let included = include_glob
                .as_ref()
                .map(|glob| glob.is_match(entry.path.as_str()))
                .unwrap_or(true);
            if !included {
                if resumed_in_file.is_some() {
                    return Err(ToolError::Message(format!(
                        "grep in-file continuation candidate `{}` no longer matches its query; restart the search",
                        entry.path
                    )));
                }
                continue;
            }
            scanned_files = scanned_files.saturating_add(1);
            let opened_internal_file = if file_index == 0 {
                internal_file.as_mut()
            } else {
                None
            };
            match read_grep_candidate(
                ctx.workspace,
                search_root.normal_guard(),
                &entry.path,
                &ctx.config.file_guard,
                opened_internal_file,
            )? {
                Ok(candidate) => {
                    let line_offset = if let Some(fence) = resumed_in_file {
                        validate_grep_content_fence(&entry.path, &candidate, fence)?;
                        fence.line_offset
                    } else {
                        0
                    };
                    for (line_index, line) in candidate.text.lines().enumerate().skip(line_offset) {
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
                                GrepCursorPosition::InFile {
                                    traversal_cursor: entry.cursor.clone(),
                                    candidate_path: entry.path.clone(),
                                    line_offset: line_index,
                                    content_sha256: candidate.content_sha256,
                                },
                                &query_digest,
                                &ctx.workspace.traversal_registry,
                            )?);
                            break 'files;
                        }
                        matches_bytes = matches_bytes
                            .saturating_add(separator)
                            .saturating_add(rendered.len());
                        matches.push(rendered);
                        if matches.len() >= limit {
                            continuation = Some(encode_grep_cursor(
                                GrepCursorPosition::InFile {
                                    traversal_cursor: entry.cursor.clone(),
                                    candidate_path: entry.path.clone(),
                                    line_offset: line_index.saturating_add(1),
                                    content_sha256: candidate.content_sha256,
                                },
                                &query_digest,
                                &ctx.workspace.traversal_registry,
                            )?);
                            break 'files;
                        }
                    }
                }
                Err(reason) => {
                    if resumed_in_file.is_some() {
                        return Err(ToolError::Message(format!(
                            "grep in-file continuation candidate `{}` is no longer searchable ({}); restart the search",
                            entry.path,
                            reason.as_str()
                        )));
                    }
                    skipped.record(entry.path.clone(), reason);
                }
            }
        }
        if continuation.is_none() {
            continuation = page
                .continuation
                .as_deref()
                .map(|cursor| {
                    encode_grep_cursor(
                        GrepCursorPosition::BetweenFiles {
                            traversal_cursor: cursor.to_string(),
                        },
                        &query_digest,
                        &ctx.workspace.traversal_registry,
                    )
                })
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
    const SINGLE_FILE_CURSOR: &str = "single-file-v2";
    if cursor.is_some_and(|cursor| cursor != SINGLE_FILE_CURSOR) {
        return Err(ToolError::Message(
            "grep cursor does not identify the requested file".to_string(),
        ));
    }
    let entries = if cursor.is_none() || cursor == Some(SINGLE_FILE_CURSOR) {
        vec![TraversalEntry {
            path: path.to_path_buf(),
            relative_path: Utf8PathBuf::from(path.file_name().unwrap_or(path.as_str())),
            is_directory: false,
            size_bytes: None,
            depth: 1,
            cursor: SINGLE_FILE_CURSOR.to_string(),
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
    registry: &TraversalRegistry,
) -> Result<Option<GrepCursorPosition>, ToolError> {
    let Some(cursor) = cursor.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let token = cursor
        .strip_prefix(GREP_CURSOR_PREFIX)
        .ok_or_else(|| ToolError::Message("invalid grep cursor version".to_string()))?;
    if token.trim().is_empty() {
        return Err(ToolError::Message(
            "invalid empty grep cursor token".to_string(),
        ));
    }
    let bytes = registry.consume_one_shot_continuation(GREP_CURSOR_REGISTRY_DOMAIN, token)?;
    let payload = serde_json::from_slice::<GrepCursorPayload>(&bytes)
        .map_err(|_| ToolError::Message("invalid grep cursor payload".to_string()))?;
    if payload.query_digest != query_digest {
        return Err(ToolError::Message(
            "grep cursor query does not match the requested pattern or options".to_string(),
        ));
    }
    validate_grep_cursor_position(&payload.position)?;
    Ok(Some(payload.position))
}

fn encode_grep_cursor(
    position: GrepCursorPosition,
    query_digest: &str,
    registry: &TraversalRegistry,
) -> Result<String, ToolError> {
    validate_grep_cursor_position(&position)?;
    let payload = GrepCursorPayload {
        query_digest: query_digest.to_string(),
        position,
    };
    let bytes = serde_json::to_vec(&payload)
        .map_err(|error| ToolError::Message(format!("failed to encode grep cursor: {error}")))?;
    let token = registry.register_one_shot_continuation(GREP_CURSOR_REGISTRY_DOMAIN, bytes)?;
    Ok(format!("{GREP_CURSOR_PREFIX}{token}"))
}

fn validate_grep_cursor_position(position: &GrepCursorPosition) -> Result<(), ToolError> {
    if position.traversal_cursor().trim().is_empty() {
        return Err(ToolError::Message(
            "grep cursor cannot contain an empty traversal cursor".to_string(),
        ));
    }
    if let GrepCursorPosition::InFile { candidate_path, .. } = position
        && candidate_path.as_str().trim().is_empty()
    {
        return Err(ToolError::Message(
            "grep in-file cursor cannot contain an empty candidate path".to_string(),
        ));
    }
    Ok(())
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

fn validate_grep_content_fence(
    path: &Utf8Path,
    candidate: &GrepCandidateText,
    fence: GrepInFileFence<'_>,
) -> Result<(), ToolError> {
    if &candidate.content_sha256 != fence.content_sha256 {
        return Err(ToolError::Message(format!(
            "grep continuation content changed for `{path}`; restart the search to avoid omitted or duplicated matches"
        )));
    }
    if fence.line_offset > candidate.text.lines().count() {
        return Err(ToolError::Message(format!(
            "grep continuation line offset is outside `{path}`; restart the search"
        )));
    }
    Ok(())
}

fn validate_grep_resume_candidate(
    resumed_path: &Utf8Path,
    fence: GrepInFileFence<'_>,
) -> Result<(), ToolError> {
    if !PathGuard::same_existing_namespace_entry(resumed_path, fence.candidate_path)? {
        return Err(ToolError::Message(format!(
            "grep in-file continuation resumed at `{resumed_path}` instead of `{}`; restart the search",
            fence.candidate_path
        )));
    }
    Ok(())
}

fn read_grep_candidate(
    workspace: &Workspace,
    search_root: Option<&GuardedPath>,
    path: &Utf8Path,
    guard: &FileGuardConfig,
    opened_internal_file: Option<&mut std::fs::File>,
) -> Result<Result<GrepCandidateText, GrepSkipReason>, ToolError> {
    if let Some(file) = opened_internal_file {
        return read_grep_candidate_from_file(path, file, guard);
    }
    let search_root = search_root.ok_or_else(|| {
        ToolError::Message(
            "internal grep candidate lost its authorized opened-file handle".to_string(),
        )
    })?;
    let candidate = PathGuard::require_descendant(workspace, search_root, path)?;
    let mut file = PathGuard::open_validated_read_file(&candidate)?;
    read_grep_candidate_from_file(path, &mut file, guard)
}

fn read_grep_candidate_from_file(
    path: &Utf8Path,
    file: &mut std::fs::File,
    guard: &FileGuardConfig,
) -> Result<Result<GrepCandidateText, GrepSkipReason>, ToolError> {
    let metadata = file.metadata()?;
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
    let content_sha256 = Sha256::digest(&bytes).into();
    crate::tool::text_encoding::decode_text(bytes)
        .map(|decoded| {
            Ok(GrepCandidateText {
                text: decoded.text,
                content_sha256,
            })
        })
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
    use std::fs::{self, FileTimes, OpenOptions};

    use camino::Utf8PathBuf;
    use sha2::{Digest, Sha256};

    use super::{
        GREP_CURSOR_PREFIX, GREP_MAX_SKIP_DETAIL_BYTES, GREP_MAX_SKIP_DETAILS, GrepCursorPosition,
        GrepSkipReason, GrepSkipSummary, bounded_result_limit, compile_include_glob,
        decode_glob_cursor, decode_grep_cursor, encode_glob_cursor, encode_grep_cursor,
        glob_matches_path, glob_output_label, glob_workspace_relative_path, read_grep_candidate,
        search_query_digest, single_file_page, validate_grep_content_fence,
        validate_grep_resume_candidate,
    };
    use crate::config::ResolvedConfig;
    use crate::workspace::traversal::{TraversalOptions, TraversalRegistry, walk_page};
    use crate::workspace::{AccessKind, PathGuard, WorkspaceDiscovery};

    #[cfg(unix)]
    fn link_directory(target: &camino::Utf8Path, link: &camino::Utf8Path) {
        std::os::unix::fs::symlink(target, link).expect("create directory symlink");
    }

    #[cfg(windows)]
    fn link_directory(target: &camino::Utf8Path, link: &camino::Utf8Path) {
        std::os::windows::fs::symlink_dir(target, link).expect("create directory symlink");
    }

    #[cfg(unix)]
    fn link_file(target: &camino::Utf8Path, link: &camino::Utf8Path) {
        std::os::unix::fs::symlink(target, link).expect("create file symlink");
    }

    #[cfg(windows)]
    fn link_file(target: &camino::Utf8Path, link: &camino::Utf8Path) {
        std::os::windows::fs::symlink_file(target, link).expect("create file symlink");
    }

    #[cfg(windows)]
    fn enable_case_sensitive_directory(path: &camino::Utf8Path) {
        use std::os::windows::fs::OpenOptionsExt as _;
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_CASE_SENSITIVE_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FileCaseSensitiveInfo,
            SetFileInformationByHandle,
        };
        use windows_sys::Win32::System::SystemServices::FILE_CS_FLAG_CASE_SENSITIVE_DIR;

        let mut options = std::fs::OpenOptions::new();
        options
            .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
        let directory = options.open(path).expect("open case-sensitive directory");
        let info = FILE_CASE_SENSITIVE_INFO {
            Flags: FILE_CS_FLAG_CASE_SENSITIVE_DIR,
        };
        let result = unsafe {
            SetFileInformationByHandle(
                directory.as_raw_handle() as HANDLE,
                FileCaseSensitiveInfo,
                (&info as *const FILE_CASE_SENSITIVE_INFO).cast(),
                std::mem::size_of::<FILE_CASE_SENSITIVE_INFO>() as u32,
            )
        };
        assert_ne!(
            result,
            0,
            "enable per-directory case sensitivity: {}",
            std::io::Error::last_os_error()
        );
    }

    fn rewrite_same_size_preserving_modified_time(path: &camino::Utf8Path, replacement: &[u8]) {
        let before = fs::metadata(path).expect("metadata before rewrite");
        let modified = before.modified().expect("modified time before rewrite");
        assert_eq!(before.len(), replacement.len() as u64);

        fs::write(path, replacement).expect("rewrite fixture in place");
        OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open rewritten fixture")
            .set_times(FileTimes::new().set_modified(modified))
            .expect("restore modified time");

        let after = fs::metadata(path).expect("metadata after rewrite");
        assert_eq!(after.len(), before.len());
        assert_eq!(after.modified().expect("restored modified time"), modified);
    }

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
        let registry = TraversalRegistry::default();
        let digest = search_query_digest(
            "grep-v5",
            &["needle", "case-sensitive", "no-include-glob", ""],
        );
        let position = GrepCursorPosition::InFile {
            traversal_cursor: "walk-v3:Zm9v".to_string(),
            candidate_path: Utf8PathBuf::from("src/lib.rs"),
            line_offset: 9,
            content_sha256: [7; 32],
        };
        let cursor =
            encode_grep_cursor(position.clone(), &digest, &registry).expect("encode cursor");
        assert_eq!(
            decode_grep_cursor(Some(&cursor), &digest, &registry).expect("decode cursor"),
            Some(position)
        );
        assert!(
            decode_grep_cursor(Some(&cursor), &digest, &registry).is_err(),
            "opaque grep cursors are consumed exactly once"
        );
    }

    #[test]
    fn search_cursors_reject_a_different_semantic_query() {
        let registry = TraversalRegistry::default();
        let first_glob = search_query_digest("glob-v1", &["src/**/*.rs"]);
        let other_glob = search_query_digest("glob-v1", &["tests/**/*.rs"]);
        let cursor = encode_glob_cursor("walk-v2:Zm9v", &first_glob).expect("glob cursor");
        assert!(decode_glob_cursor(Some(&cursor), &other_glob).is_err());

        let first_grep = search_query_digest(
            "grep-v5",
            &["needle", "case-sensitive", "include-glob", "*.rs"],
        );
        let other_grep = search_query_digest(
            "grep-v5",
            &["needle", "case-insensitive", "include-glob", "*.rs"],
        );
        let cursor = encode_grep_cursor(
            GrepCursorPosition::BetweenFiles {
                traversal_cursor: "walk-v3:Zm9v".to_string(),
            },
            &first_grep,
            &registry,
        )
        .expect("grep cursor");
        assert!(decode_grep_cursor(Some(&cursor), &other_grep, &registry).is_err());
    }

    #[test]
    fn grep_candidates_apply_file_guards_before_full_text_search() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");
        let mut guard = crate::config::ResolvedConfig::default().file_guard;
        guard.max_inline_read_bytes = 8;
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let search_root = PathGuard::require_path(&workspace, &root, AccessKind::Search)
            .expect("guard search root");

        let blocked = root.join("weights.bin");
        fs::write(&blocked, b"text").expect("write blocked fixture");
        assert_eq!(
            read_grep_candidate(&workspace, Some(&search_root), &blocked, &guard, None)
                .expect("guard result"),
            Err(GrepSkipReason::BlockedExtension)
        );

        let large = root.join("large.txt");
        fs::write(&large, b"0123456789").expect("write large fixture");
        assert_eq!(
            read_grep_candidate(&workspace, Some(&search_root), &large, &guard, None)
                .expect("guard result"),
            Err(GrepSkipReason::LargeFile)
        );

        let binary = root.join("binary.txt");
        fs::write(&binary, [0_u8, 1, 2]).expect("write binary fixture");
        assert_eq!(
            read_grep_candidate(&workspace, Some(&search_root), &binary, &guard, None)
                .expect("guard result"),
            Err(GrepSkipReason::BinaryContent)
        );

        let text = root.join("source.txt");
        fs::write(&text, b"needle").expect("write text fixture");
        assert_eq!(
            read_grep_candidate(&workspace, Some(&search_root), &text, &guard, None)
                .expect("guard result"),
            Ok(super::GrepCandidateText {
                text: "needle".to_string(),
                content_sha256: Sha256::digest(b"needle").into(),
            })
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn grep_rejects_an_enumerated_candidate_swapped_to_an_external_link() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        let external =
            Utf8PathBuf::from_path_buf(temp.path().join("external")).expect("utf8 external");
        let slot = root.join("slot");
        fs::create_dir_all(&slot).expect("workspace slot");
        fs::create_dir_all(&external).expect("external root");
        fs::write(slot.join("candidate.txt"), "inside").expect("inside fixture");
        fs::write(external.join("candidate.txt"), "outside secret").expect("outside fixture");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let search_root = PathGuard::require_path(&workspace, &root, AccessKind::Search)
            .expect("guard search root");
        let page = walk_page(
            &root,
            &workspace,
            None,
            TraversalOptions {
                include_hidden: false,
                max_depth: None,
                include_files: true,
                include_directories: false,
                result_limit: 8,
                visit_limit: 32,
            },
        )
        .expect("enumerate candidate");
        let candidate = page.entries.first().expect("candidate").path.clone();

        fs::remove_file(slot.join("candidate.txt")).expect("remove original candidate");
        fs::remove_dir(&slot).expect("remove original slot");
        link_directory(&external, &slot);

        let guard = ResolvedConfig::default().file_guard;
        let result = read_grep_candidate(&workspace, Some(&search_root), &candidate, &guard, None);

        assert!(
            result.is_err(),
            "a path enumerated inside the workspace must not read through a later external link"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn grep_rejects_a_single_file_swapped_to_an_external_link() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        let external =
            Utf8PathBuf::from_path_buf(temp.path().join("external.txt")).expect("utf8 external");
        fs::create_dir_all(&root).expect("workspace root");
        let candidate = root.join("candidate.txt");
        fs::write(&candidate, "inside").expect("inside fixture");
        fs::write(&external, "outside secret").expect("outside fixture");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let search_root = PathGuard::require_path(&workspace, &candidate, AccessKind::Search)
            .expect("guard single file");

        fs::remove_file(&candidate).expect("remove original candidate");
        link_file(&external, &candidate);

        let guard = ResolvedConfig::default().file_guard;
        let result = read_grep_candidate(&workspace, Some(&search_root), &candidate, &guard, None);

        assert!(
            result.is_err(),
            "a single-file search target must not read through a later external link"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn grep_reads_an_internal_candidate_from_its_authorized_held_handle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        let truncation_dir =
            Utf8PathBuf::from_path_buf(temp.path().join("truncated")).expect("utf8 truncation");
        fs::create_dir_all(&root).expect("workspace root");
        fs::create_dir_all(&truncation_dir).expect("truncation root");
        let candidate = truncation_dir.join("owned.txt");
        let redirected = truncation_dir.join("other-session.txt");
        fs::write(&candidate, "owned needle").expect("owned fixture");
        fs::write(&redirected, "other-session needle").expect("redirect fixture");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let search_root = PathGuard::trusted_internal_path(&candidate, &truncation_dir)
            .expect("guard internal candidate");
        let mut opened =
            PathGuard::open_validated_read_file(&search_root).expect("authorized held handle");

        fs::remove_file(&candidate).expect("replace candidate after authorization");
        link_file(&redirected, &candidate);
        let result = read_grep_candidate(
            &workspace,
            None,
            &candidate,
            &ResolvedConfig::default().file_guard,
            Some(&mut opened),
        )
        .expect("held candidate read")
        .expect("held candidate remains searchable");

        assert_eq!(result.text, "owned needle");
    }

    #[test]
    fn directory_grep_in_file_cursor_rejects_same_size_same_mtime_content_rewrite() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let candidate_path = root.join("candidate.txt");
        fs::write(&candidate_path, b"needle one\nneedle two\n").expect("seed candidate");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let search_root = PathGuard::require_path(&workspace, &root, AccessKind::Search)
            .expect("guard search root");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: false,
            result_limit: 8,
            visit_limit: 32,
        };
        let first = walk_page(&root, &workspace, None, options).expect("first page");
        let entry = first.entries.first().expect("candidate entry");
        let original = read_grep_candidate(
            &workspace,
            Some(&search_root),
            &entry.path,
            &config.file_guard,
            None,
        )
        .expect("read candidate")
        .expect("searchable candidate");
        let query_digest = search_query_digest(
            "grep-v5",
            &["needle", "case-sensitive", "no-include-glob", ""],
        );
        let cursor = encode_grep_cursor(
            GrepCursorPosition::InFile {
                traversal_cursor: entry.cursor.clone(),
                candidate_path: entry.path.clone(),
                line_offset: 1,
                content_sha256: original.content_sha256,
            },
            &query_digest,
            &workspace.traversal_registry,
        )
        .expect("in-file cursor");

        rewrite_same_size_preserving_modified_time(&candidate_path, b"needle uno\nneedle dos\n");

        let position =
            decode_grep_cursor(Some(&cursor), &query_digest, &workspace.traversal_registry)
                .expect("decode cursor")
                .expect("cursor position");
        let walk_cursor = position.traversal_cursor().to_string();
        let resumed = walk_page(&root, &workspace, Some(&walk_cursor), options)
            .expect("directory metadata alone does not detect an in-place rewrite");
        let resumed_entry = resumed.entries.first().expect("resumed candidate");
        let current = read_grep_candidate(
            &workspace,
            Some(&search_root),
            &resumed_entry.path,
            &config.file_guard,
            None,
        )
        .expect("read rewritten candidate")
        .expect("rewritten candidate remains text");

        let error = validate_grep_content_fence(
            &resumed_entry.path,
            &current,
            position.in_file_fence().expect("in-file fence"),
        )
        .expect_err("rewritten bytes must invalidate the old line offset");
        assert!(error.to_string().contains("content changed"));

        let stale = walk_page(&root, &workspace, Some(&walk_cursor), options)
            .expect_err("the underlying traversal cursor remains one-shot");
        assert!(stale.to_string().contains("expired"));
    }

    #[test]
    fn single_file_grep_in_file_cursor_rejects_same_size_same_mtime_content_rewrite() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let candidate_path = root.join("candidate.txt");
        fs::write(&candidate_path, b"needle one\nneedle two\n").expect("seed candidate");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let search_root = PathGuard::require_path(&workspace, &candidate_path, AccessKind::Search)
            .expect("guard single file");
        let first = single_file_page(&candidate_path, None).expect("single-file page");
        let entry = first.entries.first().expect("candidate entry");
        let original = read_grep_candidate(
            &workspace,
            Some(&search_root),
            &entry.path,
            &config.file_guard,
            None,
        )
        .expect("read candidate")
        .expect("searchable candidate");
        let query_digest = search_query_digest(
            "grep-v5",
            &["needle", "case-sensitive", "no-include-glob", ""],
        );
        let cursor = encode_grep_cursor(
            GrepCursorPosition::InFile {
                traversal_cursor: entry.cursor.clone(),
                candidate_path: entry.path.clone(),
                line_offset: 1,
                content_sha256: original.content_sha256,
            },
            &query_digest,
            &workspace.traversal_registry,
        )
        .expect("in-file cursor");

        rewrite_same_size_preserving_modified_time(&candidate_path, b"needle uno\nneedle dos\n");

        let position =
            decode_grep_cursor(Some(&cursor), &query_digest, &workspace.traversal_registry)
                .expect("decode cursor")
                .expect("cursor position");
        let resumed = single_file_page(&candidate_path, Some(position.traversal_cursor()))
            .expect("size and mtime metadata still match the single-file cursor");
        let resumed_entry = resumed.entries.first().expect("resumed candidate");
        let current = read_grep_candidate(
            &workspace,
            Some(&search_root),
            &resumed_entry.path,
            &config.file_guard,
            None,
        )
        .expect("read rewritten candidate")
        .expect("rewritten candidate remains text");

        let error = validate_grep_content_fence(
            &resumed_entry.path,
            &current,
            position.in_file_fence().expect("in-file fence"),
        )
        .expect_err("rewritten bytes must invalidate the old line offset");
        assert!(error.to_string().contains("content changed"));
    }

    #[cfg(windows)]
    #[test]
    fn single_file_grep_cursor_accepts_the_same_windows_path_with_different_case() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let candidate_path = root.join("CaseFile.txt");
        fs::write(&candidate_path, b"needle one\nneedle two\n").expect("seed candidate");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
        let first = single_file_page(&candidate_path, None).expect("single-file page");
        let entry = first.entries.first().expect("candidate entry");
        let query_digest = search_query_digest(
            "grep-v5",
            &["needle", "case-sensitive", "no-include-glob", ""],
        );
        let cursor = encode_grep_cursor(
            GrepCursorPosition::InFile {
                traversal_cursor: entry.cursor.clone(),
                candidate_path: entry.path.clone(),
                line_offset: 1,
                content_sha256: Sha256::digest(b"needle one\nneedle two\n").into(),
            },
            &query_digest,
            &workspace.traversal_registry,
        )
        .expect("in-file cursor");

        let position =
            decode_grep_cursor(Some(&cursor), &query_digest, &workspace.traversal_registry)
                .expect("decode cursor")
                .expect("cursor position");
        let case_variant = Utf8PathBuf::from(candidate_path.as_str().to_ascii_uppercase());
        let resumed = single_file_page(&case_variant, Some(position.traversal_cursor()))
            .expect("same Windows file with case-variant spelling");
        super::validate_grep_resume_candidate(
            &resumed.entries[0].path,
            position.in_file_fence().expect("in-file fence"),
        )
        .expect("case variant preserves candidate identity");
    }

    #[cfg(windows)]
    #[test]
    fn grep_resume_rejects_case_distinct_entries_with_identical_content() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent =
            Utf8PathBuf::from_path_buf(temp.path().join("case-parent")).expect("utf8 parent");
        fs::create_dir(&parent).expect("case parent");
        enable_case_sensitive_directory(&parent);
        let upper = parent.join("A.txt");
        let lower = parent.join("a.txt");
        fs::write(&upper, b"same content\n").expect("upper fixture");
        fs::write(&lower, b"same content\n").expect("lower fixture");
        let digest: [u8; 32] = Sha256::digest(b"same content\n").into();

        let error = validate_grep_resume_candidate(
            &lower,
            super::GrepInFileFence {
                candidate_path: &upper,
                line_offset: 1,
                content_sha256: &digest,
            },
        )
        .expect_err("case-distinct namespace entries must not share a continuation");

        assert!(error.to_string().contains("instead of"));
    }

    #[cfg(windows)]
    #[test]
    fn grep_resume_accepts_unicode_case_alias_in_case_insensitive_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent =
            Utf8PathBuf::from_path_buf(temp.path().join("UnicodeParent")).expect("utf8 parent");
        fs::create_dir(&parent).expect("parent");
        let canonical = parent.join("Ünicode.txt");
        fs::write(&canonical, b"same content\n").expect("fixture");
        let alias = parent.join("ünicode.TXT");
        let digest: [u8; 32] = Sha256::digest(b"same content\n").into();

        validate_grep_resume_candidate(
            &alias,
            super::GrepInFileFence {
                candidate_path: &canonical,
                line_offset: 1,
                content_sha256: &digest,
            },
        )
        .expect("Unicode case alias identifies the same namespace entry");
    }

    #[cfg(windows)]
    #[test]
    fn glob_resume_matches_root_relative_pattern_through_unicode_case_alias() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 workspace root");
        let search_root = workspace_root.join("ÜnicodeRoot");
        let nested = search_root.join("nested");
        fs::create_dir_all(&nested).expect("nested search root");
        fs::write(nested.join("a.txt"), "first").expect("first fixture");
        fs::write(nested.join("b.txt"), "second").expect("second fixture");
        let workspace =
            WorkspaceDiscovery::discover_fixed_root(&workspace_root, &ResolvedConfig::default())
                .expect("workspace");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: false,
            result_limit: 1,
            visit_limit: 32,
        };
        let first_root_alias = Utf8PathBuf::from(search_root.as_str().to_ascii_uppercase());
        let first =
            walk_page(&first_root_alias, &workspace, None, options).expect("first glob page");
        assert_eq!(
            first.entries[0].relative_path,
            Utf8PathBuf::from("nested").join("a.txt")
        );
        let cursor = first.continuation.expect("glob traversal continuation");

        let alias_root = workspace_root.join("ünicodeRoot");
        let second = walk_page(&alias_root, &workspace, Some(&cursor), options)
            .expect("resume glob through Unicode case alias");
        assert_eq!(
            second.entries[0].relative_path,
            Utf8PathBuf::from("nested").join("b.txt")
        );
        assert!(
            second.entries[0].path.starts_with(&first_root_alias),
            "the walker preserves the snapshot root spelling"
        );
        assert!(
            second.entries[0]
                .path
                .strip_prefix(&workspace.root)
                .is_err(),
            "the snapshot spelling must exercise the Windows workspace-prefix alias"
        );
        let guarded_alias = PathGuard::require_path(&workspace, &alias_root, AccessKind::Search)
            .expect("guard resumed alias root");
        let projected_workspace_relative =
            glob_workspace_relative_path(&guarded_alias, &second.entries[0].relative_path)
                .expect("workspace-relative projection");
        let matcher = compile_include_glob("nested/*.txt").expect("root-relative glob");
        assert!(glob_matches_path(
            &matcher,
            &second.entries[0].path,
            &second.entries[0].relative_path,
            &workspace.root,
            Some(&projected_workspace_relative),
        ));
        let workspace_pattern = format!(
            "{}/nested/*.txt",
            guarded_alias.relative_to_root.as_str().replace('\\', "/")
        );
        let workspace_matcher = compile_include_glob(&workspace_pattern)
            .expect("workspace-relative glob through alias");
        assert!(glob_matches_path(
            &workspace_matcher,
            &second.entries[0].path,
            &second.entries[0].relative_path,
            &workspace.root,
            Some(&projected_workspace_relative),
        ));
        assert_eq!(
            glob_output_label(
                &second.entries[0].path,
                &workspace.root,
                Some(&projected_workspace_relative),
            ),
            projected_workspace_relative.as_str().replace('\\', "/")
        );
    }

    #[test]
    fn glob_output_label_prefers_lexical_workspace_path_and_preserves_external_path() {
        let workspace_root = Utf8PathBuf::from("workspace");
        let lexical_workspace_path = workspace_root.join("alias").join("file.rs");
        let projected_workspace_path = Utf8PathBuf::from("canonical").join("file.rs");
        assert_eq!(
            glob_output_label(
                &lexical_workspace_path,
                &workspace_root,
                Some(&projected_workspace_path),
            ),
            "alias/file.rs",
            "a lexical workspace alias must remain the user-visible spelling"
        );

        let external_path = Utf8PathBuf::from("/external/file.rs");
        assert_eq!(
            glob_output_label(&external_path, &workspace_root, None),
            "/external/file.rs",
            "an external root must not be projected into the workspace namespace"
        );
    }

    #[test]
    fn grep_cursor_is_opaque_tamper_resistant_and_one_shot() {
        let registry = TraversalRegistry::default();
        let query_digest = search_query_digest(
            "grep-v5",
            &["needle", "case-sensitive", "no-include-glob", ""],
        );
        let cursor = encode_grep_cursor(
            GrepCursorPosition::InFile {
                traversal_cursor: "walk-v3:fixture".to_string(),
                candidate_path: Utf8PathBuf::from("candidate.txt"),
                line_offset: 1,
                content_sha256: [5; 32],
            },
            &query_digest,
            &registry,
        )
        .expect("typed cursor");
        let token = cursor
            .strip_prefix(GREP_CURSOR_PREFIX)
            .expect("cursor prefix");
        assert_eq!(token.len(), 26, "only the random ULID token is exposed");
        let tampered = format!("{cursor}x");

        let tampered_error = decode_grep_cursor(Some(&tampered), &query_digest, &registry)
            .expect_err("an unregistered token must fail closed");
        assert!(tampered_error.to_string().contains("continuation token"));
        decode_grep_cursor(Some(&cursor), &query_digest, &registry)
            .expect("tampering with another token does not consume the issued token");
        assert!(
            decode_grep_cursor(Some(&cursor), &query_digest, &registry).is_err(),
            "the issued token cannot be replayed"
        );
        assert!(
            decode_grep_cursor(Some("grep-v4:retired"), &query_digest, &registry).is_err(),
            "malformed typed cursors must fail closed"
        );
    }

    #[test]
    fn between_file_grep_cursor_preserves_root_and_one_shot_traversal_contracts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let other_root = root.join("other");
        fs::create_dir_all(&other_root).expect("other root");
        fs::write(root.join("a.txt"), "needle a").expect("first fixture");
        fs::write(root.join("b.txt"), "needle b").expect("second fixture");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let options = TraversalOptions {
            include_hidden: false,
            max_depth: None,
            include_files: true,
            include_directories: false,
            result_limit: 1,
            visit_limit: 16,
        };
        let first = walk_page(&root, &workspace, None, options).expect("first page");
        assert_eq!(first.entries[0].relative_path.as_str(), "a.txt");
        let traversal_cursor = first.continuation.expect("between-file continuation");
        let query_digest = search_query_digest(
            "grep-v5",
            &["needle", "case-sensitive", "no-include-glob", ""],
        );
        let cursor = encode_grep_cursor(
            GrepCursorPosition::BetweenFiles { traversal_cursor },
            &query_digest,
            &workspace.traversal_registry,
        )
        .expect("between-file cursor");
        let position =
            decode_grep_cursor(Some(&cursor), &query_digest, &workspace.traversal_registry)
                .expect("decode cursor")
                .expect("cursor position");
        assert!(position.in_file_fence().is_none());

        let wrong_root = walk_page(
            &other_root,
            &workspace,
            Some(position.traversal_cursor()),
            options,
        )
        .expect_err("wrapper must preserve traversal root ownership");
        assert!(wrong_root.to_string().contains("different root"));

        let second = walk_page(
            &root,
            &workspace,
            Some(position.traversal_cursor()),
            options,
        )
        .expect("resume next candidate");
        assert_eq!(second.entries[0].relative_path.as_str(), "b.txt");
        let stale = walk_page(
            &root,
            &workspace,
            Some(position.traversal_cursor()),
            options,
        )
        .expect_err("between-file traversal cursor remains one-shot");
        assert!(stale.to_string().contains("expired"));
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
