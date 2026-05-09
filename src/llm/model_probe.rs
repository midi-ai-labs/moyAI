use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::config::ResolvedConfig;
use crate::error::LlmError;

#[derive(Debug, Deserialize)]
struct OpenAiModelsResponse {
    #[serde(default)]
    data: Vec<OpenAiModelEntry>,
}

#[derive(Debug, Deserialize)]
struct OpenAiModelEntry {
    id: String,
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
    pub loaded: bool,
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailabilityStatus {
    Pass,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAvailabilityReport {
    pub gate: String,
    pub status: ModelAvailabilityStatus,
    pub generated_by: String,
    pub model: String,
    pub base_url: String,
    pub v1_present: bool,
    pub native_present: bool,
    pub require_vision: bool,
    pub vision_capable: bool,
    pub tool_use_capable: Option<bool>,
    pub reasoning_capable: Option<bool>,
    pub context: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub max_parallel_predictions: Option<u32>,
    pub matched_model: Option<ProviderModelInfo>,
    pub v1_models: Vec<String>,
    pub native_models: Vec<String>,
    pub openai_error: Option<String>,
    pub native_error: Option<String>,
    pub checked_at_ms: u64,
}

pub fn normalize_provider_base_url(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    match trimmed.strip_suffix("/v1") {
        Some(prefix) if !prefix.is_empty() => prefix.to_string(),
        _ => trimmed.to_string(),
    }
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
    let base_url = normalize_provider_base_url(base_url_input);
    if base_url.is_empty() {
        return Err(LlmError::Message("provider URL is empty".to_string()));
    }

    let client = build_probe_client(config)?;
    let headers = build_probe_headers(config)?;

    let openai_models = fetch_openai_model_infos(&client, &base_url, headers.clone()).await?;
    if openai_models.is_empty() {
        return Err(LlmError::Message(
            "provider returned an empty model list".to_string(),
        ));
    }
    let mut models = openai_models
        .into_iter()
        .map(|model| (model.id.clone(), model))
        .collect::<std::collections::BTreeMap<_, _>>();

    if let Ok(native_models) = fetch_lmstudio_model_infos(&client, &base_url, headers).await {
        for model in native_models {
            models
                .entry(model.id.clone())
                .and_modify(|existing| existing.enrich_from(&model))
                .or_insert(model);
        }
    }

    let mut models = models.into_values().collect::<Vec<_>>();
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
    let base_url = normalize_provider_base_url(base_url_override.unwrap_or(&config.model.base_url));
    let checked_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);

    let mut report = ModelAvailabilityReport {
        gate: "model_availability".to_string(),
        status: ModelAvailabilityStatus::Fail,
        generated_by: "moyai_model_availability_v1".to_string(),
        model,
        base_url,
        v1_present: false,
        native_present: false,
        require_vision,
        vision_capable: false,
        tool_use_capable: None,
        reasoning_capable: None,
        context: None,
        max_output_tokens: None,
        max_parallel_predictions: None,
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
    if report.base_url.is_empty() {
        report.openai_error = Some("provider URL is empty".to_string());
        report.native_error = Some("provider URL is empty".to_string());
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

    let openai_models =
        match fetch_openai_model_infos(&client, &report.base_url, headers.clone()).await {
            Ok(models) => models,
            Err(error) => {
                report.openai_error = Some(error.to_string());
                Vec::new()
            }
        };
    let native_models = match fetch_lmstudio_model_infos(&client, &report.base_url, headers).await {
        Ok(models) => models,
        Err(error) => {
            report.native_error = Some(error.to_string());
            Vec::new()
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

    let mut matched_model = openai_models
        .iter()
        .find(|entry| entry.id == report.model)
        .cloned()
        .or_else(|| {
            native_models
                .iter()
                .find(|entry| entry.id == report.model)
                .cloned()
        });
    if let Some(existing) = matched_model.as_mut() {
        if let Some(native) = native_models.iter().find(|entry| entry.id == report.model) {
            existing.enrich_from(native);
        }
    }

    if let Some(model) = matched_model.as_ref() {
        report.vision_capable = model.supports_images.unwrap_or(false);
        report.tool_use_capable = model.supports_tools;
        report.reasoning_capable = model.supports_reasoning;
        report.context = model.context_window;
        report.max_output_tokens = model.max_output_tokens;
        report.max_parallel_predictions = model.max_parallel_predictions;
    }
    report.matched_model = matched_model;

    let vision_ok = !report.require_vision || report.vision_capable;
    if report.v1_present && report.native_present && vision_ok {
        report.status = ModelAvailabilityStatus::Pass;
    }
    report
}

pub async fn ensure_openai_model_available(config: &ResolvedConfig) -> Result<(), LlmError> {
    let configured_model = config.model.model.trim();
    if configured_model.is_empty() {
        return Err(LlmError::Message("configured model is empty".to_string()));
    }

    let models = fetch_provider_model_infos(config, &config.model.base_url).await?;
    if models.iter().any(|model| model.id == configured_model) {
        return Ok(());
    }
    let ids = models
        .iter()
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();

    Err(LlmError::Message(format!(
        "configured model `{configured_model}` is not available at `{}`; available models: {}",
        normalize_provider_base_url(&config.model.base_url),
        summarize_models(&ids)
    )))
}

fn build_probe_client(config: &ResolvedConfig) -> Result<reqwest::Client, LlmError> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(config.model.connect_timeout_ms))
        .timeout(Duration::from_millis(config.model.request_timeout_ms))
        .build()?)
}

fn build_probe_headers(config: &ResolvedConfig) -> Result<HeaderMap, LlmError> {
    let mut headers = HeaderMap::new();
    if let Some(api_key) = config
        .model
        .api_key_env
        .as_ref()
        .and_then(|name| std::env::var(name).ok())
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
    if let Some(value) = model.supports_tools {
        config.supports_tools = value;
    }
    if let Some(value) = model.supports_reasoning {
        config.supports_reasoning = value;
    }
    if let Some(value) = model.max_parallel_predictions {
        config.max_parallel_predictions = value.max(1);
    }
    if config.parallel_tool_calls && config.max_parallel_predictions > 1 {
        config.extra_body_json = Some(extra_body_with_parallel_tool_calls(
            config.extra_body_json.clone(),
        ));
    }
}

async fn fetch_openai_model_infos(
    client: &reqwest::Client,
    base_url: &str,
    headers: HeaderMap,
) -> Result<Vec<ProviderModelInfo>, LlmError> {
    let endpoint = format!("{}/v1/models", base_url);
    let response = client.get(&endpoint).headers(headers).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<response body unavailable>".to_string());
        return Err(LlmError::Message(format!(
            "model list request failed with status {}: {}",
            status,
            summarize_body(&body)
        )));
    }

    let payload = response.json::<OpenAiModelsResponse>().await?;
    let mut models = payload
        .data
        .into_iter()
        .map(|entry| entry.id.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|id| ProviderModelInfo {
            id,
            display_name: None,
            context_window: None,
            max_output_tokens: None,
            supports_images: None,
            supports_tools: None,
            supports_reasoning: None,
            max_parallel_predictions: None,
            loaded: false,
            source: "openai_compat".to_string(),
        })
        .collect::<Vec<_>>();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    Ok(models)
}

async fn fetch_lmstudio_model_infos(
    client: &reqwest::Client,
    base_url: &str,
    headers: HeaderMap,
) -> Result<Vec<ProviderModelInfo>, LlmError> {
    let endpoint = format!("{}/api/v1/models", base_url);
    let response = client.get(&endpoint).headers(headers).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<response body unavailable>".to_string());
        return Err(LlmError::Message(format!(
            "LM Studio model metadata request failed with status {}: {}",
            status,
            summarize_body(&body)
        )));
    }
    let payload = response.json::<Value>().await?;
    Ok(parse_lmstudio_model_infos(&payload))
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
                        &[
                            "context_length",
                            "contextLength",
                            "max_context_length",
                            "maxContextLength",
                            "num_ctx",
                            "numCtx",
                        ],
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
                })
                .or_else(|| {
                    loaded_instance.and_then(|loaded| {
                        number_field_u32(loaded, &["context_length", "contextLength"])
                    })
                })
                .or_else(|| number_field_u32(entry, &["max_context_length", "maxContextLength"])),
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
            loaded: loaded_instance.is_some(),
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

fn extra_body_with_num_ctx(extra_body: Option<Value>, num_ctx: u32) -> Value {
    let mut value = extra_body.unwrap_or_else(|| serde_json::json!({}));
    match &mut value {
        Value::Object(map) => {
            map.insert("num_ctx".to_string(), Value::from(num_ctx));
            value
        }
        _ => serde_json::json!({ "num_ctx": num_ctx }),
    }
}

fn extra_body_with_parallel_tool_calls(extra_body: Option<Value>) -> Value {
    let mut value = extra_body.unwrap_or_else(|| serde_json::json!({}));
    match &mut value {
        Value::Object(map) => {
            map.insert("parallel_tool_calls".to_string(), Value::Bool(true));
            value
        }
        _ => serde_json::json!({ "parallel_tool_calls": true }),
    }
}

impl ProviderModelInfo {
    fn enrich_from(&mut self, other: &ProviderModelInfo) {
        self.display_name = other
            .display_name
            .clone()
            .or_else(|| self.display_name.clone());
        self.context_window = other.context_window.or(self.context_window);
        self.max_output_tokens = other.max_output_tokens.or(self.max_output_tokens);
        self.supports_images = other.supports_images.or(self.supports_images);
        self.supports_tools = other.supports_tools.or(self.supports_tools);
        self.supports_reasoning = other.supports_reasoning.or(self.supports_reasoning);
        self.max_parallel_predictions = other
            .max_parallel_predictions
            .or(self.max_parallel_predictions);
        self.loaded = other.loaded || self.loaded;
        if other.source == "lmstudio_api" {
            self.source = other.source.clone();
        }
    }
}

fn summarize_models(models: &[String]) -> String {
    if models.is_empty() {
        return "<none>".to_string();
    }
    let limit = 12;
    let mut summary = models.iter().take(limit).cloned().collect::<Vec<_>>();
    if models.len() > limit {
        summary.push(format!("... and {} more", models.len() - limit));
    }
    summary.join(", ")
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
