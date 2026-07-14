use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::config::ResolvedConfig;
use crate::config::model::{ProviderApiMode, ProviderReasoningCapability};
use crate::error::LlmError;
use crate::llm::{ChatRequest, LlmClient, LlmEvent, LlmEventSink, ModelMessage, ModelProfile};
use crate::tool::PermissionRequest;

const SYSTEM_PROMPT: &str = include_str!("../../assets/prompts/permission_reviewer.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionReviewRisk {
    Low,
    Medium,
    High,
    Critical,
}

impl PermissionReviewRisk {
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionReviewUserAuthorization {
    Unknown,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionReviewOutcome {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionReviewDecision {
    pub risk_level: PermissionReviewRisk,
    pub user_authorization: PermissionReviewUserAuthorization,
    pub outcome: PermissionReviewOutcome,
    pub rationale: String,
}

#[derive(Debug, Deserialize)]
struct PermissionReviewPayload {
    risk_level: Option<PermissionReviewRisk>,
    user_authorization: Option<PermissionReviewUserAuthorization>,
    outcome: PermissionReviewOutcome,
    rationale: Option<String>,
}

impl PermissionReviewDecision {
    pub fn allows(&self) -> bool {
        self.outcome == PermissionReviewOutcome::Allow
    }
}

pub struct PermissionReviewer<'a> {
    pub llm: &'a dyn LlmClient,
    pub model: &'a ModelProfile,
    pub config: &'a ResolvedConfig,
    pub request_gate: Option<Arc<Semaphore>>,
}

impl PermissionReviewer<'_> {
    pub async fn review(
        &self,
        task_context: &str,
        request: &PermissionRequest,
        cancel: CancellationToken,
    ) -> Result<PermissionReviewDecision, LlmError> {
        let request_json = serde_json::to_string_pretty(request)?;
        let user_message = format!(
            "<task_context>\n{task_context}\n</task_context>\n\n<permission_request>\n{request_json}\n</permission_request>"
        );
        let chat_request = ChatRequest {
            model: self.model.clone(),
            base_url: self.config.model.base_url.clone(),
            system_prompt: SYSTEM_PROMPT.trim().to_string(),
            messages: vec![ModelMessage::User {
                content: user_message,
            }],
            tools: Vec::new(),
            provider_api_mode: ProviderApiMode::ChatCompletions,
            reasoning: None,
            reasoning_capability: ProviderReasoningCapability::Unsupported,
            responses_continuation: None,
            tool_choice: None,
            parallel_tool_calls: false,
            timeout_ms: self.config.model.request_timeout_ms,
            stream_idle_timeout_ms: self.config.model.stream_idle_timeout_ms,
            stream_max_retries: self.config.model.stream_max_retries,
            extra_headers: self.config.model.extra_headers.clone(),
            temperature: Some(0.0),
            top_p: self.config.model.top_p,
            top_k: self.config.model.top_k,
            presence_penalty: self.config.model.presence_penalty,
            frequency_penalty: self.config.model.frequency_penalty,
            seed: self.config.model.seed,
            stop_sequences: self.config.model.stop_sequences.clone(),
            extra_body: self.config.model.extra_body_json.clone(),
        };
        chat_request.validate_provider_lifecycle()?;

        let _permit = match self.request_gate.clone() {
            Some(gate) => {
                let acquire = gate.acquire_owned();
                tokio::pin!(acquire);
                tokio::select! {
                    permit = &mut acquire => Some(permit.map_err(|_| {
                        LlmError::Message("permission reviewer model request gate closed".to_string())
                    })?),
                    _ = cancel.cancelled() => return Err(LlmError::Message(
                        "permission review cancelled".to_string(),
                    )),
                }
            }
            None => None,
        };

        let mut sink = PermissionReviewSink::default();
        let summary = self
            .llm
            .stream_chat(chat_request, cancel, &mut sink)
            .await?;
        crate::llm::validate_toolless_text_response(
            "permission reviewer",
            &summary,
            sink.saw_tool_call,
        )?;
        parse_review_response(&sink.output)
    }
}

#[derive(Default)]
struct PermissionReviewSink {
    output: String,
    saw_tool_call: bool,
}

impl LlmEventSink for PermissionReviewSink {
    fn push(&mut self, event: LlmEvent) -> Result<(), LlmError> {
        match event {
            LlmEvent::TextDelta(delta) => self.output.push_str(&delta),
            LlmEvent::ToolCallStart { .. } | LlmEvent::ToolCallArgsDelta { .. } => {
                self.saw_tool_call = true;
            }
            LlmEvent::ReasoningDelta(_) | LlmEvent::Finished { .. } => {}
        }
        Ok(())
    }
}

fn parse_review_response(output: &str) -> Result<PermissionReviewDecision, LlmError> {
    let output = output.trim();
    let payload = if let Ok(payload) = serde_json::from_str::<PermissionReviewPayload>(output) {
        payload
    } else if let (Some(start), Some(end)) = (output.find('{'), output.rfind('}'))
        && start < end
        && let Some(slice) = output.get(start..=end)
    {
        serde_json::from_str::<PermissionReviewPayload>(slice).map_err(|_| {
            LlmError::Message("permission reviewer returned an invalid assessment".to_string())
        })?
    } else {
        return Err(LlmError::Message(
            "permission reviewer returned an invalid assessment".to_string(),
        ));
    };

    let risk_level = payload.risk_level.unwrap_or(match payload.outcome {
        PermissionReviewOutcome::Allow => PermissionReviewRisk::Low,
        PermissionReviewOutcome::Deny => PermissionReviewRisk::High,
    });
    let rationale = payload
        .rationale
        .filter(|rationale| !rationale.trim().is_empty())
        .unwrap_or_else(|| match payload.outcome {
            PermissionReviewOutcome::Allow => {
                "Auto-review returned a low-risk allow decision.".to_string()
            }
            PermissionReviewOutcome::Deny => {
                "Auto-review returned a deny decision without a rationale.".to_string()
            }
        });

    Ok(PermissionReviewDecision {
        risk_level,
        user_authorization: payload
            .user_authorization
            .unwrap_or(PermissionReviewUserAuthorization::Unknown),
        outcome: payload.outcome,
        rationale,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use camino::Utf8PathBuf;

    use super::*;
    use crate::config::ProviderMetadataMode;
    use crate::llm::{LlmResponseSummary, ModelCapabilities};
    use crate::session::FinishReason;
    use crate::tool::PermissionRisk;
    use crate::workspace::AccessKind;

    struct ApprovingClient {
        requests: Mutex<Vec<ChatRequest>>,
    }

    #[async_trait(?Send)]
    impl LlmClient for ApprovingClient {
        async fn stream_chat(
            &self,
            request: ChatRequest,
            _cancel: CancellationToken,
            sink: &mut dyn LlmEventSink,
        ) -> Result<LlmResponseSummary, LlmError> {
            self.requests.lock().expect("requests").push(request);
            sink.push(LlmEvent::TextDelta(
                r#"{"risk_level":"medium","user_authorization":"high","outcome":"allow","rationale":"The scoped dependency restore supports the requested build."}"#
                    .to_string(),
            ))?;
            Ok(LlmResponseSummary {
                finish_reason: FinishReason::Stop,
                usage: None,
                response_id: None,
            })
        }
    }

    #[test]
    fn parses_full_allow_and_deny_assessments() {
        let approved = parse_review_response(
            r#"{"risk_level":"critical","user_authorization":"high","outcome":"allow","rationale":"The user explicitly authorized the scoped action."}"#,
        )
        .expect("allow assessment");
        assert!(approved.allows());
        assert_eq!(approved.risk_level, PermissionReviewRisk::Critical);
        assert_eq!(
            approved.user_authorization,
            PermissionReviewUserAuthorization::High
        );

        let denied = parse_review_response(
            r#"{"risk_level":"critical","user_authorization":"low","outcome":"deny","rationale":"Unrelated credential access."}"#,
        )
        .expect("deny assessment");
        assert!(!denied.allows());
        assert_eq!(denied.risk_level, PermissionReviewRisk::Critical);
    }

    #[test]
    fn outcome_only_allow_uses_codex_defaults() {
        let assessment =
            parse_review_response(r#"{"outcome":"allow"}"#).expect("minimal allow assessment");

        assert!(assessment.allows());
        assert_eq!(assessment.risk_level, PermissionReviewRisk::Low);
        assert_eq!(
            assessment.user_authorization,
            PermissionReviewUserAuthorization::Unknown
        );
        assert_eq!(
            assessment.rationale,
            "Auto-review returned a low-risk allow decision."
        );
    }

    #[test]
    fn outcome_only_deny_uses_codex_defaults() {
        let assessment =
            parse_review_response(r#"{"outcome":"deny"}"#).expect("minimal deny assessment");

        assert!(!assessment.allows());
        assert_eq!(assessment.risk_level, PermissionReviewRisk::High);
        assert_eq!(
            assessment.user_authorization,
            PermissionReviewUserAuthorization::Unknown
        );
        assert_eq!(
            assessment.rationale,
            "Auto-review returned a deny decision without a rationale."
        );
    }

    #[test]
    fn surrounding_prose_recovers_the_first_to_last_json_object() {
        let assessment = parse_review_response(
            "Assessment follows:\n{\"outcome\":\"deny\",\"rationale\":\"Not authorized.\"}\nEnd.",
        )
        .expect("thin wrapper recovery");

        assert!(!assessment.allows());
        assert_eq!(assessment.risk_level, PermissionReviewRisk::High);
        assert_eq!(assessment.rationale, "Not authorized.");
    }

    #[test]
    fn optional_metadata_and_unknown_fields_do_not_replace_outcome_as_owner() {
        let assessment = parse_review_response(
            r#"{"risk_level":"critical","user_authorization":"high","outcome":"allow","rationale":"Explicitly authorized.","future_metadata":true}"#,
        )
        .expect("forward-compatible assessment");

        assert!(assessment.allows());
        assert_eq!(assessment.risk_level, PermissionReviewRisk::Critical);
        assert_eq!(
            assessment.user_authorization,
            PermissionReviewUserAuthorization::High
        );
    }

    #[test]
    fn unknown_outcome_and_non_json_fail_closed_without_keyword_inference() {
        for output in [
            "APPROVE",
            "DECISION: APPROVE\nRISK: LOW\nRATIONALE: old format",
            r#"{"risk_level":"low","user_authorization":"high","outcome":"maybe","rationale":"uncertain"}"#,
            r#"{"risk_level":"unknown","user_authorization":"high","outcome":"allow","rationale":"invalid risk"}"#,
            r#"{"risk_level":"low","user_authorization":"explicit","outcome":"allow","rationale":"invalid authorization"}"#,
            r#"{"risk_level":"low","user_authorization":"high","rationale":"missing outcome"}"#,
            r#"{"risk_level":"low","risk_level":"high","user_authorization":"high","outcome":"allow","rationale":"duplicate"}"#,
            "DECISION: MAYBE\nRISK: LOW\nRATIONALE: uncertain",
        ] {
            assert!(parse_review_response(output).is_err(), "{output}");
        }
    }

    #[tokio::test]
    async fn reviewer_sends_context_and_exact_request_without_tools() {
        let client = ApprovingClient {
            requests: Mutex::new(Vec::new()),
        };
        let config = ResolvedConfig::default();
        let model = ModelProfile {
            name: "review-model".to_string(),
            context_window: 32_768,
            max_output_tokens: 512,
            provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
            capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: false,
                supports_images: false,
            },
        };
        let permission = PermissionRequest {
            access: AccessKind::Shell,
            summary: "Restore project dependencies".to_string(),
            details: vec!["command: cargo fetch".to_string()],
            targets: vec![Utf8PathBuf::from("C:/workspace")],
            outside_workspace: false,
            risks: vec![PermissionRisk::Network],
            agent_path: None,
            agent_task_name: None,
        };
        let decision = PermissionReviewer {
            llm: &client,
            model: &model,
            config: &config,
            request_gate: None,
        }
        .review(
            "The user asked to build the Rust project.",
            &permission,
            CancellationToken::new(),
        )
        .await
        .expect("review");

        assert!(decision.allows());
        assert_eq!(decision.risk_level, PermissionReviewRisk::Medium);
        let requests = client.requests.lock().expect("requests");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].tools.is_empty());
        assert!(!requests[0].parallel_tool_calls);
        assert_eq!(requests[0].temperature, Some(0.0));
        let ModelMessage::User { content } = &requests[0].messages[0] else {
            panic!("expected reviewer user message");
        };
        assert!(content.contains("The user asked to build the Rust project."));
        assert!(content.contains("cargo fetch"));
        assert!(content.contains("\"network\""));
    }
}
