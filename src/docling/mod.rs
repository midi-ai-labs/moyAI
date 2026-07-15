use std::collections::BTreeMap;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use reqwest::multipart::{Form, Part};
use serde_json::{Value, json};

use crate::config::DoclingConfig;
use crate::error::ToolError;
use crate::tool::truncate::clip_text_with_ellipsis;

#[derive(Debug, Clone)]
pub struct DoclingConvertRequest {
    pub path: Option<Utf8PathBuf>,
    pub source_url: Option<String>,
    pub from_formats: Vec<String>,
    pub to_formats: Vec<String>,
    pub do_ocr: Option<bool>,
    pub include_images: Option<bool>,
    pub page_range: Option<(u32, u32)>,
}

#[derive(Debug, Clone)]
pub struct DoclingConvertResult {
    pub endpoint: String,
    pub filename: Option<String>,
    pub status: String,
    pub processing_time_secs: f64,
    pub output_formats: Vec<String>,
    pub outputs: BTreeMap<String, String>,
    pub error_messages: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DoclingClient {
    config: DoclingConfig,
    http: reqwest::Client,
}

impl DoclingClient {
    pub fn new(config: DoclingConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub fn config(&self) -> &DoclingConfig {
        &self.config
    }

    pub async fn convert(
        &self,
        request: DoclingConvertRequest,
        mut effect_checkpoint: impl FnMut() -> Result<(), ToolError>,
    ) -> Result<DoclingConvertResult, ToolError> {
        if !self.config.enabled {
            return Err(ToolError::Message(
                "docling is disabled by config".to_string(),
            ));
        }

        match (
            request.path.as_ref(),
            request
                .source_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty()),
        ) {
            (Some(path), None) => {
                self.convert_file(path, &request, &mut effect_checkpoint)
                    .await
            }
            (None, Some(source_url)) => {
                self.convert_source(source_url, &request, &mut effect_checkpoint)
                    .await
            }
            (Some(_), Some(_)) => Err(ToolError::Message(
                "docling_convert accepts exactly one of `path` or `source_url`".to_string(),
            )),
            (None, None) => Err(ToolError::Message(
                "docling_convert requires either `path` or `source_url`".to_string(),
            )),
        }
    }

    async fn convert_file(
        &self,
        path: &Utf8Path,
        request: &DoclingConvertRequest,
        effect_checkpoint: &mut impl FnMut() -> Result<(), ToolError>,
    ) -> Result<DoclingConvertResult, ToolError> {
        let endpoint = endpoint(&self.config.base_url, "/v1/convert/file");
        let bytes = tokio::fs::read(path.as_std_path())
            .await
            .map_err(|error| ToolError::Message(format!("failed to read `{path}`: {error}")))?;
        let file_name = path
            .file_name()
            .map(str::to_string)
            .unwrap_or_else(|| "document".to_string());
        let part = Part::bytes(bytes)
            .file_name(file_name)
            .mime_str(mime_for_path(path))
            .map_err(|error| ToolError::Message(format!("failed to prepare upload: {error}")))?;

        let mut form = Form::new().part("files", part);
        form = append_convert_form_fields(form, request);

        let request = self
            .request_builder(&endpoint, reqwest::Method::POST)?
            .multipart(form);
        effect_checkpoint()?;
        let body = request
            .send()
            .await
            .map_err(|error| ToolError::Message(format!("docling request failed: {error}")))?;
        parse_convert_response(&endpoint, body).await
    }

    async fn convert_source(
        &self,
        source_url: &str,
        request: &DoclingConvertRequest,
        effect_checkpoint: &mut impl FnMut() -> Result<(), ToolError>,
    ) -> Result<DoclingConvertResult, ToolError> {
        let endpoint = endpoint(&self.config.base_url, "/v1/convert/source");
        let mut options = serde_json::Map::new();
        if !request.from_formats.is_empty() {
            options.insert(
                "from_formats".to_string(),
                Value::Array(
                    request
                        .from_formats
                        .iter()
                        .map(|value| Value::String(value.clone()))
                        .collect(),
                ),
            );
        }
        if !request.to_formats.is_empty() {
            options.insert(
                "to_formats".to_string(),
                Value::Array(
                    request
                        .to_formats
                        .iter()
                        .map(|value| Value::String(value.clone()))
                        .collect(),
                ),
            );
        }
        if let Some(value) = request.do_ocr {
            options.insert("do_ocr".to_string(), Value::Bool(value));
        }
        if let Some(value) = request.include_images {
            options.insert("include_images".to_string(), Value::Bool(value));
        }
        if let Some((start, end)) = request.page_range {
            options.insert("page_range".to_string(), json!([start, end]));
        }

        let documents = json!({
            "sources": [{ "kind": "http", "url": source_url }],
            "target": { "kind": "inbody" },
            "options": Value::Object(options),
        });

        let request = self
            .request_builder(&endpoint, reqwest::Method::POST)?
            .header("Content-Type", "application/json")
            .body(documents.to_string());
        effect_checkpoint()?;
        let body = request
            .send()
            .await
            .map_err(|error| ToolError::Message(format!("docling request failed: {error}")))?;
        parse_convert_response(&endpoint, body).await
    }

    fn request_builder(
        &self,
        endpoint: &str,
        method: reqwest::Method,
    ) -> Result<reqwest::RequestBuilder, ToolError> {
        let mut request = self
            .http
            .request(method, endpoint)
            .timeout(Duration::from_millis(self.config.timeout_ms))
            .header("Accept", "application/json");
        if let Some(api_key) = self
            .config
            .api_key_env
            .as_deref()
            .and_then(|key| std::env::var(key).ok())
            .filter(|value| !value.is_empty())
        {
            request = request.header("X-Api-Key", api_key);
        }
        for (name, value) in &self.config.headers {
            request = request.header(name, value);
        }
        Ok(request)
    }
}

pub fn normalize_docling_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

fn append_convert_form_fields(mut form: Form, request: &DoclingConvertRequest) -> Form {
    for value in &request.from_formats {
        form = form.text("from_formats", value.clone());
    }
    for value in &request.to_formats {
        form = form.text("to_formats", value.clone());
    }
    if let Some(value) = request.do_ocr {
        form = form.text("do_ocr", value.to_string());
    }
    if let Some(value) = request.include_images {
        form = form.text("include_images", value.to_string());
    }
    if let Some((start, end)) = request.page_range {
        form = form
            .text("page_range", start.to_string())
            .text("page_range", end.to_string());
    }
    form
}

async fn parse_convert_response(
    endpoint: &str,
    response: reqwest::Response,
) -> Result<DoclingConvertResult, ToolError> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| ToolError::Message(format!("failed to read docling response: {error}")))?;
    if !status.is_success() {
        return Err(ToolError::Message(format!(
            "docling request to `{endpoint}` failed with HTTP {}: {}",
            status.as_u16(),
            compact_body(&body)
        )));
    }

    let response: Value = serde_json::from_str(&body).map_err(|error| {
        ToolError::Message(format!(
            "failed to parse docling response body: {error}: {}",
            compact_body(&body)
        ))
    })?;
    let document = response
        .get("document")
        .and_then(Value::as_object)
        .ok_or_else(|| ToolError::Message("docling response is missing `document`".to_string()))?;
    let filename = document
        .get("filename")
        .and_then(Value::as_str)
        .map(str::to_string);
    let status_text = response
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let processing_time_secs = response
        .get("processing_time")
        .and_then(Value::as_f64)
        .unwrap_or_default();

    let mut outputs = BTreeMap::new();
    for (format_name, field_name) in [
        ("md", "md_content"),
        ("json", "json_content"),
        ("html", "html_content"),
        ("text", "text_content"),
        ("doctags", "doctags_content"),
    ] {
        if let Some(value) = document.get(field_name).and_then(value_to_text) {
            outputs.insert(format_name.to_string(), value);
        }
    }

    if outputs.is_empty() {
        outputs.insert(
            "document".to_string(),
            serde_json::to_string_pretty(&response).unwrap_or_else(|_| response.to_string()),
        );
    }

    let error_messages = response
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .map(error_item_to_text)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(DoclingConvertResult {
        endpoint: endpoint.to_string(),
        filename,
        status: status_text,
        processing_time_secs,
        output_formats: outputs.keys().cloned().collect(),
        outputs,
        error_messages,
    })
}

fn endpoint(base_url: &str, suffix: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), suffix)
}

fn value_to_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => serde_json::to_string_pretty(other).ok(),
    }
}

fn error_item_to_text(value: &Value) -> String {
    if let Some(message) = value.get("message").and_then(Value::as_str) {
        return message.to_string();
    }
    if let Some(detail) = value.get("detail").and_then(Value::as_str) {
        return detail.to_string();
    }
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

fn mime_for_path(path: &Utf8Path) -> &'static str {
    match path
        .extension()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "csv" => "text/csv",
        "htm" | "html" => "text/html",
        "json" => "application/json",
        "md" => "text/markdown",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
}

fn compact_body(body: &str) -> String {
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() <= 240 {
        compact
    } else {
        clip_text_with_ellipsis(&compact, 243)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::any;

    use super::*;

    #[tokio::test]
    async fn typed_effect_admission_is_checked_at_docling_send_boundary() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind Docling fixture");
        let address = listener.local_addr().expect("fixture address");
        let request_count = Arc::new(AtomicUsize::new(0));
        let handler_count = Arc::clone(&request_count);
        let app = Router::new().fallback(any(move || {
            let handler_count = Arc::clone(&handler_count);
            async move {
                handler_count.fetch_add(1, Ordering::SeqCst);
                StatusCode::OK
            }
        }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve Docling fixture");
        });

        let client = DoclingClient::new(DoclingConfig {
            enabled: true,
            base_url: format!("http://{address}"),
            timeout_ms: 2_000,
            api_key_env: None,
            headers: BTreeMap::new(),
        });
        let error = client
            .convert(
                DoclingConvertRequest {
                    path: None,
                    source_url: Some("https://example.test/document.pdf".to_string()),
                    from_formats: Vec::new(),
                    to_formats: vec!["md".to_string()],
                    do_ocr: None,
                    include_images: Some(false),
                    page_range: None,
                },
                || Err(ToolError::RunInterrupted),
            )
            .await
            .expect_err("the typed terminal owner must reject the network send");

        assert!(matches!(error, ToolError::RunInterrupted));
        assert_eq!(request_count.load(Ordering::SeqCst), 0);
        server.abort();
    }
}
