use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

use crate::config::model::ProviderApiMode;
use crate::error::LlmError;
use crate::llm::ProviderRequestId;
use crate::llm::contract::ChatRequest;
use crate::llm::openai_compat::to_openai_request_with_reasoning;
use crate::llm::responses::{ResponsesRequestOptions, to_responses_request};
use crate::session::RequestWireDiagnostic;

const HTTP_REQUEST_CAPTURE_DIR_ENV: &str = "MOYAI_HTTP_REQUEST_CAPTURE_DIR";
static HTTP_REQUEST_CAPTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Derives redacted diagnostics from the same provider DTO and bounded
/// serialization used by the HTTP transport. The request body itself is never
/// retained in session or harness state.
pub(crate) fn http_request_wire_diagnostic(
    request: &ChatRequest,
) -> Result<RequestWireDiagnostic, LlmError> {
    let (api_mode, input_kind, input_key, body) = match request.provider_target().api_mode() {
        ProviderApiMode::ChatCompletions => (
            "chat_completions",
            "messages",
            "messages",
            to_openai_request_with_reasoning(
                request,
                request.reasoning.as_ref(),
                request.reasoning_capability,
            )?,
        ),
        ProviderApiMode::Responses => (
            "responses",
            "input_items",
            "input",
            to_responses_request(request, ResponsesRequestOptions::from_request(request))?,
        ),
    };
    let input_count = body
        .get(input_key)
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| {
            LlmError::Message(format!(
                "serialized {api_mode} request is missing its `{input_key}` array"
            ))
        })?;
    let continuation_present = body.get("previous_response_id").is_some();
    let serialized_body_bytes = u64::try_from(request.serialize_wire_body(&body)?.len())
        .map_err(|_| LlmError::Message("serialized request size exceeds u64".to_string()))?;

    Ok(RequestWireDiagnostic {
        transport: "http".to_string(),
        api_mode: api_mode.to_string(),
        input_kind: input_kind.to_string(),
        input_count,
        serialized_body_bytes,
        continuation_present,
    })
}

/// Opt-in, task-local capture of the exact prepared outbound HTTP request DTO.
///
/// Full prompts can contain workspace content and user secrets, so this is
/// deliberately disabled unless an absolute capture directory is supplied via
/// `MOYAI_HTTP_REQUEST_CAPTURE_DIR`. The capture does not prove that a network
/// attempt started or that the provider received the body. Its request ID joins
/// it to the runtime provider-phase events that own attempt and outcome facts.
/// Write failures abort request preparation instead of silently omitting the
/// configured evidence.
pub(crate) fn capture_http_request_wire_body(
    request_id: &ProviderRequestId,
    api_mode: ProviderApiMode,
    endpoint_path: &str,
    body: &[u8],
) -> Result<(), LlmError> {
    let Some(directory) = std::env::var_os(HTTP_REQUEST_CAPTURE_DIR_ENV) else {
        return Ok(());
    };
    let directory = PathBuf::from(directory);
    if !directory.is_absolute() {
        return Err(LlmError::Message(format!(
            "{HTTP_REQUEST_CAPTURE_DIR_ENV} must be an absolute directory"
        )));
    }
    capture_http_request_wire_body_in(&directory, request_id, api_mode, endpoint_path, body)
}

fn capture_http_request_wire_body_in(
    directory: &Path,
    request_id: &ProviderRequestId,
    api_mode: ProviderApiMode,
    endpoint_path: &str,
    body: &[u8],
) -> Result<(), LlmError> {
    prepare_capture_directory(directory)?;
    let sequence = HTTP_REQUEST_CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let mode = match api_mode {
        ProviderApiMode::ChatCompletions => "chat_completions",
        ProviderApiMode::Responses => "responses",
    };
    let stem = format!(
        "{timestamp_ms:020}-{:010}-{sequence:010}-{mode}",
        std::process::id()
    );
    let body_path = directory.join(format!("{stem}.request.json"));
    write_new_capture_file(&body_path, body)?;
    let metadata = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 2,
        "transport": "http",
        "capture_stage": "prepared",
        "request_id": request_id.as_str(),
        "captured_at_unix_ms": timestamp_ms,
        "process_id": std::process::id(),
        "sequence": sequence,
        "api_mode": mode,
        "endpoint_path": endpoint_path,
        "request_body_file": body_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
        "request_body_bytes": body.len(),
    }))
    .map_err(|error| {
        LlmError::Message(format!(
            "failed to encode request capture metadata: {error}"
        ))
    })?;
    write_new_capture_file(&directory.join(format!("{stem}.metadata.json")), &metadata)
}

fn prepare_capture_directory(directory: &Path) -> Result<(), LlmError> {
    #[cfg(unix)]
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(directory).map_err(|error| {
            LlmError::Message(format!(
                "failed to create HTTP request capture directory `{}`: {error}",
                directory.display()
            ))
        })?;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                LlmError::Message(format!(
                    "failed to secure HTTP request capture directory `{}`: {error}",
                    directory.display()
                ))
            },
        )?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(directory).map_err(|error| {
            LlmError::Message(format!(
                "failed to create HTTP request capture directory `{}`: {error}",
                directory.display()
            ))
        })?;
    }
    Ok(())
}

fn write_new_capture_file(path: &Path, bytes: &[u8]) -> Result<(), LlmError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).map_err(|error| {
        LlmError::Message(format!(
            "failed to create HTTP request capture `{}`: {error}",
            path.display()
        ))
    })?;
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| {
            LlmError::Message(format!(
                "failed to secure HTTP request capture `{}`: {error}",
                path.display()
            ))
        })?;
    file.write_all(bytes).map_err(|error| {
        LlmError::Message(format!(
            "failed to write HTTP request capture `{}`: {error}",
            path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        LlmError::Message(format!(
            "failed to flush HTTP request capture `{}`: {error}",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::Value;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::config::model::{ProviderApiMode, ProviderReasoningCapability};
    use crate::config::{ProviderDeadlines, ProviderMetadataMode, ProviderTarget};
    use crate::llm::{ModelCapabilities, ModelMessage, ModelProfile, ModelToolCall};

    #[test]
    fn chat_completions_diagnostics_measure_the_exact_wire_messages_and_body() {
        let request = request(
            ProviderApiMode::ChatCompletions,
            vec![
                ModelMessage::System {
                    content: "additional policy".to_string(),
                },
                ModelMessage::User {
                    content: "wire-payload-secret".to_string(),
                },
                ModelMessage::Assistant {
                    content: "done".to_string(),
                },
            ],
        );

        let diagnostics = http_request_wire_diagnostic(&request).expect("wire diagnostics");
        let body = to_openai_request_with_reasoning(
            &request,
            request.reasoning.as_ref(),
            request.reasoning_capability,
        )
        .expect("chat request body");
        let expected_bytes = request
            .serialize_wire_body(&body)
            .expect("serialized chat body")
            .len() as u64;

        assert_eq!(diagnostics.api_mode, "chat_completions");
        assert_eq!(diagnostics.input_kind, "messages");
        assert_eq!(diagnostics.input_count, 3);
        assert_eq!(diagnostics.serialized_body_bytes, expected_bytes);
        assert!(!diagnostics.continuation_present);
        assert!(
            !serde_json::to_string(&diagnostics)
                .expect("serialized diagnostics")
                .contains("wire-payload-secret")
        );
    }

    #[test]
    fn responses_diagnostics_count_expanded_input_items_without_a_cursor() {
        let request = request(
            ProviderApiMode::Responses,
            vec![
                ModelMessage::User {
                    content: "inspect the source".to_string(),
                },
                ModelMessage::AssistantToolCalls {
                    content: Some("I will inspect it.".to_string()),
                    tool_calls: vec![ModelToolCall {
                        call_id: "call-1".to_string(),
                        tool_name: "read".to_string(),
                        arguments_json: r#"{"path":"src/main.rs"}"#.to_string(),
                    }],
                },
                ModelMessage::Tool {
                    call_id: "call-1".to_string(),
                    tool_name: "read".to_string(),
                    result: "source".to_string(),
                    metadata: Value::Null,
                },
            ],
        );

        let diagnostics = http_request_wire_diagnostic(&request).expect("wire diagnostics");
        let body = to_responses_request(&request, ResponsesRequestOptions::from_request(&request))
            .expect("Responses request body");
        let expected_bytes = request
            .serialize_wire_body(&body)
            .expect("serialized Responses body")
            .len() as u64;

        assert_eq!(diagnostics.api_mode, "responses");
        assert_eq!(diagnostics.input_kind, "input_items");
        assert_eq!(diagnostics.input_count, 4);
        assert_eq!(diagnostics.serialized_body_bytes, expected_bytes);
        assert!(!diagnostics.continuation_present);
    }

    #[test]
    fn explicit_wire_capture_writes_exact_body_and_separate_metadata() {
        let directory = tempfile::tempdir().expect("capture tempdir");
        let body = br#"{"messages":[{"role":"user","content":"full prompt"}]}"#;
        let request_id = ProviderRequestId::new();

        capture_http_request_wire_body_in(
            directory.path(),
            &request_id,
            ProviderApiMode::ChatCompletions,
            "v1/chat/completions",
            body,
        )
        .expect("capture");

        let mut entries = std::fs::read_dir(directory.path())
            .expect("capture entries")
            .map(|entry| entry.expect("entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(entries.len(), 2);
        let request_path = entries
            .iter()
            .find(|path| path.to_string_lossy().ends_with(".request.json"))
            .expect("request body");
        let metadata_path = entries
            .iter()
            .find(|path| path.to_string_lossy().ends_with(".metadata.json"))
            .expect("metadata");
        assert_eq!(std::fs::read(request_path).expect("request bytes"), body);
        let metadata: Value =
            serde_json::from_slice(&std::fs::read(metadata_path).expect("metadata bytes"))
                .expect("metadata json");
        assert_eq!(metadata["api_mode"], "chat_completions");
        assert_eq!(metadata["endpoint_path"], "v1/chat/completions");
        assert_eq!(metadata["request_body_bytes"], body.len());
        assert_eq!(metadata["schema_version"], 2);
        assert_eq!(metadata["transport"], "http");
        assert_eq!(metadata["capture_stage"], "prepared");
        assert_eq!(metadata["request_id"], request_id.as_str());
    }

    #[cfg(unix)]
    #[test]
    fn explicit_wire_capture_enforces_owner_only_unix_permissions() {
        let root = tempfile::tempdir().expect("capture root");
        let directory = root.path().join("capture");
        std::fs::create_dir(&directory).expect("permissive capture directory");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o777))
            .expect("permissive directory mode");

        capture_http_request_wire_body_in(
            &directory,
            &ProviderRequestId::new(),
            ProviderApiMode::Responses,
            "v1/responses",
            br#"{"input":[]}"#,
        )
        .expect("secure capture");

        assert_eq!(
            std::fs::metadata(&directory)
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for entry in std::fs::read_dir(&directory).expect("capture entries") {
            let path = entry.expect("capture entry").path();
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("capture metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "capture file was not owner-only: {}",
                path.display()
            );
        }
    }

    fn request(api_mode: ProviderApiMode, messages: Vec<ModelMessage>) -> ChatRequest {
        let model = ModelProfile {
            name: "wire-diagnostics-model".to_string(),
            context_window: 131_072,
            max_output_tokens: 8_192,
            provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
            capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: true,
                supports_images: false,
            },
        };
        let provider = ProviderTarget::new(
            "http://provider.fixture.invalid/v1",
            &model.name,
            model.provider_metadata_mode,
            api_mode,
            ProviderDeadlines {
                response_start_timeout_ms: 30_000,
                stream_idle_timeout_ms: 30_000,
                connect_timeout_ms: 1_000,
                max_connect_retries: 0,
            },
        )
        .expect("provider target");
        let reasoning_capability = match api_mode {
            ProviderApiMode::ChatCompletions => ProviderReasoningCapability::Unsupported,
            ProviderApiMode::Responses => ProviderReasoningCapability::Responses {
                supports_summary: true,
            },
        };

        ChatRequest::new(
            provider,
            model,
            "base instructions".to_string(),
            messages,
            Vec::new(),
            None,
            reasoning_capability,
            BTreeMap::new(),
        )
    }
}
