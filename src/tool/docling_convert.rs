use std::fs;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use regex::Regex;
use serde::Deserialize;
use serde_json::json;

use crate::docling::{DoclingConvertRequest, DoclingConvertResult};
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::Tool;
use crate::tool::{PermissionRisk, ToolName, ToolResult, ToolSpec};
use crate::workspace::{AccessKind, PathGuard};

const ALLOWED_FROM_FORMATS: &[&str] = &[
    "docx",
    "pptx",
    "html",
    "image",
    "pdf",
    "asciidoc",
    "md",
    "csv",
    "xlsx",
    "xml_uspto",
    "xml_jats",
    "xml_xbrl",
    "mets_gbs",
    "json_docling",
    "audio",
    "vtt",
    "latex",
];

const ALLOWED_TO_FORMATS: &[&str] = &[
    "md",
    "json",
    "yaml",
    "html",
    "html_split_page",
    "text",
    "doctags",
    "vtt",
];

#[derive(Debug, Deserialize)]
pub struct DoclingConvertInput {
    pub path: Option<Utf8PathBuf>,
    pub source_url: Option<String>,
    pub from_formats: Option<Vec<String>>,
    pub to_formats: Option<Vec<String>>,
    pub do_ocr: Option<bool>,
    pub include_images: Option<bool>,
    pub page_range: Option<Vec<u32>>,
}

#[derive(Debug, Default)]
pub struct DoclingConvertTool;

#[async_trait(?Send)]
impl Tool for DoclingConvertTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::DoclingConvert,
            effect: crate::tool::ToolEffectPolicy::read(),
            description: "Convert a local document file or source URL through the configured Docling Serve API. Prefer this for PDF, DOCX, XLSX, and PPTX when `read` blocks structured documents.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "source_url": { "type": "string" },
                    "from_formats": { "type": "array", "items": { "type": "string" } },
                    "to_formats": { "type": "array", "items": { "type": "string" } },
                    "do_ocr": { "type": "boolean" },
                    "include_images": { "type": "boolean" },
                    "page_range": {
                        "type": "array",
                        "items": { "type": "integer" },
                        "minItems": 2,
                        "maxItems": 2
                    }
                }
            }),
        }
    }

    async fn execute(
        &self,
        raw_arguments: serde_json::Value,
        mut ctx: ToolContext<'_>,
    ) -> Result<ToolResult, ToolError> {
        let input = serde_json::from_value::<DoclingConvertInput>(raw_arguments)?;
        let from_formats = normalize_formats(
            input.from_formats.unwrap_or_default(),
            ALLOWED_FROM_FORMATS,
            "from_formats",
        )?;
        let to_formats = {
            let normalized = normalize_formats(
                input.to_formats.unwrap_or_else(|| vec!["md".to_string()]),
                ALLOWED_TO_FORMATS,
                "to_formats",
            )?;
            if normalized.is_empty() {
                vec!["md".to_string()]
            } else {
                normalized
            }
        };
        let page_range = parse_page_range(input.page_range)?;

        let (path, effect_admission) = match (
            input.path,
            input
                .source_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty()),
        ) {
            (Some(path), None) => {
                let guarded = PathGuard::require_path(ctx.workspace, &path, AccessKind::Read)?;
                let effect_admission = ctx
                    .confirm_if_needed(
                        AccessKind::Read,
                        format!("Upload {} to Docling Serve", guarded.absolute),
                        vec![guarded.absolute.clone()],
                        !guarded.inside_workspace && !guarded.trusted_external,
                        vec![PermissionRisk::ConfiguredLocalService],
                    )
                    .await?;
                if !guarded.absolute.exists() {
                    return Ok(missing_input_result(
                        &guarded.absolute,
                        ctx.workspace.root.as_path(),
                    ));
                }
                if guarded.absolute.is_dir() {
                    return Ok(directory_input_result(&guarded.absolute));
                }
                (Some(guarded.absolute), effect_admission)
            }
            (None, Some(source_url)) => {
                let effect_admission = ctx
                    .confirm_if_needed(
                        AccessKind::Read,
                        format!("Fetch {} through Docling Serve", source_url),
                        Vec::new(),
                        false,
                        vec![PermissionRisk::Network],
                    )
                    .await?;
                (None, effect_admission)
            }
            (Some(_), Some(_)) => {
                return Err(ToolError::Message(
                    "docling_convert accepts exactly one of `path` or `source_url`".to_string(),
                ));
            }
            (None, None) => {
                return Err(ToolError::Message(
                    "docling_convert requires either `path` or `source_url`".to_string(),
                ));
            }
        };

        let result = crate::docling::DoclingClient::new(ctx.config.docling.clone())
            .convert(
                DoclingConvertRequest {
                    path,
                    source_url: input.source_url,
                    from_formats,
                    to_formats,
                    do_ocr: input.do_ocr,
                    include_images: input.include_images.or(Some(false)),
                    page_range,
                },
                || effect_admission.admit(),
            )
            .await?;

        let output_text = render_convert_output(&result);
        let preview = ctx.services.truncator.preview(
            output_text,
            &ctx.config.tool_output,
            &ctx.services.storage_paths,
        )?;

        Ok(ToolResult {
            title: format!(
                "Docling converted {}",
                result
                    .filename
                    .clone()
                    .unwrap_or_else(|| "document".to_string())
            ),
            output_text: preview.preview_text,
            metadata: json!({
                "endpoint": result.endpoint,
                "filename": result.filename,
                "status": result.status,
                "processing_time_secs": result.processing_time_secs,
                "output_formats": result.output_formats,
                "error_messages": result.error_messages,
                "output_bytes": result.outputs.iter().map(|(format, content)| {
                    json!({ "format": format, "bytes": content.len() })
                }).collect::<Vec<_>>(),
                "truncated": preview.truncated,
            }),
            truncated_output_path: preview.truncated_output_path,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }
}

fn normalize_formats(
    values: Vec<String>,
    allowed: &[&str],
    field_name: &str,
) -> Result<Vec<String>, ToolError> {
    let mut normalized = Vec::new();
    for value in values {
        let lowered = normalize_format_alias(value.trim().to_ascii_lowercase(), allowed);
        if lowered.is_empty() {
            continue;
        }
        if !allowed.iter().any(|allowed| *allowed == lowered) {
            return Err(ToolError::Message(format!(
                "unsupported {field_name} value `{lowered}`"
            )));
        }
        if !normalized.iter().any(|existing| existing == &lowered) {
            normalized.push(lowered);
        }
    }
    Ok(normalized)
}

fn normalize_format_alias(value: String, allowed: &[&str]) -> String {
    let canonical = match value.as_str() {
        "markdown" => Some("md"),
        "txt" | "plain_text" | "plaintext" => Some("text"),
        "htm" => Some("html"),
        "yml" => Some("yaml"),
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "tif" | "tiff" | "webp" => Some("image"),
        _ => None,
    };
    canonical
        .filter(|candidate| allowed.iter().any(|allowed| allowed == candidate))
        .map(str::to_string)
        .unwrap_or(value)
}

fn parse_page_range(value: Option<Vec<u32>>) -> Result<Option<(u32, u32)>, ToolError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.len() != 2 {
        return Err(ToolError::Message(
            "page_range must contain exactly two integers".to_string(),
        ));
    }
    let start = value[0];
    let end = value[1];
    if start == 0 || end == 0 || end < start {
        return Err(ToolError::Message(
            "page_range must be a 1-based inclusive range with end >= start".to_string(),
        ));
    }
    Ok(Some((start, end)))
}

fn missing_input_result(path: &Utf8Path, workspace_root: &Utf8Path) -> ToolResult {
    let suggestions = suggest_existing_docling_inputs(path, workspace_root);
    let suggestion_line = if suggestions.is_empty() {
        "If you do not know the exact filename, inspect the directory with `list`, `glob`, or `inspect_directory` first and then reuse the exact path.".to_string()
    } else {
        format!(
            "Closest existing matches: {}. Reuse one of those exact paths, or inspect the directory first if you still need confirmation.",
            suggestions
                .iter()
                .map(|value| format!("`{value}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    corrective_result(
        "Docling input missing",
        &format!(
            "`{path}` does not exist. Use an exact existing file path before calling `docling_convert`. {suggestion_line}"
        ),
        json!({
            "corrective_result": true,
            "blocked_reason": "missing_path",
            "requested_path": path,
            "suggested_paths": suggestions,
        }),
    )
}

fn directory_input_result(path: &Utf8Path) -> ToolResult {
    corrective_result(
        "Docling input requires a file",
        &format!(
            "`{path}` is a directory. Use `inspect_directory` or `list` to pick one concrete file path, then call `docling_convert` on that file."
        ),
        json!({
            "corrective_result": true,
            "blocked_reason": "directory",
            "requested_path": path,
            "suggested_tools": ["inspect_directory", "list"],
        }),
    )
}

fn suggest_existing_docling_inputs(path: &Utf8Path, workspace_root: &Utf8Path) -> Vec<String> {
    let search_root = path
        .parent()
        .filter(|parent| parent.exists())
        .unwrap_or(workspace_root);
    let requested_name = path.file_name().unwrap_or(path.as_str());
    let requested_extension = normalized_extension(path);
    let requested_canonical = canonical_filename_for_match(requested_name);
    let mut suggestions = fs::read_dir(search_root.as_std_path())
        .ok()
        .into_iter()
        .flat_map(|entries| entries.flatten())
        .filter_map(|entry| {
            let entry_path = Utf8PathBuf::from_path_buf(entry.path()).ok()?;
            let file_type = entry.file_type().ok()?;
            if !file_type.is_file() {
                return None;
            }
            if !requested_extension.is_empty()
                && normalized_extension(entry_path.as_path()) != requested_extension
            {
                return None;
            }
            let candidate_name = entry_path
                .file_name()
                .unwrap_or(entry_path.as_str())
                .to_string();
            let candidate_canonical = canonical_filename_for_match(&candidate_name);
            let canonical_match =
                !requested_canonical.is_empty() && candidate_canonical == requested_canonical;
            let score = shared_prefix_len(&requested_canonical, &candidate_canonical);
            let rendered = entry_path
                .strip_prefix(workspace_root)
                .unwrap_or(entry_path.as_path())
                .as_str()
                .replace('\\', "/");
            Some((canonical_match, score, rendered))
        })
        .collect::<Vec<_>>();
    suggestions.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then(right.1.cmp(&left.1))
            .then(left.2.cmp(&right.2))
    });
    suggestions
        .into_iter()
        .map(|(_, _, path)| path)
        .take(5)
        .collect()
}

fn normalized_extension(path: &Utf8Path) -> String {
    path.extension()
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default()
}

fn canonical_filename_for_match(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn shared_prefix_len(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .count()
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

fn render_convert_output(result: &DoclingConvertResult) -> String {
    let mut lines = vec![
        format!("Docling status: {}", result.status),
        format!("Endpoint: {}", result.endpoint),
    ];
    if let Some(filename) = &result.filename {
        lines.push(format!("Filename: {filename}"));
    }
    lines.push(format!(
        "Processing time: {:.3}s",
        result.processing_time_secs
    ));
    if !result.error_messages.is_empty() {
        lines.push(format!("Errors: {}", result.error_messages.join(" | ")));
    }
    lines.push(String::new());

    for (format_name, content) in &result.outputs {
        lines.push(format!("[{format_name}]"));
        lines.push(sanitize_docling_output_content(content));
        lines.push(String::new());
    }

    lines.join("\n").trim().to_string()
}

fn sanitize_docling_output_content(content: &str) -> String {
    let data_uri_regex = Regex::new(r#"(?is)data:image/[^;)\s]+;base64,[A-Za-z0-9+/=\s]+"#)
        .expect("data-uri regex should compile");
    let markdown_image_regex = Regex::new(r#"!\[[^\]]*]\(\[inline image data omitted]\)"#)
        .expect("markdown image regex should compile");

    let sanitized = data_uri_regex.replace_all(content, "[inline image data omitted]");
    markdown_image_regex
        .replace_all(&sanitized, "[inline image omitted]")
        .into_owned()
}
