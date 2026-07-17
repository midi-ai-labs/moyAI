use std::fmt;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::config::{ProviderEndpoint, ProviderMetadataMode, ResolvedConfig};
use crate::error::LlmError;

const PROVIDER_READINESS_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const PROVIDER_METADATA_RESPONSE_LIMIT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct OpenAiModelsResponse {
    #[serde(default)]
    data: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModelInfo {
    pub id: String,
    pub display_name: Option<String>,
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub supports_images: Option<bool>,
    pub supports_tools: Option<bool>,
    pub supports_reasoning: Option<bool>,
    pub max_parallel_predictions: Option<u32>,
    pub load_state: ProviderModelLoadState,
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderModelLoadState {
    Loaded,
    NotLoaded,
    Unknown,
}

impl Default for ProviderModelLoadState {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailabilityStatus {
    Pass,
    Fail,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAvailabilityReport {
    pub gate: String,
    pub status: ModelAvailabilityStatus,
    pub generated_by: String,
    pub model: String,
    pub base_url: String,
    pub provider_metadata_mode: ProviderMetadataMode,
    pub v1_present: bool,
    pub native_present: bool,
    pub require_vision: bool,
    pub vision_capable: bool,
    pub tool_use_capable: Option<bool>,
    pub reasoning_capable: Option<bool>,
    pub context: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub max_parallel_predictions: Option<u32>,
    #[serde(default)]
    pub load_state: ProviderModelLoadState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness_detail: Option<String>,
    pub matched_model: Option<ProviderModelInfo>,
    pub v1_models: Vec<String>,
    pub native_models: Vec<String>,
    pub openai_error: Option<String>,
    pub native_error: Option<String>,
    pub checked_at_ms: u64,
}

impl fmt::Debug for ModelAvailabilityReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelAvailabilityReport")
            .field("gate", &self.gate)
            .field("status", &self.status)
            .field("generated_by", &self.generated_by)
            .field("model", &self.model)
            .field(
                "base_url",
                &ProviderEndpoint::parse(&self.base_url)
                    .map(|endpoint| endpoint.catalog_root().as_str().to_string())
                    .unwrap_or_else(|_| "<invalid-provider-endpoint>".to_string()),
            )
            .field("provider_metadata_mode", &self.provider_metadata_mode)
            .field("v1_present", &self.v1_present)
            .field("native_present", &self.native_present)
            .field("require_vision", &self.require_vision)
            .field("vision_capable", &self.vision_capable)
            .field("tool_use_capable", &self.tool_use_capable)
            .field("reasoning_capable", &self.reasoning_capable)
            .field("context", &self.context)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("max_parallel_predictions", &self.max_parallel_predictions)
            .field("load_state", &self.load_state)
            .field("readiness_detail", &self.readiness_detail)
            .field("matched_model", &self.matched_model)
            .field("v1_models", &self.v1_models)
            .field("native_models", &self.native_models)
            .field("openai_error_present", &self.openai_error.is_some())
            .field("native_error_present", &self.native_error.is_some())
            .field("checked_at_ms", &self.checked_at_ms)
            .finish()
    }
}

pub fn normalize_provider_base_url(input: &str) -> String {
    ProviderEndpoint::parse(input)
        .map(|endpoint| endpoint.catalog_root().as_str().to_string())
        .unwrap_or_default()
}

pub async fn fetch_openai_models(
    config: &ResolvedConfig,
    base_url_input: &str,
) -> Result<Vec<String>, LlmError> {
    let infos = fetch_provider_model_infos(config, base_url_input).await?;
    Ok(infos.into_iter().map(|model| model.id).collect())
}

pub async fn fetch_provider_model_infos(
    config: &ResolvedConfig,
    base_url_input: &str,
) -> Result<Vec<ProviderModelInfo>, LlmError> {
    let base_url = ProviderEndpoint::parse(base_url_input)
        .map_err(|error| LlmError::Message(error.to_string()))?
        .catalog_root();

    let client = build_probe_client(config)?;
    let headers = build_probe_headers(config)?;

    let mut models = match config.model.provider_metadata_mode {
        ProviderMetadataMode::LmStudioNativeRequired => {
            fetch_lmstudio_model_infos(&client, &base_url, headers).await?
        }
        ProviderMetadataMode::OpenAiCompatibleOnly => {
            fetch_openai_model_infos(&client, &base_url, headers).await?
        }
    };
    if models.is_empty() {
        return Err(LlmError::Message(
            "configured provider metadata endpoint returned an empty model list".to_string(),
        ));
    }
    models.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(models)
}

pub async fn check_model_availability(
    config: &ResolvedConfig,
    model_override: Option<&str>,
    base_url_override: Option<&str>,
    require_vision: bool,
) -> ModelAvailabilityReport {
    let model = model_override
        .unwrap_or(&config.model.model)
        .trim()
        .to_string();
    let endpoint = ProviderEndpoint::parse(base_url_override.unwrap_or(&config.model.base_url))
        .map(|endpoint| endpoint.catalog_root());
    let base_url = endpoint
        .as_ref()
        .map(|endpoint| endpoint.as_str().to_string())
        .unwrap_or_else(|_| "<invalid-provider-endpoint>".to_string());
    let checked_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);

    let mut report = ModelAvailabilityReport {
        gate: "model_availability".to_string(),
        status: ModelAvailabilityStatus::Fail,
        generated_by: "moyai_model_availability_v4_catalog_and_load_state".to_string(),
        model,
        base_url,
        provider_metadata_mode: config.model.provider_metadata_mode,
        v1_present: false,
        native_present: false,
        require_vision,
        vision_capable: false,
        tool_use_capable: None,
        reasoning_capable: None,
        context: None,
        max_output_tokens: None,
        max_parallel_predictions: None,
        load_state: ProviderModelLoadState::Unknown,
        readiness_detail: None,
        matched_model: None,
        v1_models: Vec::new(),
        native_models: Vec::new(),
        openai_error: None,
        native_error: None,
        checked_at_ms,
    };

    if report.model.is_empty() {
        report.openai_error = Some("configured model is empty".to_string());
        report.native_error = Some("configured model is empty".to_string());
        return report;
    }
    let endpoint = match endpoint {
        Ok(endpoint) => endpoint,
        Err(error) => {
            let message = error.to_string();
            report.openai_error = Some(message.clone());
            report.native_error = Some(message);
            return report;
        }
    };
    if report.base_url.is_empty() {
        report.openai_error = Some("provider endpoint must not be empty".to_string());
        report.native_error = Some("provider endpoint must not be empty".to_string());
        return report;
    }

    let client = match build_probe_client(config) {
        Ok(client) => client,
        Err(error) => {
            let message = error.to_string();
            report.openai_error = Some(message.clone());
            report.native_error = Some(message);
            return report;
        }
    };
    let headers = match build_probe_headers(config) {
        Ok(headers) => headers,
        Err(error) => {
            let message = error.to_string();
            report.openai_error = Some(message.clone());
            report.native_error = Some(message);
            return report;
        }
    };

    let (openai_models, native_models) = match report.provider_metadata_mode {
        ProviderMetadataMode::OpenAiCompatibleOnly => {
            let models = match fetch_openai_model_infos(&client, &endpoint, headers).await {
                Ok(models) => models,
                Err(error) => {
                    report.openai_error = Some(error.to_string());
                    Vec::new()
                }
            };
            (models, Vec::new())
        }
        ProviderMetadataMode::LmStudioNativeRequired => {
            let models = match fetch_lmstudio_model_infos(&client, &endpoint, headers).await {
                Ok(models) => models,
                Err(error) => {
                    report.native_error = Some(error.to_string());
                    Vec::new()
                }
            };
            (Vec::new(), models)
        }
    };

    report.v1_present = openai_models.iter().any(|entry| entry.id == report.model);
    report.native_present = native_models.iter().any(|entry| entry.id == report.model);
    report.v1_models = openai_models
        .iter()
        .map(|entry| entry.id.clone())
        .collect::<Vec<_>>();
    report.native_models = native_models
        .iter()
        .map(|entry| entry.id.clone())
        .collect::<Vec<_>>();

    let matched_model = openai_models
        .iter()
        .find(|entry| entry.id == report.model)
        .cloned()
        .or_else(|| {
            native_models
                .iter()
                .find(|entry| entry.id == report.model)
                .cloned()
        });
    if let Some(model) = matched_model.as_ref() {
        report.vision_capable = model.supports_images.unwrap_or(false);
        report.tool_use_capable = model.supports_tools;
        report.reasoning_capable = model.supports_reasoning;
        report.context = model.context_window;
        report.max_output_tokens = model.max_output_tokens;
        report.max_parallel_predictions = model.max_parallel_predictions;
        report.load_state = model.load_state;
    }
    report.matched_model = matched_model;

    if model_availability_passes(
        report.provider_metadata_mode,
        report.v1_present,
        report.native_present,
        report.require_vision,
        report.vision_capable,
        report.load_state,
    ) {
        report.status = ModelAvailabilityStatus::Pass;
    } else {
        report.readiness_detail = Some(model_readiness_failure_detail(&report));
    }
    report
}

fn model_readiness_failure_detail(report: &ModelAvailabilityReport) -> String {
    match report.provider_metadata_mode {
        ProviderMetadataMode::LmStudioNativeRequired if report.native_present => {
            match report.load_state {
                ProviderModelLoadState::NotLoaded =>
                    "model is registered in the LM Studio catalog, but no instance is currently loaded".to_string(),
                ProviderModelLoadState::Unknown =>
                    "model is registered in the LM Studio catalog, but the provider did not report instance load state".to_string(),
                ProviderModelLoadState::Loaded if report.require_vision && !report.vision_capable =>
                    "the loaded model does not advertise the required image capability".to_string(),
                ProviderModelLoadState::Loaded =>
                    "the loaded model did not satisfy the requested readiness contract".to_string(),
            }
        }
        ProviderMetadataMode::OpenAiCompatibleOnly
            if report.v1_present && report.require_vision && !report.vision_capable =>
        {
            "the registered model does not advertise the required image capability".to_string()
        }
        _ => "the configured model is not registered in the declared provider catalog".to_string(),
    }
}

fn model_availability_passes(
    provider_metadata_mode: ProviderMetadataMode,
    v1_present: bool,
    native_present: bool,
    require_vision: bool,
    vision_capable: bool,
    load_state: ProviderModelLoadState,
) -> bool {
    let provider_ok = match provider_metadata_mode {
        ProviderMetadataMode::LmStudioNativeRequired => {
            native_present && load_state == ProviderModelLoadState::Loaded
        }
        ProviderMetadataMode::OpenAiCompatibleOnly => v1_present,
    };
    let vision_ok = !require_vision || vision_capable;
    provider_ok && vision_ok
}

fn build_probe_client(config: &ResolvedConfig) -> Result<reqwest::Client, LlmError> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(config.model.connect_timeout_ms))
        .timeout(PROVIDER_READINESS_REQUEST_TIMEOUT)
        .build()?)
}

fn build_probe_headers(config: &ResolvedConfig) -> Result<HeaderMap, LlmError> {
    let mut headers = HeaderMap::new();
    if let Some(api_key) =
        crate::llm::resolve_api_key_from_env(config.model.api_key_env.as_deref())?
    {
        let value = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|error| LlmError::Message(format!("invalid API key header: {error}")))?;
        headers.insert(AUTHORIZATION, value);
    }
    for (name, value) in &config.model.extra_headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| LlmError::Message(format!("invalid header name `{name}`: {error}")))?;
        let value = HeaderValue::from_str(value).map_err(|error| {
            LlmError::Message(format!("invalid header value for `{name}`: {error}"))
        })?;
        headers.insert(name, value);
    }
    Ok(headers)
}

pub fn apply_provider_model_info_to_config(
    config: &mut crate::config::model::ModelConfig,
    model: &ProviderModelInfo,
) {
    if let Some(context_window) = model.context_window {
        config.context_window = context_window;
        config.extra_body_json = Some(extra_body_with_num_ctx(
            config.extra_body_json.clone(),
            context_window,
        ));
    }
    if let Some(max_output_tokens) = model.max_output_tokens {
        config.max_output_tokens = max_output_tokens;
    }
    if let Some(value) = model.supports_images {
        config.supports_images = value;
    }
    if model.supports_tools == Some(true) {
        config.supports_tools = true;
    }
    if let Some(value) = model.supports_reasoning {
        config.supports_reasoning = value;
    }
    if let Some(value) = model.max_parallel_predictions {
        config.max_parallel_predictions = value.max(1);
    }
}

pub fn validate_model_availability_report(
    config: &crate::config::model::ModelConfig,
    report: &ModelAvailabilityReport,
    require_vision: bool,
) -> Result<(), LlmError> {
    if !matches!(report.status, ModelAvailabilityStatus::Pass) {
        return Err(LlmError::Message(format!(
            "model availability gate did not pass for `{}`{}",
            report.model,
            report
                .readiness_detail
                .as_deref()
                .map(|detail| format!(": {detail}"))
                .unwrap_or_default()
        )));
    }
    if config.model.trim() != report.model {
        return Err(LlmError::Message(format!(
            "model availability report for `{}` does not match configured model `{}`",
            report.model, config.model
        )));
    }
    let configured_base_url = ProviderEndpoint::parse(&config.base_url)
        .map_err(|error| LlmError::Message(error.to_string()))?
        .catalog_root();
    let report_base_url = ProviderEndpoint::parse(&report.base_url)
        .map_err(|error| LlmError::Message(error.to_string()))?
        .catalog_root();
    if configured_base_url != report_base_url {
        return Err(LlmError::Message(
            "model availability report does not match the configured provider".to_string(),
        ));
    }
    if config.provider_metadata_mode != report.provider_metadata_mode {
        return Err(LlmError::Message(format!(
            "model availability report metadata mode {:?} does not match configured mode {:?}",
            report.provider_metadata_mode, config.provider_metadata_mode
        )));
    }
    if report.require_vision != require_vision {
        return Err(LlmError::Message(format!(
            "model availability report vision requirement {} does not match run requirement {}",
            report.require_vision, require_vision
        )));
    }
    let matched_model = report.matched_model.as_ref().ok_or_else(|| {
        LlmError::Message(format!(
            "model availability report passed without matched model metadata for `{}`",
            report.model
        ))
    })?;
    if matched_model.load_state != report.load_state {
        return Err(LlmError::Message(format!(
            "model availability report for `{}` has inconsistent load-state evidence",
            report.model
        )));
    }
    if config.provider_metadata_mode == ProviderMetadataMode::LmStudioNativeRequired
        && report.load_state != ProviderModelLoadState::Loaded
    {
        return Err(LlmError::Message(format!(
            "model availability report for `{}` does not prove a loaded LM Studio instance",
            report.model
        )));
    }
    Ok(())
}

async fn fetch_openai_model_infos(
    client: &reqwest::Client,
    base_url: &ProviderEndpoint,
    headers: HeaderMap,
) -> Result<Vec<ProviderModelInfo>, LlmError> {
    let endpoint = base_url
        .join_api_path("v1/models")
        .map_err(|error| LlmError::Message(error.to_string()))?;
    let response = client.get(endpoint).headers(headers).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = bounded_provider_metadata_text(response)
            .await
            .unwrap_or_else(|error| format!("<response body unavailable: {error}>"));
        return Err(LlmError::Message(format!(
            "model list request failed with status {}: {}",
            status,
            summarize_body(&body)
        )));
    }

    let body = read_bounded_provider_metadata_body(response).await?;
    let payload = serde_json::from_slice::<OpenAiModelsResponse>(&body)?;
    let mut models = parse_openai_compatible_model_infos(&payload);
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    Ok(models)
}

fn parse_openai_compatible_model_infos(payload: &OpenAiModelsResponse) -> Vec<ProviderModelInfo> {
    payload
        .data
        .iter()
        .filter_map(|entry| {
            let id = string_field(entry, &["id"])?;
            let id = id.trim();
            if id.is_empty() {
                return None;
            }
            Some(ProviderModelInfo {
                id: id.to_string(),
                display_name: string_field(entry, &["display_name", "displayName", "name"]),
                context_window: number_field_u32(
                    entry,
                    &[
                        "context_length",
                        "contextLength",
                        "context_window",
                        "contextWindow",
                        "max_context_length",
                        "maxContextLength",
                        "max_model_len",
                        "maxModelLen",
                        "max_request_tokens",
                        "maxRequestTokens",
                    ],
                ),
                max_output_tokens: number_field_u32(
                    entry,
                    &[
                        "max_output_tokens",
                        "maxOutputTokens",
                        "max_tokens",
                        "maxTokens",
                        "max_completion_tokens",
                        "maxCompletionTokens",
                        "max_new_tokens",
                        "maxNewTokens",
                        "max_prediction_tokens",
                        "maxPredictionTokens",
                    ],
                ),
                supports_images: bool_field_nested(
                    entry,
                    &[
                        &["capabilities", "vision"],
                        &["capabilities", "images"],
                        &["vision"],
                        &["supports_images"],
                    ],
                ),
                supports_tools: bool_field_nested(
                    entry,
                    &[
                        &["capabilities", "tools"],
                        &["capabilities", "trained_for_tool_use"],
                        &["supports_tools"],
                        &["tools"],
                    ],
                ),
                supports_reasoning: bool_field_nested(
                    entry,
                    &[
                        &["capabilities", "reasoning"],
                        &["reasoning"],
                        &["supports_reasoning"],
                    ],
                ),
                max_parallel_predictions: number_field_u32(
                    entry,
                    &[
                        "max_parallel_predictions",
                        "maxParallelPredictions",
                        "parallel_predictions",
                        "parallelPredictions",
                    ],
                ),
                load_state: ProviderModelLoadState::Unknown,
                source: "openai_compat".to_string(),
            })
        })
        .collect()
}

async fn fetch_lmstudio_model_infos(
    client: &reqwest::Client,
    base_url: &ProviderEndpoint,
    headers: HeaderMap,
) -> Result<Vec<ProviderModelInfo>, LlmError> {
    let endpoint = base_url
        .join_api_path("api/v1/models")
        .map_err(|error| LlmError::Message(error.to_string()))?;
    let response = client.get(endpoint).headers(headers).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = bounded_provider_metadata_text(response)
            .await
            .unwrap_or_else(|error| format!("<response body unavailable: {error}>"));
        return Err(LlmError::Message(format!(
            "LM Studio model metadata request failed with status {}: {}",
            status,
            summarize_body(&body)
        )));
    }
    let body = read_bounded_provider_metadata_body(response).await?;
    let payload = serde_json::from_slice::<Value>(&body)?;
    Ok(parse_lmstudio_model_infos(&payload))
}

async fn bounded_provider_metadata_text(response: reqwest::Response) -> Result<String, LlmError> {
    let body = read_bounded_provider_metadata_body(response).await?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

async fn read_bounded_provider_metadata_body(
    response: reqwest::Response,
) -> Result<Vec<u8>, LlmError> {
    if response
        .content_length()
        .is_some_and(|length| length > PROVIDER_METADATA_RESPONSE_LIMIT_BYTES as u64)
    {
        return Err(LlmError::Message(format!(
            "provider metadata response exceeds the {} byte limit",
            PROVIDER_METADATA_RESPONSE_LIMIT_BYTES
        )));
    }
    let initial_capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(PROVIDER_METADATA_RESPONSE_LIMIT_BYTES);
    let mut body = Vec::with_capacity(initial_capacity);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len().saturating_add(chunk.len()) > PROVIDER_METADATA_RESPONSE_LIMIT_BYTES {
            return Err(LlmError::Message(format!(
                "provider metadata response exceeds the {} byte limit",
                PROVIDER_METADATA_RESPONSE_LIMIT_BYTES
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn parse_lmstudio_model_infos(payload: &Value) -> Vec<ProviderModelInfo> {
    let Some(entries) = payload
        .get("models")
        .and_then(Value::as_array)
        .or_else(|| payload.as_array())
    else {
        return Vec::new();
    };

    let mut models = Vec::new();
    for entry in entries {
        let model_type = string_field(entry, &["type"]);
        if model_type
            .as_deref()
            .map(|value| value != "llm")
            .unwrap_or(false)
        {
            continue;
        }
        let Some(id) = string_field(entry, &["key", "id"]) else {
            continue;
        };
        if id.trim().is_empty() {
            continue;
        }
        let loaded_instance = preferred_loaded_instance(entry);
        let load_state = lmstudio_model_load_state(entry);
        models.push(ProviderModelInfo {
            id: id.trim().to_string(),
            display_name: string_field(entry, &["display_name", "displayName", "name"]),
            context_window: loaded_instance
                .and_then(|loaded| {
                    number_field_u32(
                        loaded,
                        &["context_length", "contextLength", "num_ctx", "numCtx"],
                    )
                })
                .or_else(|| {
                    number_field_u32(
                        entry,
                        &["context_length", "contextLength", "num_ctx", "numCtx"],
                    )
                }),
            max_output_tokens: loaded_instance
                .and_then(|loaded| {
                    number_field_u32(
                        loaded,
                        &[
                            "max_prediction_tokens",
                            "maxPredictionTokens",
                            "max_num_predict",
                            "maxNumPredict",
                            "max_output_tokens",
                            "maxOutputTokens",
                            "max_tokens",
                            "maxTokens",
                        ],
                    )
                })
                .or_else(|| {
                    number_field_u32(
                        entry,
                        &[
                            "max_prediction_tokens",
                            "maxPredictionTokens",
                            "max_num_predict",
                            "maxNumPredict",
                            "max_output_tokens",
                            "maxOutputTokens",
                            "max_tokens",
                            "maxTokens",
                        ],
                    )
                }),
            supports_images: bool_field_nested(
                entry,
                &[
                    &["capabilities", "vision"],
                    &["vision"],
                    &["capabilities", "images"],
                ],
            ),
            supports_tools: bool_field_nested(
                entry,
                &[
                    &["capabilities", "trained_for_tool_use"],
                    &["trained_for_tool_use"],
                    &["supports_tools"],
                    &["tools"],
                ],
            ),
            supports_reasoning: bool_field_nested(
                entry,
                &[
                    &["capabilities", "reasoning"],
                    &["reasoning"],
                    &["supports_reasoning"],
                ],
            ),
            max_parallel_predictions: loaded_instance
                .and_then(|loaded| {
                    number_field_u32(
                        loaded,
                        &[
                            "max_parallel_predictions",
                            "maxParallelPredictions",
                            "parallel_predictions",
                            "parallelPredictions",
                            "parallel_count",
                            "parallelCount",
                        ],
                    )
                })
                .or_else(|| {
                    number_field_u32(
                        entry,
                        &[
                            "max_parallel_predictions",
                            "maxParallelPredictions",
                            "parallel_predictions",
                            "parallelPredictions",
                            "parallel_count",
                            "parallelCount",
                        ],
                    )
                }),
            load_state,
            source: "lmstudio_api".to_string(),
        });
    }
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    models
}

fn preferred_loaded_instance(entry: &Value) -> Option<&Value> {
    entry
        .get("loaded_instances")
        .or_else(|| entry.get("loadedInstances"))
        .and_then(Value::as_array)
        .and_then(|instances| instances.first())
}

fn lmstudio_model_load_state(entry: &Value) -> ProviderModelLoadState {
    match entry
        .get("loaded_instances")
        .or_else(|| entry.get("loadedInstances"))
        .and_then(Value::as_array)
    {
        Some(instances) if instances.is_empty() => ProviderModelLoadState::NotLoaded,
        Some(_) => ProviderModelLoadState::Loaded,
        None => ProviderModelLoadState::Unknown,
    }
}

fn string_field(value: &Value, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str))
        .map(ToString::to_string)
}

fn number_field_u32(value: &Value, names: &[&str]) -> Option<u32> {
    names.iter().find_map(|name| {
        value
            .get(*name)
            .and_then(Value::as_u64)
            .and_then(|number| u32::try_from(number).ok())
    })
}

fn bool_field_nested(value: &Value, paths: &[&[&str]]) -> Option<bool> {
    for path in paths {
        let mut cursor = value;
        let mut found = true;
        for segment in *path {
            if let Some(next) = cursor.get(*segment) {
                cursor = next;
            } else {
                found = false;
                break;
            }
        }
        if found {
            if let Some(value) = cursor.as_bool() {
                return Some(value);
            }
            if cursor.is_object() {
                return Some(true);
            }
            if cursor.is_array() {
                return Some(cursor.as_array().is_some_and(|values| !values.is_empty()));
            }
        }
    }
    None
}

pub fn extra_body_with_num_ctx(extra_body: Option<Value>, num_ctx: u32) -> Value {
    let mut value = extra_body.unwrap_or_else(|| serde_json::json!({}));
    match &mut value {
        Value::Object(map) => {
            map.insert("num_ctx".to_string(), Value::from(num_ctx));
            value
        }
        _ => serde_json::json!({ "num_ctx": num_ctx }),
    }
}

fn summarize_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() <= 200 {
        trimmed.to_string()
    } else {
        let prefix = trimmed.chars().take(200).collect::<String>();
        format!("{prefix}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_catalog_normalization_accepts_lm_studio_root_and_v1() {
        assert_eq!(
            normalize_provider_base_url("http://lm-studio.local:1234"),
            "http://lm-studio.local:1234"
        );
        assert_eq!(
            normalize_provider_base_url("http://lm-studio.local:1234/v1/"),
            "http://lm-studio.local:1234"
        );
    }

    #[tokio::test]
    async fn availability_rejects_secret_bearing_endpoint_before_network_and_debug() {
        let config = ResolvedConfig::default();
        let raw = "https://user:super-secret@provider.invalid/v1?api_key=hidden";

        let report = check_model_availability(&config, None, Some(raw), false).await;

        assert_eq!(report.status, ModelAvailabilityStatus::Fail);
        assert_eq!(report.base_url, "<invalid-provider-endpoint>");
        let serialized = serde_json::to_string(&report).expect("serialize report");
        let debug = format!("{report:?}");
        for diagnostic in [serialized, debug] {
            assert!(!diagnostic.contains("super-secret"));
            assert!(!diagnostic.contains("hidden"));
            assert!(!diagnostic.contains(raw));
        }

        let mut malicious_report = passing_availability_report(&config.model, false);
        malicious_report.base_url = raw.to_string();
        malicious_report.openai_error = Some("server echoed super-secret".to_string());
        let debug = format!("{malicious_report:?}");
        assert!(!debug.contains("super-secret"));
        assert!(!debug.contains("hidden"));
    }

    fn passing_availability_report(
        config: &crate::config::model::ModelConfig,
        require_vision: bool,
    ) -> ModelAvailabilityReport {
        let matched_model = ProviderModelInfo {
            id: config.model.clone(),
            display_name: Some("runtime provider model".to_string()),
            context_window: Some(config.context_window.saturating_mul(2)),
            max_output_tokens: Some(config.max_output_tokens.saturating_mul(2)),
            supports_images: Some(true),
            supports_tools: Some(true),
            supports_reasoning: Some(true),
            max_parallel_predictions: Some(config.max_parallel_predictions.saturating_add(3)),
            load_state: ProviderModelLoadState::Loaded,
            source: "test".to_string(),
        };
        ModelAvailabilityReport {
            gate: "model_availability".to_string(),
            status: ModelAvailabilityStatus::Pass,
            generated_by: "test".to_string(),
            model: config.model.trim().to_string(),
            base_url: normalize_provider_base_url(&config.base_url),
            provider_metadata_mode: config.provider_metadata_mode,
            v1_present: true,
            native_present: true,
            require_vision,
            vision_capable: require_vision,
            tool_use_capable: Some(true),
            reasoning_capable: Some(true),
            context: matched_model.context_window,
            max_output_tokens: matched_model.max_output_tokens,
            max_parallel_predictions: matched_model.max_parallel_predictions,
            load_state: matched_model.load_state,
            readiness_detail: None,
            matched_model: Some(matched_model),
            v1_models: vec![config.model.trim().to_string()],
            native_models: vec![config.model.trim().to_string()],
            openai_error: None,
            native_error: None,
            checked_at_ms: 1,
        }
    }

    #[test]
    fn availability_report_validation_never_hydrates_product_config() {
        let mut config = ResolvedConfig::default().model;
        config.base_url = "http://provider.local/v1".to_string();
        config.model = "configured-model".to_string();
        config.provider_metadata_mode = ProviderMetadataMode::OpenAiCompatibleOnly;
        config.context_window = 4_096;
        config.max_output_tokens = 512;
        config.supports_tools = false;
        config.supports_reasoning = false;
        config.supports_images = false;
        config.max_parallel_predictions = 1;
        let before = serde_json::to_value(&config).expect("serialize config");
        let report = passing_availability_report(&config, false);

        validate_model_availability_report(&config, &report, false)
            .expect("passing runtime projection");

        assert_eq!(
            serde_json::to_value(&config).expect("serialize config"),
            before
        );
        assert_eq!(config.context_window, 4_096);
        assert_eq!(config.max_output_tokens, 512);
        assert!(!config.supports_tools);
        assert!(!config.supports_reasoning);
        assert!(!config.supports_images);
        assert_eq!(config.max_parallel_predictions, 1);
    }

    #[test]
    fn availability_report_validation_rejects_stale_or_incomplete_projection() {
        let mut config = ResolvedConfig::default().model;
        config.base_url = "http://provider.local".to_string();
        config.model = "configured-model".to_string();
        config.provider_metadata_mode = ProviderMetadataMode::OpenAiCompatibleOnly;
        let report = passing_availability_report(&config, false);

        let mut stale_model = report.clone();
        stale_model.model = "other-model".to_string();
        assert!(validate_model_availability_report(&config, &stale_model, false).is_err());

        let mut stale_provider = report.clone();
        stale_provider.base_url = "http://other-provider.local".to_string();
        assert!(validate_model_availability_report(&config, &stale_provider, false).is_err());

        let mut stale_mode = report.clone();
        stale_mode.provider_metadata_mode = ProviderMetadataMode::LmStudioNativeRequired;
        assert!(validate_model_availability_report(&config, &stale_mode, false).is_err());

        assert!(validate_model_availability_report(&config, &report, true).is_err());

        let mut incomplete = report.clone();
        incomplete.matched_model = None;
        assert!(validate_model_availability_report(&config, &incomplete, false).is_err());

        let mut failed = report;
        failed.status = ModelAvailabilityStatus::Fail;
        assert!(validate_model_availability_report(&config, &failed, false).is_err());
    }

    #[test]
    fn lmstudio_parser_does_not_treat_model_max_context_as_hosting_context() {
        let payload = serde_json::json!({
            "models": [
                {
                    "key": "openai-compatible-fixture-model",
                    "type": "llm",
                    "loaded_instances": [],
                    "context_length": null,
                    "max_context_length": 262144
                }
            ]
        });

        let models = parse_lmstudio_model_infos(&payload);

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "openai-compatible-fixture-model");
        assert_eq!(models[0].context_window, None);
        assert_eq!(models[0].max_output_tokens, None);
        assert_eq!(models[0].load_state, ProviderModelLoadState::NotLoaded);
    }

    #[test]
    fn lmstudio_parser_uses_loaded_instance_context_as_active_context() {
        let payload = serde_json::json!({
            "models": [
                {
                    "key": "openai-compatible-fixture-model",
                    "type": "llm",
                    "loaded_instances": [
                        {
                            "context_length": 131072,
                            "max_prediction_tokens": 8192
                        }
                    ],
                    "max_context_length": 262144
                }
            ]
        });

        let models = parse_lmstudio_model_infos(&payload);

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].context_window, Some(131_072));
        assert_eq!(models[0].max_output_tokens, Some(8_192));
        assert_eq!(models[0].load_state, ProviderModelLoadState::Loaded);
    }

    #[test]
    fn lmstudio_context_without_output_limit_preserves_configured_max_output() {
        let payload = serde_json::json!({
            "models": [
                {
                    "key": "openai-compatible-fixture-model",
                    "type": "llm",
                    "loaded_instances": [
                        {
                            "context_length": 131072
                        }
                    ]
                }
            ]
        });
        let models = parse_lmstudio_model_infos(&payload);
        let mut config = ResolvedConfig::default().model;
        config.max_output_tokens = 4_096;

        apply_provider_model_info_to_config(&mut config, &models[0]);

        assert_eq!(models[0].context_window, Some(131_072));
        assert_eq!(models[0].max_output_tokens, None);
        assert_eq!(config.context_window, 131_072);
        assert_eq!(config.max_output_tokens, 4_096);
    }

    #[tokio::test]
    async fn provider_model_infos_can_load_from_lmstudio_native_when_v1_models_is_missing() {
        use axum::{Json, Router, http::StatusCode, routing::get};

        let app = Router::new()
            .route(
                "/api/v1/models",
                get(|| async {
                    Json(serde_json::json!({
                        "models": [
                            {
                                "key": "native-model",
                                "type": "llm",
                                "display_name": "Native Model",
                                "context_length": 32768,
                                "max_prediction_tokens": 4096,
                                "capabilities": {
                                    "trained_for_tool_use": true,
                                    "reasoning": true
                                }
                            }
                        ]
                    }))
                }),
            )
            .fallback(|| async {
                (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": "not found"})),
                )
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("test server");
        });

        let mut config = ResolvedConfig::default();
        config.model.provider_metadata_mode = ProviderMetadataMode::LmStudioNativeRequired;

        let models = fetch_provider_model_infos(&config, &format!("http://{addr}"))
            .await
            .expect("models from native endpoint");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "native-model");
        assert_eq!(models[0].source, "lmstudio_api");
        assert_eq!(models[0].context_window, Some(32_768));
        assert_eq!(models[0].max_output_tokens, Some(4096));
        assert_eq!(models[0].supports_tools, Some(true));
        assert_eq!(models[0].supports_reasoning, Some(true));
        assert_eq!(models[0].load_state, ProviderModelLoadState::Unknown);

        server.abort();
    }

    #[test]
    fn openai_compatible_parser_uses_extended_metadata_when_provider_exposes_it() {
        let payload = OpenAiModelsResponse {
            data: vec![serde_json::json!({
                "id": "openai-compatible-fixture-model",
                "max_model_len": 131072,
                "max_output_tokens": 8192,
                "capabilities": {
                    "tools": true,
                    "reasoning": false
                }
            })],
        };

        let models = parse_openai_compatible_model_infos(&payload);

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].context_window, Some(131_072));
        assert_eq!(models[0].max_output_tokens, Some(8_192));
        assert_eq!(models[0].supports_tools, Some(true));
        assert_eq!(models[0].supports_reasoning, Some(false));
        assert_eq!(models[0].load_state, ProviderModelLoadState::Unknown);
    }

    #[test]
    fn availability_uses_only_the_declared_metadata_dialect() {
        assert!(model_availability_passes(
            ProviderMetadataMode::OpenAiCompatibleOnly,
            true,
            false,
            false,
            false,
            ProviderModelLoadState::Unknown
        ));
        assert!(!model_availability_passes(
            ProviderMetadataMode::LmStudioNativeRequired,
            true,
            false,
            false,
            false,
            ProviderModelLoadState::Loaded
        ));
        assert!(model_availability_passes(
            ProviderMetadataMode::LmStudioNativeRequired,
            false,
            true,
            false,
            false,
            ProviderModelLoadState::Loaded
        ));
        assert!(!model_availability_passes(
            ProviderMetadataMode::OpenAiCompatibleOnly,
            true,
            false,
            true,
            false,
            ProviderModelLoadState::Unknown
        ));
        assert!(model_availability_passes(
            ProviderMetadataMode::OpenAiCompatibleOnly,
            true,
            false,
            true,
            true,
            ProviderModelLoadState::Unknown
        ));
    }

    #[test]
    fn lmstudio_catalog_presence_does_not_claim_loaded_availability() {
        assert!(!model_availability_passes(
            ProviderMetadataMode::LmStudioNativeRequired,
            false,
            true,
            false,
            false,
            ProviderModelLoadState::NotLoaded,
        ));
        assert!(!model_availability_passes(
            ProviderMetadataMode::LmStudioNativeRequired,
            false,
            true,
            false,
            false,
            ProviderModelLoadState::Unknown,
        ));
        assert!(model_availability_passes(
            ProviderMetadataMode::LmStudioNativeRequired,
            false,
            true,
            false,
            false,
            ProviderModelLoadState::Loaded,
        ));

        let config = ResolvedConfig::default().model;
        let mut report = passing_availability_report(&config, false);
        report.provider_metadata_mode = ProviderMetadataMode::LmStudioNativeRequired;
        report.native_present = true;
        report.load_state = ProviderModelLoadState::NotLoaded;
        assert!(
            model_readiness_failure_detail(&report).contains("registered")
                && model_readiness_failure_detail(&report).contains("no instance")
        );
        report.load_state = ProviderModelLoadState::Unknown;
        assert!(model_readiness_failure_detail(&report).contains("did not report"));
        report.native_present = false;
        assert!(model_readiness_failure_detail(&report).contains("not registered"));
    }

    #[tokio::test]
    async fn provider_catalog_rejects_a_chunked_body_above_the_transport_limit() {
        use std::convert::Infallible;

        use axum::{Router, body::Body, http::Response, routing::get};
        use bytes::Bytes;
        use futures_util::stream;

        let app = Router::new().route(
            "/v1/models",
            get(|| async {
                let half = PROVIDER_METADATA_RESPONSE_LIMIT_BYTES / 2 + 1;
                let chunks = vec![
                    Ok::<_, Infallible>(Bytes::from(vec![b' '; half])),
                    Ok::<_, Infallible>(Bytes::from(vec![b' '; half])),
                ];
                Response::new(Body::from_stream(stream::iter(chunks)))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("test server");
        });
        let mut config = ResolvedConfig::default();
        config.model.provider_metadata_mode = ProviderMetadataMode::OpenAiCompatibleOnly;

        let error = fetch_provider_model_infos(&config, &format!("http://{addr}"))
            .await
            .expect_err("oversized chunked catalog body must fail closed");

        assert!(error.to_string().contains("exceeds the"));
        assert!(error.to_string().contains("byte limit"));
        server.abort();
    }
}
