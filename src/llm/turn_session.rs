use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::LlmError;

use super::{ChatRequest, ModelMessage, ResponsesContinuation};

/// Describes how canonical model history changed since the last request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryUpdateKind {
    Unchanged,
    AppendOnly,
    Compacted,
    Rewritten,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundResponsesCursor {
    continuation: ResponsesContinuation,
    request_fingerprint: String,
    history_revision: String,
}

/// Turn-scoped owner for Responses continuity and its one-shot recovery path.
///
/// The session does not own network deadlines. Transport timeout and stream-idle
/// behavior remain properties of `ChatRequest` and the provider transport.
#[derive(Debug, Clone)]
pub struct LlmTurnSession {
    cursor: Option<BoundResponsesCursor>,
    request_fingerprint: Option<String>,
    history_revision: String,
    full_history_fallback_used: bool,
}

impl LlmTurnSession {
    pub fn new(canonical_history_revision: impl Into<String>) -> Self {
        Self {
            cursor: None,
            request_fingerprint: None,
            history_revision: canonical_history_revision.into(),
            full_history_fallback_used: false,
        }
    }

    #[cfg(test)]
    fn responses_continuation(&self) -> Option<&ResponsesContinuation> {
        self.cursor.as_ref().map(|cursor| &cursor.continuation)
    }

    #[cfg(test)]
    fn full_history_fallback_used(&self) -> bool {
        self.full_history_fallback_used
    }

    /// Applies a valid same-turn cursor to `request`, or explicitly clears a
    /// stale/caller-supplied cursor. A fingerprint or history revision mismatch
    /// is a continuity boundary, not a recoverable cursor reuse case.
    pub fn prepare_request(
        &mut self,
        request: &mut ChatRequest,
        canonical_history_revision: &str,
    ) -> Result<bool, LlmError> {
        let fingerprint = chat_request_fingerprint(request)?;
        self.align_identity(&fingerprint, canonical_history_revision);

        let continuation = self
            .cursor
            .as_ref()
            .filter(|cursor| {
                cursor.request_fingerprint == fingerprint
                    && cursor.history_revision == canonical_history_revision
            })
            .map(|cursor| cursor.continuation.clone());
        request.responses_continuation = continuation;
        Ok(request.responses_continuation.is_some())
    }

    /// Records the cursor returned by a successful Responses request. The
    /// cursor represents every non-system input item plus the provider response
    /// itself; later append-only tool outputs remain new input after this index.
    pub fn record_response(
        &mut self,
        request: &ChatRequest,
        canonical_history_revision: &str,
        response_id: Option<String>,
    ) -> Result<(), LlmError> {
        let fingerprint = chat_request_fingerprint(request)?;
        self.align_identity(&fingerprint, canonical_history_revision);

        let response_id = response_id
            .filter(|response_id| !response_id.trim().is_empty())
            .filter(|_| {
                request.provider_target().api_mode()
                    == crate::config::model::ProviderApiMode::Responses
            });
        self.cursor = response_id.map(|previous_response_id| BoundResponsesCursor {
            continuation: ResponsesContinuation {
                previous_response_id,
                input_start: non_system_message_count(&request.messages).saturating_add(1),
            },
            request_fingerprint: fingerprint,
            history_revision: canonical_history_revision.to_string(),
        });
        Ok(())
    }

    /// Advances the canonical revision and preserves a cursor only for a
    /// declared append-only update. Compaction and rewrite always sever server
    /// cursor lineage. An `Unchanged` claim with a different revision is also
    /// treated as a mismatch and invalidates continuity.
    pub fn update_history(
        &mut self,
        update: HistoryUpdateKind,
        canonical_history_revision: impl Into<String>,
    ) {
        let canonical_history_revision = canonical_history_revision.into();
        match update {
            HistoryUpdateKind::Unchanged => {
                if self.history_revision != canonical_history_revision {
                    self.cursor = None;
                    self.history_revision = canonical_history_revision;
                }
            }
            HistoryUpdateKind::AppendOnly => {
                self.history_revision = canonical_history_revision.clone();
                if let Some(cursor) = &mut self.cursor {
                    cursor.history_revision = canonical_history_revision;
                }
            }
            HistoryUpdateKind::Compacted | HistoryUpdateKind::Rewritten => {
                self.history_revision = canonical_history_revision;
                self.cursor = None;
            }
        }
    }

    /// Returns one full-history retry request only for a typed provider
    /// rejection of the cursor used by `failed_request`.
    pub fn full_history_retry_after_rejection(
        &mut self,
        failed_request: &ChatRequest,
        error: &LlmError,
    ) -> Option<ChatRequest> {
        if self.full_history_fallback_used
            || failed_request.responses_continuation.is_none()
            || !error.rejects_previous_response_id()
        {
            return None;
        }

        self.full_history_fallback_used = true;
        self.cursor = None;
        let mut retry = failed_request.clone();
        retry.responses_continuation = None;
        Some(retry)
    }

    pub fn invalidate_cursor(&mut self) {
        self.cursor = None;
    }

    fn align_identity(&mut self, fingerprint: &str, canonical_history_revision: &str) {
        let fingerprint_matches = self
            .request_fingerprint
            .as_deref()
            .is_none_or(|stored| stored == fingerprint);
        let revision_matches = self.history_revision == canonical_history_revision;
        if !fingerprint_matches || !revision_matches {
            self.cursor = None;
        }
        self.request_fingerprint = Some(fingerprint.to_string());
        self.history_revision = canonical_history_revision.to_string();
    }
}

/// Hashes an explicit provider-semantic projection. Canonical history messages,
/// the Responses cursor, transport endpoint/headers, and timing/retry policy are
/// excluded: history is guarded by its own revision and transport policy does
/// not affect server-side response lineage.
fn chat_request_fingerprint(request: &ChatRequest) -> Result<String, LlmError> {
    let canonical = canonical_json(chat_request_semantic_projection(request)?);
    let encoded = serde_json::to_vec(&canonical)?;
    let digest = Sha256::digest(encoded);
    let mut fingerprint = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut fingerprint, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(fingerprint)
}

#[derive(Serialize)]
struct ChatRequestSemanticProjection<'a> {
    model: &'a super::ModelProfile,
    system_prompt: &'a str,
    tools: &'a [super::ToolSchema],
    provider_api_mode: crate::config::model::ProviderApiMode,
    reasoning: &'a Option<super::ReasoningRequest>,
    reasoning_capability: crate::config::model::ProviderReasoningCapability,
    tool_choice: &'a Option<super::ProviderToolChoice>,
    parallel_tool_calls: bool,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<u32>,
    presence_penalty: Option<f64>,
    frequency_penalty: Option<f64>,
    seed: Option<u64>,
    stop_sequences: &'a [String],
    extra_body: &'a Option<Value>,
}

fn chat_request_semantic_projection(request: &ChatRequest) -> Result<Value, LlmError> {
    Ok(serde_json::to_value(ChatRequestSemanticProjection {
        model: &request.model,
        system_prompt: &request.system_prompt,
        tools: &request.tools,
        provider_api_mode: request.provider_target().api_mode(),
        reasoning: &request.reasoning,
        reasoning_capability: request.reasoning_capability,
        tool_choice: &request.tool_choice,
        parallel_tool_calls: request.parallel_tool_calls,
        temperature: request.temperature,
        top_p: request.top_p,
        top_k: request.top_k,
        presence_penalty: request.presence_penalty,
        frequency_penalty: request.frequency_penalty,
        seed: request.seed,
        stop_sequences: &request.stop_sequences,
        extra_body: &request.extra_body,
    })?)
}

fn canonical_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
        Value::Object(object) => {
            let mut entries = object.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonical_json(value)))
                    .collect(),
            )
        }
        scalar => scalar,
    }
}

fn non_system_message_count(messages: &[ModelMessage]) -> usize {
    messages
        .iter()
        .filter(|message| !matches!(message, ModelMessage::System { .. }))
        .count()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use crate::config::model::{
        ProviderApiMode, ProviderReasoningCapability, ReasoningEffort, ReasoningSummary,
    };
    use crate::config::{ProviderDeadlines, ProviderMetadataMode, ProviderTarget};
    use crate::llm::{
        ModelCapabilities, ModelProfile, ProviderToolChoice, ReasoningRequest, ToolSchema,
    };

    use super::*;

    fn provider_target(
        endpoint: &str,
        api_mode: ProviderApiMode,
        deadlines: ProviderDeadlines,
    ) -> ProviderTarget {
        ProviderTarget::new(
            endpoint,
            "model",
            ProviderMetadataMode::OpenAiCompatibleOnly,
            api_mode,
            deadlines,
        )
        .expect("provider target")
    }

    fn request() -> ChatRequest {
        let model = ModelProfile {
            name: "model".to_string(),
            context_window: 32_768,
            max_output_tokens: 4_096,
            provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
            capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: true,
                supports_images: false,
            },
        };
        let provider = provider_target(
            "http://localhost:1234",
            ProviderApiMode::Responses,
            ProviderDeadlines {
                response_start_timeout_ms: 600_000,
                stream_idle_timeout_ms: 60_000,
                connect_timeout_ms: 10_000,
                max_connect_retries: 2,
            },
        );
        let mut request = ChatRequest::new(
            provider,
            model,
            "stable instructions".to_string(),
            vec![ModelMessage::User {
                content: "first input".to_string(),
            }],
            vec![ToolSchema {
                name: "read".to_string(),
                description: "read a file".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "offset": {"type": "integer"}
                    }
                }),
            }],
            Some(ReasoningRequest {
                effort: Some(ReasoningEffort::Medium),
                summary: ReasoningSummary::Concise,
            }),
            ProviderReasoningCapability::Responses {
                supports_summary: true,
                supports_previous_response_id: true,
            },
            BTreeMap::from([("x-provider".to_string(), "test".to_string())]),
        );
        request.tool_choice = Some(ProviderToolChoice::Required);
        request.temperature = Some(0.2);
        request.top_p = Some(0.9);
        request.seed = Some(7);
        request.stop_sequences = vec!["stop".to_string()];
        request.extra_body = Some(json!({"nested": {"b": 2, "a": 1}}));
        request
    }

    #[test]
    fn fingerprint_excludes_history_cursor_and_transport_timing() {
        let first = request();
        let expected = chat_request_fingerprint(&first).expect("fingerprint");
        let mut equivalent = first.clone();
        equivalent.messages.push(ModelMessage::Assistant {
            content: "prior output".to_string(),
        });
        equivalent.responses_continuation = Some(ResponsesContinuation {
            previous_response_id: "resp_old".to_string(),
            input_start: 2,
        });
        equivalent.replace_provider_target(provider_target(
            "http://localhost:1234",
            ProviderApiMode::Responses,
            ProviderDeadlines {
                response_start_timeout_ms: 1,
                stream_idle_timeout_ms: 2,
                connect_timeout_ms: 3,
                max_connect_retries: 4,
            },
        ));

        assert_eq!(
            chat_request_fingerprint(&equivalent).expect("equivalent fingerprint"),
            expected
        );

        equivalent.system_prompt.push_str(" changed");
        assert_ne!(
            chat_request_fingerprint(&equivalent).expect("changed fingerprint"),
            expected
        );
    }

    #[test]
    fn transport_endpoint_headers_and_secrets_are_absent_from_fingerprint_projection() {
        let mut first = request();
        first.replace_provider_target(provider_target(
            "https://provider.example/v1",
            ProviderApiMode::Responses,
            ProviderDeadlines {
                response_start_timeout_ms: 600_000,
                stream_idle_timeout_ms: 60_000,
                connect_timeout_ms: 10_000,
                max_connect_retries: 2,
            },
        ));
        first.replace_extra_headers(BTreeMap::from([
            (
                "authorization".to_string(),
                "Bearer header-secret".to_string(),
            ),
            ("x-provider-key".to_string(), "another-secret".to_string()),
        ]));
        first.responses_continuation = Some(ResponsesContinuation {
            previous_response_id: "response-id-secret".to_string(),
            input_start: 0,
        });

        let debug = format!("{first:?}");
        for secret in ["header-secret", "another-secret", "response-id-secret"] {
            assert!(!debug.contains(secret));
        }

        let projection = serde_json::to_string(
            &chat_request_semantic_projection(&first).expect("semantic projection"),
        )
        .expect("serialize semantic projection");
        for secret in ["header-secret", "another-secret", "response-id-secret"] {
            assert!(!projection.contains(secret));
        }

        let expected = chat_request_fingerprint(&first).expect("fingerprint");
        let mut different_transport = first.clone();
        different_transport.replace_provider_target(provider_target(
            "http://different-provider.invalid",
            ProviderApiMode::Responses,
            ProviderDeadlines {
                response_start_timeout_ms: 1,
                stream_idle_timeout_ms: 2,
                connect_timeout_ms: 3,
                max_connect_retries: 4,
            },
        ));
        different_transport.replace_extra_headers(BTreeMap::from([(
            "authorization".to_string(),
            "Bearer different-secret".to_string(),
        )]));
        assert_eq!(
            chat_request_fingerprint(&different_transport).expect("transport-independent hash"),
            expected
        );
    }

    #[test]
    fn canonical_json_order_does_not_change_fingerprint() {
        let first = request();
        let mut reordered = first.clone();
        reordered.extra_body = Some(json!({"nested": {"a": 1, "b": 2}}));
        assert_eq!(
            chat_request_fingerprint(&first).expect("first"),
            chat_request_fingerprint(&reordered).expect("reordered")
        );
    }

    #[test]
    fn append_only_history_advances_revision_and_keeps_cursor() {
        let mut session = LlmTurnSession::new("rev-0");
        let mut first = request();
        assert!(
            !session
                .prepare_request(&mut first, "rev-0")
                .expect("prepare first")
        );
        session
            .record_response(&first, "rev-0", Some("resp-1".to_string()))
            .expect("record response");
        assert_eq!(
            session.responses_continuation(),
            Some(&ResponsesContinuation {
                previous_response_id: "resp-1".to_string(),
                input_start: 2,
            })
        );

        session.update_history(HistoryUpdateKind::AppendOnly, "rev-1");
        let mut next = request();
        next.messages.extend([
            ModelMessage::AssistantToolCalls {
                content: None,
                tool_calls: Vec::new(),
            },
            ModelMessage::Tool {
                call_id: "call-1".to_string(),
                tool_name: "read".to_string(),
                result: "result".to_string(),
                metadata: Value::Null,
            },
        ]);
        assert!(
            session
                .prepare_request(&mut next, "rev-1")
                .expect("prepare continuation")
        );
        assert_eq!(
            next.responses_continuation,
            Some(ResponsesContinuation {
                previous_response_id: "resp-1".to_string(),
                input_start: 2,
            })
        );
    }

    #[test]
    fn compaction_rewrite_and_revision_mismatch_invalidate_cursor() {
        for update in [HistoryUpdateKind::Compacted, HistoryUpdateKind::Rewritten] {
            let mut session = session_with_cursor();
            session.update_history(update, "rev-1");
            assert!(session.responses_continuation().is_none());
        }

        let mut session = session_with_cursor();
        let mut request = request();
        assert!(
            !session
                .prepare_request(&mut request, "unexpected-revision")
                .expect("mismatched revision")
        );
        assert!(session.responses_continuation().is_none());

        let mut session = session_with_cursor();
        session.update_history(HistoryUpdateKind::Unchanged, "unexpected-revision");
        assert!(session.responses_continuation().is_none());
    }

    #[test]
    fn request_fingerprint_mismatch_invalidates_cursor() {
        let mut session = session_with_cursor();
        let mut changed = request();
        changed.tools[0].description = "changed schema contract".to_string();

        assert!(
            !session
                .prepare_request(&mut changed, "rev-0")
                .expect("mismatched request")
        );
        assert!(session.responses_continuation().is_none());
    }

    #[test]
    fn cursor_rejection_allows_exactly_one_full_history_retry() {
        let mut session = session_with_cursor();
        let mut failed = request();
        assert!(
            session
                .prepare_request(&mut failed, "rev-0")
                .expect("cursor request")
        );
        let rejection = LlmError::ProviderRejected {
            status: Some(400),
            code: Some("invalid_previous_response_id".to_string()),
            param: Some("previous_response_id".to_string()),
            message: "previous response expired".to_string(),
        };

        let retry = session
            .full_history_retry_after_rejection(&failed, &rejection)
            .expect("one full-history retry");
        assert!(retry.responses_continuation.is_none());
        assert_eq!(retry.messages.len(), failed.messages.len());
        assert!(session.full_history_fallback_used());
        assert!(session.responses_continuation().is_none());
        assert!(
            session
                .full_history_retry_after_rejection(&failed, &rejection)
                .is_none()
        );
    }

    #[test]
    fn unrelated_provider_error_does_not_consume_fallback() {
        let mut session = session_with_cursor();
        let mut failed = request();
        session
            .prepare_request(&mut failed, "rev-0")
            .expect("cursor request");
        let unrelated = LlmError::ProviderRejected {
            status: Some(400),
            code: Some("invalid_request".to_string()),
            param: Some("tools".to_string()),
            message: "invalid tool schema".to_string(),
        };

        assert!(
            session
                .full_history_retry_after_rejection(&failed, &unrelated)
                .is_none()
        );
        assert!(!session.full_history_fallback_used());
    }

    #[test]
    fn non_responses_and_blank_response_ids_do_not_create_cursor() {
        let mut session = LlmTurnSession::new("rev-0");
        let mut chat_completions = request();
        chat_completions.replace_provider_target(provider_target(
            "http://localhost:1234",
            ProviderApiMode::ChatCompletions,
            ProviderDeadlines {
                response_start_timeout_ms: 600_000,
                stream_idle_timeout_ms: 60_000,
                connect_timeout_ms: 10_000,
                max_connect_retries: 2,
            },
        ));
        chat_completions.reasoning = None;
        chat_completions.reasoning_capability = ProviderReasoningCapability::Unsupported;
        session
            .record_response(&chat_completions, "rev-0", Some("resp-ignored".to_string()))
            .expect("record chat completions");
        assert!(session.responses_continuation().is_none());

        let responses = request();
        session
            .record_response(&responses, "rev-0", Some("   ".to_string()))
            .expect("record blank id");
        assert!(session.responses_continuation().is_none());
    }

    fn session_with_cursor() -> LlmTurnSession {
        let mut session = LlmTurnSession::new("rev-0");
        let request = request();
        session
            .record_response(&request, "rev-0", Some("resp-1".to_string()))
            .expect("cursor");
        session
    }
}
