use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Value, json};

use crate::config::{ProviderMetadataMode, ResolvedConfig};
use crate::error::LlmError;
use crate::llm::dto::{
    OpenAiChatRequest, OpenAiContent, OpenAiContentPart, OpenAiImageUrl, OpenAiMessage,
};
use crate::llm::openai_compat::{openai_tool_schema_json, provider_tool_choice_json};
use crate::llm::{
    ProviderToolChoice, ToolSchema, effective_parallel_tool_calls,
    tool_surface_scoped_parallel_tool_calls_projection,
};

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
pub struct ToolCallProbeReport {
    pub probe: String,
    pub status: ModelAvailabilityStatus,
    pub tool_choice: String,
    pub required_for_gate: bool,
    pub finish_reason: Option<String>,
    pub tool_call_received: bool,
    pub tool_name: Option<String>,
    pub tool_arguments: Option<String>,
    pub arguments_valid: bool,
    pub content: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisionProbeReport {
    pub probe: String,
    pub status: ModelAvailabilityStatus,
    pub required_for_gate: bool,
    pub image_content_sent: bool,
    pub response_received: bool,
    pub finish_reason: Option<String>,
    pub content: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapabilityKind {
    ToolUse,
    VisionInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilityOverride {
    pub capability: ModelCapabilityKind,
    pub metadata_value: Option<bool>,
    pub effective_value: bool,
    pub evidence_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(default)]
    pub vision_probe_passed: bool,
    #[serde(default)]
    pub vision_probes: Vec<VisionProbeReport>,
    pub tool_use_capable: Option<bool>,
    #[serde(default)]
    pub capability_overrides: Vec<ModelCapabilityOverride>,
    pub tool_call_probe_passed: bool,
    pub tool_call_probes: Vec<ToolCallProbeReport>,
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

    let mut models = std::collections::BTreeMap::new();
    let mut first_error = None;
    match fetch_openai_model_infos(&client, &base_url, headers.clone()).await {
        Ok(openai_models) => {
            for model in openai_models {
                models.insert(model.id.clone(), model);
            }
        }
        Err(error) => first_error = Some(error),
    }

    match fetch_lmstudio_model_infos(&client, &base_url, headers.clone()).await {
        Ok(native_models) => {
            for model in native_models {
                models
                    .entry(model.id.clone())
                    .and_modify(|existing| existing.enrich_from(&model))
                    .or_insert(model);
            }
        }
        Err(error) if first_error.is_none() => first_error = Some(error),
        Err(_) => {}
    }
    match fetch_vllm_mlx_model_infos(&client, &base_url, headers).await {
        Ok(vllm_mlx_models) => {
            for model in vllm_mlx_models {
                models
                    .entry(model.id.clone())
                    .and_modify(|existing| existing.enrich_from(&model))
                    .or_insert(model);
            }
        }
        Err(error) if first_error.is_none() => first_error = Some(error),
        Err(_) => {}
    }

    if models.is_empty() {
        return match first_error {
            Some(error) => Err(error),
            None => Err(LlmError::Message(
                "provider returned an empty model list".to_string(),
            )),
        };
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
        generated_by: "moyai_model_availability_v2".to_string(),
        model,
        base_url,
        provider_metadata_mode: config.model.provider_metadata_mode,
        v1_present: false,
        native_present: false,
        require_vision,
        vision_capable: false,
        vision_probe_passed: false,
        vision_probes: Vec::new(),
        tool_use_capable: None,
        capability_overrides: Vec::new(),
        tool_call_probe_passed: false,
        tool_call_probes: Vec::new(),
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
    let native_models =
        match fetch_lmstudio_model_infos(&client, &report.base_url, headers.clone()).await {
            Ok(models) => models,
            Err(error) => {
                report.native_error = Some(error.to_string());
                Vec::new()
            }
        };
    let vllm_mlx_models =
        match fetch_vllm_mlx_model_infos(&client, &report.base_url, headers.clone()).await {
            Ok(models) => models,
            Err(_) => Vec::new(),
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
        })
        .or_else(|| {
            vllm_mlx_models
                .iter()
                .find(|entry| entry.id == report.model)
                .cloned()
        });
    if let Some(existing) = matched_model.as_mut() {
        if let Some(native) = native_models.iter().find(|entry| entry.id == report.model) {
            existing.enrich_from(native);
        }
        if let Some(vllm_mlx) = vllm_mlx_models
            .iter()
            .find(|entry| entry.id == report.model)
        {
            existing.enrich_from(vllm_mlx);
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

    if report.v1_present && report.require_vision {
        let vision_probe =
            run_vision_probe(&client, &report.base_url, headers.clone(), &report.model).await;
        report.vision_probes = vec![vision_probe];
        report.vision_probe_passed = report
            .vision_probes
            .iter()
            .filter(|probe| probe.required_for_gate)
            .all(|probe| matches!(probe.status, ModelAvailabilityStatus::Pass));
        if report.vision_probe_passed {
            apply_vision_probe_capability_evidence(&mut report);
        } else {
            report.vision_capable = false;
        }
    }

    if report.v1_present {
        report.tool_call_probes = run_tool_call_probe_suite(
            &client,
            &report.base_url,
            headers,
            &report.model,
            report.provider_metadata_mode,
            config.model.parallel_tool_calls,
            report
                .max_parallel_predictions
                .unwrap_or(config.model.max_parallel_predictions),
        )
        .await;
        report.tool_call_probe_passed = !report.tool_call_probes.is_empty()
            && report
                .tool_call_probes
                .iter()
                .filter(|probe| probe.required_for_gate)
                .all(|probe| matches!(probe.status, ModelAvailabilityStatus::Pass));
        apply_tool_call_probe_capability_evidence(&mut report);
    }

    if model_availability_passes(
        report.provider_metadata_mode,
        report.v1_present,
        report.native_present,
        report.require_vision,
        report.vision_capable,
        report.vision_probe_passed,
        report.tool_call_probe_passed,
    ) {
        report.status = ModelAvailabilityStatus::Pass;
    }
    report
}

fn apply_vision_probe_capability_evidence(report: &mut ModelAvailabilityReport) {
    if !report.vision_probe_passed {
        return;
    }
    let metadata_value = report
        .matched_model
        .as_ref()
        .and_then(|model| model.supports_images);
    if metadata_value != Some(true)
        && !report.capability_overrides.iter().any(|override_record| {
            override_record.capability == ModelCapabilityKind::VisionInput
                && override_record.evidence_ref == "vision_probe_passed"
        })
    {
        report.capability_overrides.push(ModelCapabilityOverride {
            capability: ModelCapabilityKind::VisionInput,
            metadata_value,
            effective_value: true,
            evidence_ref: "vision_probe_passed".to_string(),
        });
    }
    report.vision_capable = true;
    if let Some(model) = report.matched_model.as_mut() {
        model.supports_images = Some(true);
    }
}

fn apply_tool_call_probe_capability_evidence(report: &mut ModelAvailabilityReport) {
    if !report.tool_call_probe_passed {
        return;
    }
    let metadata_value = report.tool_use_capable;
    if metadata_value != Some(true)
        && !report.capability_overrides.iter().any(|override_record| {
            override_record.capability == ModelCapabilityKind::ToolUse
                && override_record.evidence_ref == "tool_call_probe_passed"
        })
    {
        report.capability_overrides.push(ModelCapabilityOverride {
            capability: ModelCapabilityKind::ToolUse,
            metadata_value,
            effective_value: true,
            evidence_ref: "tool_call_probe_passed".to_string(),
        });
    }
    report.tool_use_capable = Some(true);
    if let Some(model) = report.matched_model.as_mut() {
        model.supports_tools = Some(true);
    }
}

fn model_availability_passes(
    provider_metadata_mode: ProviderMetadataMode,
    v1_present: bool,
    native_present: bool,
    require_vision: bool,
    vision_capable: bool,
    vision_probe_passed: bool,
    tool_call_probe_passed: bool,
) -> bool {
    let provider_ok = match provider_metadata_mode {
        ProviderMetadataMode::LmStudioNativeRequired => v1_present && native_present,
        ProviderMetadataMode::OpenAiCompatibleOnly => v1_present,
    };
    let vision_ok = !require_vision || (vision_capable && vision_probe_passed);
    provider_ok && vision_ok && tool_call_probe_passed
}

async fn run_vision_probe(
    client: &reqwest::Client,
    base_url: &str,
    headers: HeaderMap,
    model: &str,
) -> VisionProbeReport {
    let endpoint = format!("{}/v1/chat/completions", base_url);
    let body = match vision_probe_request_body(model) {
        Ok(body) => body,
        Err(error) => {
            return failed_vision_probe_report(format!(
                "vision probe request could not be materialized: {error}"
            ));
        }
    };

    let response = match client
        .post(&endpoint)
        .headers(headers)
        .json(&body)
        .timeout(Duration::from_secs(120))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return failed_vision_probe_report(format!("vision probe request failed: {error}"));
        }
    };
    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<response body unavailable>".to_string());
        return failed_vision_probe_report(format!(
            "vision probe request failed with status {}: {}",
            status,
            summarize_body(&body)
        ));
    }
    let payload = match response.json::<Value>().await {
        Ok(payload) => payload,
        Err(error) => {
            return failed_vision_probe_report(format!(
                "vision probe response was not valid JSON: {error}"
            ));
        }
    };
    vision_probe_report_from_response(&payload)
}

fn vision_probe_request_body(model: &str) -> Result<Value, LlmError> {
    const ONE_PIXEL_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/p9sAAAAASUVORK5CYII=";
    let request = OpenAiChatRequest {
        model: model.to_string(),
        stream: false,
        messages: vec![OpenAiMessage {
            role: "user".to_string(),
            content: Some(OpenAiContent::Parts(vec![
                OpenAiContentPart::Text {
                    text: "Reply with a short confirmation that you received this image."
                        .to_string(),
                },
                OpenAiContentPart::ImageUrl {
                    image_url: OpenAiImageUrl {
                        url: format!("data:image/png;base64,{ONE_PIXEL_PNG_BASE64}"),
                    },
                },
            ])),
            tool_calls: None,
            tool_call_id: None,
        }],
        max_tokens: Some(32),
        temperature: Some(0.0),
        top_p: None,
        top_k: None,
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        stop_sequences: Vec::new(),
        tools: Vec::new(),
        parallel_tool_calls: None,
    };
    serde_json::to_value(request)
        .map_err(|error| LlmError::Message(format!("failed to serialize vision probe: {error}")))
}

fn vision_probe_report_from_response(payload: &Value) -> VisionProbeReport {
    let choice = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first());
    let finish_reason = choice.and_then(|choice| string_field(choice, &["finish_reason"]));
    let message = choice.and_then(|choice| choice.get("message"));
    let content = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let response_received = content
        .as_deref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    VisionProbeReport {
        probe: "vision_input_image_url".to_string(),
        status: if response_received {
            ModelAvailabilityStatus::Pass
        } else {
            ModelAvailabilityStatus::Fail
        },
        required_for_gate: true,
        image_content_sent: true,
        response_received,
        finish_reason,
        content,
        error: if response_received {
            None
        } else {
            Some("expected non-empty assistant content after image input probe".to_string())
        },
    }
}

fn failed_vision_probe_report(error: String) -> VisionProbeReport {
    VisionProbeReport {
        probe: "vision_input_image_url".to_string(),
        status: ModelAvailabilityStatus::Fail,
        required_for_gate: true,
        image_content_sent: false,
        response_received: false,
        finish_reason: None,
        content: None,
        error: Some(error),
    }
}

async fn run_tool_call_probe_suite(
    client: &reqwest::Client,
    base_url: &str,
    headers: HeaderMap,
    model: &str,
    provider_metadata_mode: ProviderMetadataMode,
    parallel_tool_calls_enabled: bool,
    max_parallel_predictions: u32,
) -> Vec<ToolCallProbeReport> {
    let probes = [
        ToolCallProbeSpec {
            probe: "tool_choice_required",
            tool_choice_label: "required",
            tool_choice: ProbeToolChoice::Runtime(ProviderToolChoice::Required),
            strong_instruction: false,
            required_for_gate: true,
        },
        ToolCallProbeSpec {
            probe: "tool_choice_named",
            tool_choice_label: "named_function",
            tool_choice: ProbeToolChoice::Runtime(ProviderToolChoice::Named {
                name: "echo_word".to_string(),
            }),
            strong_instruction: false,
            required_for_gate: provider_metadata_mode == ProviderMetadataMode::OpenAiCompatibleOnly,
        },
        ToolCallProbeSpec {
            probe: "tool_choice_auto_strong",
            tool_choice_label: "auto",
            tool_choice: ProbeToolChoice::Auto,
            strong_instruction: true,
            required_for_gate: true,
        },
    ];
    let mut reports = Vec::with_capacity(probes.len());
    for spec in probes {
        reports.push(
            run_tool_call_probe(
                client,
                base_url,
                headers.clone(),
                model,
                provider_metadata_mode,
                &spec,
                parallel_tool_calls_enabled,
                max_parallel_predictions,
            )
            .await,
        );
    }
    reports
}

#[derive(Debug, Clone)]
struct ToolCallProbeSpec {
    probe: &'static str,
    tool_choice_label: &'static str,
    tool_choice: ProbeToolChoice,
    strong_instruction: bool,
    required_for_gate: bool,
}

#[derive(Debug, Clone)]
enum ProbeToolChoice {
    Runtime(ProviderToolChoice),
    Auto,
}

async fn run_tool_call_probe(
    client: &reqwest::Client,
    base_url: &str,
    headers: HeaderMap,
    model: &str,
    provider_metadata_mode: ProviderMetadataMode,
    spec: &ToolCallProbeSpec,
    parallel_tool_calls_enabled: bool,
    max_parallel_predictions: u32,
) -> ToolCallProbeReport {
    let endpoint = format!("{}/v1/chat/completions", base_url);
    let body = match tool_call_probe_request_body(
        model,
        provider_metadata_mode,
        &spec.tool_choice,
        spec.strong_instruction,
        parallel_tool_calls_enabled,
        max_parallel_predictions,
    ) {
        Ok(body) => body,
        Err(error) => {
            return failed_tool_call_probe_report(
                spec.probe,
                spec.tool_choice_label,
                spec.required_for_gate,
                format!("tool-call probe request could not be materialized: {error}"),
            );
        }
    };

    let response = match client
        .post(&endpoint)
        .headers(headers)
        .json(&body)
        .timeout(Duration::from_secs(120))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return failed_tool_call_probe_report(
                spec.probe,
                spec.tool_choice_label,
                spec.required_for_gate,
                format!("tool-call probe request failed: {error}"),
            );
        }
    };
    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<response body unavailable>".to_string());
        return failed_tool_call_probe_report(
            spec.probe,
            spec.tool_choice_label,
            spec.required_for_gate,
            format!(
                "tool-call probe request failed with status {}: {}",
                status,
                summarize_body(&body)
            ),
        );
    }
    let payload = match response.json::<Value>().await {
        Ok(payload) => payload,
        Err(error) => {
            return failed_tool_call_probe_report(
                spec.probe,
                spec.tool_choice_label,
                spec.required_for_gate,
                format!("tool-call probe response was not valid JSON: {error}"),
            );
        }
    };
    tool_call_probe_report_from_response(
        spec.probe,
        spec.tool_choice_label,
        spec.required_for_gate,
        &payload,
    )
}

fn tool_call_probe_request_body(
    model: &str,
    provider_metadata_mode: ProviderMetadataMode,
    tool_choice: &ProbeToolChoice,
    strong_instruction: bool,
    parallel_tool_calls_enabled: bool,
    max_parallel_predictions: u32,
) -> Result<Value, LlmError> {
    let messages = if strong_instruction {
        json!([
            {
                "role": "system",
                "content": "You are connected to tools. When a tool is available and the user requests using it, respond only by calling the tool. Never answer directly."
            },
            {
                "role": "user",
                "content": "Call echo_word with word ping now."
            }
        ])
    } else {
        json!([
            {
                "role": "user",
                "content": "Call echo_word with word ping now."
            }
        ])
    };
    let echo_tool = echo_word_tool_schema();
    let mut body = json!({
        "model": model,
        "stream": false,
        "messages": messages,
        "tools": [openai_tool_schema_json(&echo_tool)?],
        "tool_choice": probe_tool_choice_json(tool_choice, provider_metadata_mode),
        "temperature": 0,
        "max_tokens": 128
    });
    if let Some(parallel_tool_calls) = tool_surface_scoped_parallel_tool_calls_projection(
        1,
        effective_parallel_tool_calls(1, parallel_tool_calls_enabled, max_parallel_predictions),
    ) && let Value::Object(map) = &mut body
    {
        map.insert(
            "parallel_tool_calls".to_string(),
            Value::Bool(parallel_tool_calls),
        );
    }
    Ok(body)
}

fn probe_tool_choice_json(
    tool_choice: &ProbeToolChoice,
    provider_metadata_mode: ProviderMetadataMode,
) -> Value {
    match tool_choice {
        ProbeToolChoice::Runtime(choice) => {
            provider_tool_choice_json(choice, provider_metadata_mode)
        }
        ProbeToolChoice::Auto => json!("auto"),
    }
}

fn echo_word_tool_schema() -> ToolSchema {
    ToolSchema {
        name: "echo_word".to_string(),
        description: "Echoes a single word for provider tool-call capability probing.".to_string(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "word": {
                    "type": "string"
                }
            },
            "required": ["word"]
        }),
        strict: false,
    }
}

fn tool_call_probe_report_from_response(
    probe: &str,
    tool_choice_label: &str,
    required_for_gate: bool,
    payload: &Value,
) -> ToolCallProbeReport {
    let choice = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first());
    let finish_reason = choice.and_then(|choice| string_field(choice, &["finish_reason"]));
    let message = choice.and_then(|choice| choice.get("message"));
    let content = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let first_tool_call = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
        .and_then(|tool_calls| tool_calls.first());
    let function = first_tool_call.and_then(|tool_call| tool_call.get("function"));
    let tool_name = function.and_then(|function| string_field(function, &["name"]));
    let tool_arguments = function
        .and_then(|function| function.get("arguments"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let arguments_valid = tool_arguments
        .as_deref()
        .map(echo_word_probe_arguments_valid)
        .unwrap_or(false);
    let tool_call_received = first_tool_call.is_some();
    let passed = tool_call_received && tool_name.as_deref() == Some("echo_word") && arguments_valid;
    ToolCallProbeReport {
        probe: probe.to_string(),
        status: if passed {
            ModelAvailabilityStatus::Pass
        } else {
            ModelAvailabilityStatus::Fail
        },
        tool_choice: tool_choice_label.to_string(),
        required_for_gate,
        finish_reason,
        tool_call_received,
        tool_name,
        tool_arguments,
        arguments_valid,
        content,
        error: if passed {
            None
        } else {
            Some("expected echo_word tool call with arguments {\"word\":\"ping\"}".to_string())
        },
    }
}

fn echo_word_probe_arguments_valid(arguments: &str) -> bool {
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(arguments) else {
        return false;
    };
    object.len() == 1 && object.get("word").and_then(Value::as_str) == Some("ping")
}

pub fn model_probe_rejects_extra_tool_arguments_fixture_passes() -> bool {
    let valid_payload = json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "tool_calls": [{
                    "function": {
                        "name": "echo_word",
                        "arguments": "{\"word\":\"ping\"}"
                    }
                }]
            }
        }]
    });
    let extra_payload = json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "tool_calls": [{
                    "function": {
                        "name": "echo_word",
                        "arguments": "{\"word\":\"ping\",\"extra\":\"accepted\"}"
                    }
                }]
            }
        }]
    });
    let valid = tool_call_probe_report_from_response("fixture", "required", true, &valid_payload);
    let extra = tool_call_probe_report_from_response("fixture", "required", true, &extra_payload);

    matches!(valid.status, ModelAvailabilityStatus::Pass)
        && valid.arguments_valid
        && matches!(extra.status, ModelAvailabilityStatus::Fail)
        && !extra.arguments_valid
        && extra.tool_call_received
        && extra.tool_name.as_deref() == Some("echo_word")
}

fn failed_tool_call_probe_report(
    probe: &str,
    tool_choice_label: &str,
    required_for_gate: bool,
    error: String,
) -> ToolCallProbeReport {
    ToolCallProbeReport {
        probe: probe.to_string(),
        status: ModelAvailabilityStatus::Fail,
        tool_choice: tool_choice_label.to_string(),
        required_for_gate,
        finish_reason: None,
        tool_call_received: false,
        tool_name: None,
        tool_arguments: None,
        arguments_valid: false,
        content: None,
        error: Some(error),
    }
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

pub fn apply_model_availability_report_to_config(
    config: &mut crate::config::model::ModelConfig,
    report: &ModelAvailabilityReport,
) -> Result<(), LlmError> {
    if !matches!(report.status, ModelAvailabilityStatus::Pass) {
        return Err(LlmError::Message(format!(
            "model availability gate did not pass for `{}` at `{}`",
            report.model, report.base_url
        )));
    }
    if config.model.trim() != report.model {
        return Err(LlmError::Message(format!(
            "model availability report for `{}` cannot hydrate configured model `{}`",
            report.model, config.model
        )));
    }
    let Some(model) = report.matched_model.as_ref() else {
        return Err(LlmError::Message(format!(
            "model availability report passed without matched model metadata for `{}`",
            report.model
        )));
    };
    apply_provider_model_info_to_config(config, model);
    if report.tool_use_capable == Some(true) {
        config.supports_tools = true;
    }
    if report.vision_capable {
        config.supports_images = true;
    }
    if let Some(reasoning_capable) = report.reasoning_capable {
        config.supports_reasoning = reasoning_capable;
    }
    if let Some(context_window) = report.context {
        config.context_window = context_window;
        config.extra_body_json = Some(extra_body_with_num_ctx(
            config.extra_body_json.clone(),
            context_window,
        ));
    }
    if let Some(max_output_tokens) = report.max_output_tokens {
        config.max_output_tokens = max_output_tokens;
    }
    if let Some(max_parallel_predictions) = report.max_parallel_predictions {
        config.max_parallel_predictions = max_parallel_predictions.max(1);
    }
    Ok(())
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
                loaded: false,
                source: "openai_compat".to_string(),
            })
        })
        .collect()
}

async fn fetch_vllm_mlx_model_infos(
    client: &reqwest::Client,
    base_url: &str,
    headers: HeaderMap,
) -> Result<Vec<ProviderModelInfo>, LlmError> {
    let health_endpoint = format!("{}/health", base_url);
    let health = client
        .get(&health_endpoint)
        .headers(headers.clone())
        .send()
        .await?;
    if health.status().is_success() {
        let payload = health.json::<Value>().await?;
        let models = parse_vllm_mlx_health_model_infos(&payload);
        if !models.is_empty() {
            return Ok(models);
        }
    }

    let status_endpoint = format!("{}/v1/status", base_url);
    let status = client.get(&status_endpoint).headers(headers).send().await?;
    if !status.status().is_success() {
        return Ok(Vec::new());
    }
    let payload = status.json::<Value>().await?;
    Ok(parse_vllm_mlx_status_model_infos(&payload))
}

fn parse_vllm_mlx_health_model_infos(payload: &Value) -> Vec<ProviderModelInfo> {
    let loaded_model = string_field(payload, &["model_name", "model"]);
    let model_loaded = payload
        .get("model_loaded")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut ids = payload
        .get("available_models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if let Some(model) = loaded_model.as_ref().map(|value| value.trim().to_string()) {
        if !model.is_empty() && !ids.iter().any(|id| id == &model) {
            ids.push(model);
        }
    }
    ids.sort();
    ids.dedup();
    ids.into_iter()
        .map(|id| ProviderModelInfo {
            loaded: model_loaded
                && loaded_model
                    .as_deref()
                    .map(|model| model.trim() == id)
                    .unwrap_or(false),
            id,
            display_name: None,
            context_window: None,
            max_output_tokens: None,
            supports_images: None,
            supports_tools: None,
            supports_reasoning: None,
            max_parallel_predictions: None,
            source: "vllm_mlx_health".to_string(),
        })
        .collect()
}

fn parse_vllm_mlx_status_model_infos(payload: &Value) -> Vec<ProviderModelInfo> {
    let Some(model) = string_field(payload, &["model"]) else {
        return Vec::new();
    };
    let model = model.trim();
    if model.is_empty() {
        return Vec::new();
    }
    vec![ProviderModelInfo {
        id: model.to_string(),
        display_name: None,
        context_window: None,
        max_output_tokens: None,
        supports_images: None,
        supports_tools: None,
        supports_reasoning: None,
        max_parallel_predictions: None,
        loaded: true,
        source: "vllm_mlx_status".to_string(),
    }]
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
                })
                .or_else(|| {
                    loaded_instance.and_then(|loaded| {
                        number_field_u32(loaded, &["context_length", "contextLength"])
                    })
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
        if matches!(
            other.source.as_str(),
            "lmstudio_api" | "vllm_mlx_health" | "vllm_mlx_status"
        ) {
            self.source = other.source.clone();
        }
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
        assert!(!models[0].loaded);
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
        assert!(models[0].loaded);
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
    }

    #[test]
    fn vllm_mlx_health_parser_marks_loaded_model_without_request_limits() {
        let payload = serde_json::json!({
            "status": "healthy",
            "model_loaded": true,
            "model_name": "openai-compatible-fixture-model",
            "available_models": ["openai-compatible-fixture-model"],
            "engine_type": "batched"
        });

        let models = parse_vllm_mlx_health_model_infos(&payload);

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "openai-compatible-fixture-model");
        assert!(models[0].loaded);
        assert_eq!(models[0].context_window, None);
        assert_eq!(models[0].max_output_tokens, None);
    }

    #[test]
    fn openai_compatible_only_availability_does_not_require_native_metadata() {
        assert!(model_availability_passes(
            ProviderMetadataMode::OpenAiCompatibleOnly,
            true,
            false,
            false,
            false,
            false,
            true
        ));
        assert!(!model_availability_passes(
            ProviderMetadataMode::LmStudioNativeRequired,
            true,
            false,
            false,
            false,
            false,
            true
        ));
        assert!(!model_availability_passes(
            ProviderMetadataMode::OpenAiCompatibleOnly,
            true,
            false,
            true,
            false,
            false,
            true
        ));
        assert!(model_availability_passes(
            ProviderMetadataMode::OpenAiCompatibleOnly,
            true,
            false,
            true,
            true,
            true,
            true
        ));
        assert!(!model_availability_passes(
            ProviderMetadataMode::OpenAiCompatibleOnly,
            true,
            false,
            false,
            false,
            false,
            false
        ));
    }

    #[test]
    fn tool_call_probe_report_accepts_openai_tool_calls() {
        let payload = serde_json::json!({
            "choices": [
                {
                    "finish_reason": "tool_calls",
                    "message": {
                        "role": "assistant",
                        "tool_calls": [
                            {
                                "id": "call_1",
                                "type": "function",
                                "function": {
                                    "name": "echo_word",
                                    "arguments": "{\"word\":\"ping\"}"
                                }
                            }
                        ]
                    }
                }
            ]
        });

        let report = tool_call_probe_report_from_response(
            "tool_choice_required",
            "required",
            true,
            &payload,
        );

        assert_eq!(report.status, ModelAvailabilityStatus::Pass);
        assert!(report.required_for_gate);
        assert!(report.tool_call_received);
        assert_eq!(report.tool_name.as_deref(), Some("echo_word"));
        assert!(report.arguments_valid);
        assert_eq!(report.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn tool_call_probe_report_rejects_plain_text_answers() {
        let payload = serde_json::json!({
            "choices": [
                {
                    "finish_reason": "stop",
                    "message": {
                        "role": "assistant",
                        "content": "ping"
                    }
                }
            ]
        });

        let report =
            tool_call_probe_report_from_response("tool_choice_auto_strong", "auto", true, &payload);

        assert_eq!(report.status, ModelAvailabilityStatus::Fail);
        assert!(!report.tool_call_received);
        assert_eq!(report.content.as_deref(), Some("ping"));
        assert!(report.error.is_some());
    }

    #[test]
    fn lm_studio_availability_gates_on_required_and_auto_named_probe_is_optional() {
        let required = ToolCallProbeReport {
            probe: "tool_choice_required".to_string(),
            status: ModelAvailabilityStatus::Pass,
            tool_choice: "required".to_string(),
            required_for_gate: true,
            finish_reason: Some("tool_calls".to_string()),
            tool_call_received: true,
            tool_name: Some("echo_word".to_string()),
            tool_arguments: Some("{\"word\":\"ping\"}".to_string()),
            arguments_valid: true,
            content: None,
            error: None,
        };
        let named = ToolCallProbeReport {
            probe: "tool_choice_named".to_string(),
            status: ModelAvailabilityStatus::Pass,
            tool_choice: "named_function".to_string(),
            required_for_gate: false,
            finish_reason: Some("tool_calls".to_string()),
            tool_call_received: true,
            tool_name: Some("echo_word".to_string()),
            tool_arguments: Some("{\"word\":\"ping\"}".to_string()),
            arguments_valid: true,
            content: None,
            error: None,
        };
        let auto = ToolCallProbeReport {
            probe: "tool_choice_auto_strong".to_string(),
            status: ModelAvailabilityStatus::Pass,
            tool_choice: "auto".to_string(),
            required_for_gate: true,
            finish_reason: Some("tool_calls".to_string()),
            tool_call_received: true,
            tool_name: Some("echo_word".to_string()),
            tool_arguments: Some("{\"word\":\"ping\"}".to_string()),
            arguments_valid: true,
            content: None,
            error: None,
        };
        let reports = [required, named, auto];

        assert!(
            !reports.is_empty()
                && reports
                    .iter()
                    .filter(|probe| probe.required_for_gate)
                    .all(|probe| matches!(probe.status, ModelAvailabilityStatus::Pass))
        );
    }
}
