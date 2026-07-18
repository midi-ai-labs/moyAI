use async_trait::async_trait;
use camino::Utf8PathBuf;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;

use crate::docling::{DoclingConvertRequest, DoclingConvertResult, DoclingLocalInput};
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
            description: "Convert a local document file or source URL through the configured Docling Serve API.",
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

        let (local_input, effect_admission) = match (
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
                let file = PathGuard::open_validated_read_file(&guarded)?;
                (
                    Some(DoclingLocalInput::from_validated_handle(
                        guarded.absolute,
                        file,
                    )),
                    effect_admission,
                )
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
                    local_input,
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

        let preview = ctx.services.truncator.preview_chunks(
            convert_output_chunks(&result),
            "\n",
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
            _internal_file_lease: preview.internal_file_lease,
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

fn convert_output_chunks(result: &DoclingConvertResult) -> impl Iterator<Item = String> + '_ {
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
    lines
        .into_iter()
        .chain(result.outputs.iter().flat_map(|(format_name, content)| {
            [
                format!("[{format_name}]"),
                sanitize_docling_output_content(content),
            ]
        }))
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
