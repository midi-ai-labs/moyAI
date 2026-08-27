use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::config::model::ProviderReasoningCapability;
use crate::config::{AccessMode, ProviderDeadlines, ProviderProfile, ProviderTarget};
use crate::error::LlmError;
use crate::llm::{
    ChatRequest, LlmClient, LlmEvent, LlmEventSink, LlmResponseSummary, ModelCapabilities,
    ModelMessage, ModelProfile, ProviderPhaseEvent, validate_toolless_text_response,
};
use crate::protocol::{
    ContentPart, HistoryItemPayload, ModelResponseId, ProtocolPageRequest, TurnId,
    TurnInterruptionCause, TurnTerminalOutcome,
};
#[cfg(test)]
use crate::protocol::{UserInputItem, UserTurn};
use crate::session::{
    AdmissionId, DurableTurnTerminal, RunConfigSnapshot, RunEvent, RunMetrics, SessionId,
    TokenUsage,
};
use crate::storage::StoreBundle;
use crate::storage::session_repo::{
    AdmittedTerminalSettlement, ModelResponseWrite, RUN_ADMISSION_HEARTBEAT_INTERVAL_MS,
    RunAdmissionLeaseRenewalOutcome, SqliteSessionRepository,
};

const SIDE_CHAT_SYSTEM_PROMPT: &str = include_str!("../../assets/prompts/side_chat.md");
// `ModelProfile` still carries this retired compatibility field. Provider
// serializers omit it, so side chat supplies a fixed inert value instead of a
// legacy moyAI generation setting.
const RETIRED_MAX_OUTPUT_TOKENS_PLACEHOLDER: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SideChatHistoryMessage {
    User(String),
    Assistant(String),
}

#[derive(Clone)]
pub(crate) struct SideChatRequestProfile {
    pub base_url: String,
    pub model: String,
    pub provider_profile: ProviderProfile,
    pub request_timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub max_retries: u8,
    pub context_window: u32,
    pub api_key_env: Option<String>,
    pub extra_headers: BTreeMap<String, String>,
}

impl std::fmt::Debug for SideChatRequestProfile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SideChatRequestProfile")
            .field("base_url", &"<redacted provider endpoint>")
            .field("model", &self.model)
            .field("provider_profile", &self.provider_profile)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("connect_timeout_ms", &self.connect_timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("context_window", &self.context_window)
            .field("api_key_env", &self.api_key_env)
            .field("extra_header_count", &self.extra_headers.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SideChatStreamEvent {
    TextDelta(String),
    ProviderPhase(String),
}

#[derive(Debug, Clone)]
pub(crate) struct SideChatRunOutput {
    pub text: String,
    pub summary: LlmResponseSummary,
}

#[derive(Debug)]
enum SideChatAttemptResult {
    Response(SideChatRunOutput),
    Outcome {
        outcome: TurnTerminalOutcome,
        token_usage: Option<TokenUsage>,
        model_request_count: usize,
    },
    ExistingTerminal(DurableTurnTerminal),
}

impl SideChatAttemptResult {
    fn failed(error: impl Into<String>, model_request_count: usize) -> Self {
        Self::Outcome {
            outcome: TurnTerminalOutcome::Failed {
                error: error.into(),
            },
            token_usage: None,
            model_request_count,
        }
    }

    fn interrupted(
        cause: TurnInterruptionCause,
        token_usage: Option<TokenUsage>,
        model_request_count: usize,
    ) -> Self {
        Self::Outcome {
            outcome: TurnTerminalOutcome::Interrupted { cause },
            token_usage,
            model_request_count,
        }
    }
}

#[cfg(test)]
pub(crate) async fn execute_canonical_side_chat(
    store: StoreBundle,
    conversation_session_id: SessionId,
    profile: SideChatRequestProfile,
    user_text: String,
    cancel: CancellationToken,
    on_event: impl FnMut(SideChatStreamEvent),
) -> Result<(), String> {
    let turn_id = TurnId::new();
    let user_turn = UserTurn {
        turn_id,
        items: vec![UserInputItem::Text { text: user_text }],
        prompt_dispatch: None,
        editor_context: None,
    };
    let repository = store.session_repo();
    let admission = repository
        .admit_session_turn_with_initial_user_turn(
            conversation_session_id,
            turn_id,
            Some(&user_turn),
        )
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "side chat conversation already has an active request".to_string())?;

    execute_admitted_canonical_side_chat(
        store,
        conversation_session_id,
        admission.admission_id,
        turn_id,
        profile,
        cancel,
        on_event,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_admitted_canonical_side_chat(
    store: StoreBundle,
    conversation_session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    profile: SideChatRequestProfile,
    cancel: CancellationToken,
    mut on_event: impl FnMut(SideChatStreamEvent),
) -> Result<(), String> {
    let repository = store.session_repo();
    let started = Instant::now();
    let attempt = run_admitted_side_chat(
        &store,
        &repository,
        conversation_session_id,
        admission_id,
        turn_id,
        profile.clone(),
        cancel,
        &mut on_event,
    )
    .await;
    let (outcome, final_response_id, token_usage, model_request_count) = match attempt {
        SideChatAttemptResult::ExistingTerminal(terminal) => {
            return side_chat_terminal_result(&terminal.outcome);
        }
        SideChatAttemptResult::Response(output) => {
            let response_id = ModelResponseId::new();
            let token_usage = output.summary.usage;
            match repository
                .record_model_response_with_protocol_bundle(
                    conversation_session_id,
                    admission_id,
                    turn_id,
                    ModelResponseWrite {
                        response_id,
                        assistant_text: Some(output.text),
                        assistant_protocol_sequence_no: None,
                        tool_calls: Vec::new(),
                    },
                )
                .await
            {
                Ok(_) => (
                    TurnTerminalOutcome::Completed,
                    Some(response_id),
                    token_usage,
                    1,
                ),
                Err(error) => (
                    TurnTerminalOutcome::Failed {
                        error: format!("failed to record the side chat provider response: {error}"),
                    },
                    None,
                    token_usage,
                    1,
                ),
            }
        }
        SideChatAttemptResult::Outcome {
            outcome,
            token_usage,
            model_request_count,
        } => (outcome, None, token_usage, model_request_count),
    };
    let terminal = DurableTurnTerminal {
        outcome: outcome.clone(),
        final_response_id,
        tool_call_count: 0,
        failed_tool_count: 0,
        change_count: 0,
        metrics: RunMetrics {
            model_request_count,
            elapsed_ms: Some(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
            token_usage,
            config: Some(RunConfigSnapshot {
                model: profile.model,
                base_url: profile.base_url,
                access_mode: AccessMode::Default,
            }),
            ..RunMetrics::default()
        },
    };
    let event = RunEvent::TurnTerminal {
        session_id: conversation_session_id,
        terminal: Box::new(terminal),
    };
    let committed = repository
        .settle_admitted_turn_with_protocol_event(
            conversation_session_id,
            admission_id,
            &event,
            turn_id,
            None,
            None,
        )
        .await;
    match committed {
        Ok(
            AdmittedTerminalSettlement::Applied { terminal }
            | AdmittedTerminalSettlement::AlreadyTerminalizedBySameAdmission { terminal },
        ) => side_chat_terminal_result(&terminal.outcome),
        Ok(AdmittedTerminalSettlement::NotOwned) => {
            resolve_existing_side_chat_terminal(
                &repository,
                conversation_session_id,
                turn_id,
                outcome.summary(),
                "side chat terminal was not owned by the active request",
            )
            .await
        }
        Err(error) => {
            resolve_existing_side_chat_terminal(
                &repository,
                conversation_session_id,
                turn_id,
                outcome.summary(),
                &format!("side chat terminalization failed: {error}"),
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_admitted_side_chat(
    store: &StoreBundle,
    repository: &SqliteSessionRepository,
    conversation_session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
    profile: SideChatRequestProfile,
    cancel: CancellationToken,
    on_event: &mut impl FnMut(SideChatStreamEvent),
) -> SideChatAttemptResult {
    if cancel.is_cancelled() {
        return SideChatAttemptResult::interrupted(TurnInterruptionCause::UserStop, None, 0);
    }
    let history = match side_chat_history(store, conversation_session_id).await {
        Ok(history) => history,
        Err(error) => {
            return SideChatAttemptResult::failed(
                format!("failed to load canonical side chat history: {error}"),
                0,
            );
        }
    };
    if cancel.is_cancelled() {
        return SideChatAttemptResult::interrupted(TurnInterruptionCause::UserStop, None, 0);
    }
    let api_key = match crate::llm::resolve_api_key_from_env(profile.api_key_env.as_deref()) {
        Ok(api_key) => api_key,
        Err(error) => return SideChatAttemptResult::failed(error.to_string(), 0),
    };
    if cancel.is_cancelled() {
        return SideChatAttemptResult::interrupted(TurnInterruptionCause::UserStop, None, 0);
    }
    let client = crate::llm::OpenAiCompatClient::new(api_key);
    let request = run_side_chat_request(&client, profile, history, cancel.clone(), |event| {
        on_event(event);
    });
    tokio::pin!(request);
    let mut heartbeat =
        tokio::time::interval(Duration::from_millis(RUN_ADMISSION_HEARTBEAT_INTERVAL_MS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let result = loop {
        tokio::select! {
            result = &mut request => break result,
            _ = heartbeat.tick() => {
                match repository
                    .renew_admitted_run_lease(conversation_session_id, admission_id, turn_id)
                    .await
                {
                    Ok(RunAdmissionLeaseRenewalOutcome::Renewed) => {}
                    Ok(RunAdmissionLeaseRenewalOutcome::InterruptRequested(cause)) => {
                        cancel.cancel();
                        return SideChatAttemptResult::interrupted(cause, None, 1);
                    }
                    Ok(RunAdmissionLeaseRenewalOutcome::StopFenced(outcome)) => {
                        cancel.cancel();
                        return SideChatAttemptResult::Outcome {
                            outcome,
                            token_usage: None,
                            model_request_count: 1,
                        };
                    }
                    Ok(RunAdmissionLeaseRenewalOutcome::Terminal(terminal)) => {
                        cancel.cancel();
                        return SideChatAttemptResult::ExistingTerminal(terminal);
                    }
                    Ok(RunAdmissionLeaseRenewalOutcome::SupersededOrExpired) => {
                        cancel.cancel();
                        return SideChatAttemptResult::failed(
                            "side chat request ownership expired",
                            1,
                        );
                    }
                    Err(error) => {
                        cancel.cancel();
                        return SideChatAttemptResult::failed(
                            format!("side chat lease renewal failed: {error}"),
                            1,
                        );
                    }
                }
            }
        }
    };

    match result {
        Ok(output) => SideChatAttemptResult::Response(output),
        Err(error) if cancel.is_cancelled() => SideChatAttemptResult::interrupted(
            TurnInterruptionCause::UserStop,
            error.token_usage().cloned(),
            1,
        ),
        Err(error) => SideChatAttemptResult::Outcome {
            outcome: TurnTerminalOutcome::Failed {
                error: error.to_string(),
            },
            token_usage: error.token_usage().cloned(),
            model_request_count: 1,
        },
    }
}

async fn resolve_existing_side_chat_terminal(
    repository: &SqliteSessionRepository,
    conversation_session_id: SessionId,
    turn_id: TurnId,
    requested_summary: &str,
    failure: &str,
) -> Result<(), String> {
    match repository
        .durable_terminal_for_turn(conversation_session_id, turn_id)
        .await
    {
        Ok(Some(terminal)) => side_chat_terminal_result(&terminal.outcome),
        Ok(None) => Err(format!("{requested_summary}; {failure}")),
        Err(error) => Err(format!(
            "{requested_summary}; {failure}; additionally failed to read durable terminal truth: {error}"
        )),
    }
}

fn side_chat_terminal_result(outcome: &TurnTerminalOutcome) -> Result<(), String> {
    match outcome {
        TurnTerminalOutcome::Completed | TurnTerminalOutcome::Interrupted { .. } => Ok(()),
        TurnTerminalOutcome::Failed { error } => Err(error.clone()),
    }
}

async fn side_chat_history(
    store: &StoreBundle,
    conversation_session_id: SessionId,
) -> Result<Vec<SideChatHistoryMessage>, crate::error::StorageError> {
    let snapshot = store
        .session_repo()
        .canonical_session_protocol_snapshot(
            conversation_session_id,
            ProtocolPageRequest::Latest {
                limit: crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
            },
            ProtocolPageRequest::Latest { limit: 1 },
        )
        .await?;
    Ok(snapshot
        .protocol
        .history
        .items
        .into_iter()
        .filter_map(|item| match item.payload {
            HistoryItemPayload::UserTurn { content, .. } => {
                content_text(content).map(SideChatHistoryMessage::User)
            }
            HistoryItemPayload::AssistantMessage { content, .. } => {
                content_text(content).map(SideChatHistoryMessage::Assistant)
            }
            _ => None,
        })
        .collect())
}

fn content_text(content: Vec<ContentPart>) -> Option<String> {
    let text = content
        .into_iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text),
            ContentPart::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

pub(crate) async fn run_side_chat_request(
    client: &dyn LlmClient,
    profile: SideChatRequestProfile,
    history: Vec<SideChatHistoryMessage>,
    cancel: CancellationToken,
    mut on_event: impl FnMut(SideChatStreamEvent),
) -> Result<SideChatRunOutput, LlmError> {
    let target = ProviderTarget::new(
        &profile.base_url,
        &profile.model,
        profile.provider_profile,
        ProviderDeadlines {
            request_timeout_ms: profile.request_timeout_ms,
            connect_timeout_ms: profile.connect_timeout_ms,
            max_connect_retries: profile.max_retries,
        },
    )
    .map_err(|error| LlmError::Message(error.to_string()))?;
    let model = ModelProfile {
        name: profile.model,
        context_window: profile.context_window,
        max_output_tokens: RETIRED_MAX_OUTPUT_TOKENS_PLACEHOLDER,
        provider_profile: profile.provider_profile,
        capabilities: ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_images: false,
        },
    };
    let messages = history
        .into_iter()
        .map(|message| match message {
            SideChatHistoryMessage::User(content) => ModelMessage::User { content },
            SideChatHistoryMessage::Assistant(content) => ModelMessage::Assistant { content },
        })
        .collect();
    let request = ChatRequest::new(
        target,
        model,
        SIDE_CHAT_SYSTEM_PROMPT.to_string(),
        messages,
        Vec::new(),
        None,
        ProviderReasoningCapability::Unsupported,
        profile.extra_headers,
    );
    let mut sink = SideChatSink {
        output: String::new(),
        saw_tool_call: false,
        on_event: &mut on_event,
    };
    let summary = client.stream_chat(request, cancel, &mut sink).await?;
    validate_toolless_text_response("side chat", &summary, sink.saw_tool_call)?;
    if sink.output.trim().is_empty() {
        return Err(LlmError::Message(
            "side chat provider returned an empty text response".to_string(),
        ));
    }
    Ok(SideChatRunOutput {
        text: sink.output,
        summary,
    })
}

struct SideChatSink<'a, F> {
    output: String,
    saw_tool_call: bool,
    on_event: &'a mut F,
}

impl<F> LlmEventSink for SideChatSink<'_, F>
where
    F: FnMut(SideChatStreamEvent),
{
    fn push(&mut self, event: LlmEvent) -> Result<(), LlmError> {
        match event {
            LlmEvent::TextDelta(delta) => {
                self.output.push_str(&delta);
                (self.on_event)(SideChatStreamEvent::TextDelta(delta));
            }
            LlmEvent::ToolCallStart { .. } | LlmEvent::ToolCallArgsDelta { .. } => {
                self.saw_tool_call = true;
            }
            LlmEvent::ReasoningSummaryDelta(_) | LlmEvent::Finished { .. } => {}
        }
        Ok(())
    }

    fn provider_phase(&mut self, event: ProviderPhaseEvent) -> Result<(), LlmError> {
        (self.on_event)(SideChatStreamEvent::ProviderPhase(
            event.phase.as_str().to_string(),
        ));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use camino::Utf8PathBuf;

    use super::*;
    use crate::llm::LlmResponseSummary;
    use crate::protocol::{ProtocolEventStore as _, RuntimeEventMsg};
    use crate::session::{
        FinishReason, NewSession, ProjectId, ProjectRepository as _, SessionRepository as _,
        SessionStatus,
    };
    use crate::storage::{SqliteStore, StoragePaths};

    struct FixtureClient {
        events: Vec<LlmEvent>,
        finish_reason: FinishReason,
    }

    #[async_trait(?Send)]
    impl LlmClient for FixtureClient {
        async fn stream_chat(
            &self,
            request: ChatRequest,
            _cancel: CancellationToken,
            sink: &mut dyn LlmEventSink,
        ) -> Result<LlmResponseSummary, LlmError> {
            request.validate_provider_lifecycle()?;
            assert!(request.tools.is_empty());
            assert!(request.reasoning.is_none());
            assert!(!request.parallel_tool_calls);
            assert!(request.extra_body.is_none());
            assert!(request.temperature.is_none());
            assert!(request.top_p.is_none());
            assert_eq!(
                request.model.max_output_tokens,
                RETIRED_MAX_OUTPUT_TOKENS_PLACEHOLDER
            );
            assert!(!request.model.capabilities.supports_reasoning);
            for event in self.events.clone() {
                sink.push(event)?;
            }
            Ok(LlmResponseSummary {
                finish_reason: self.finish_reason,
                usage: None,
                response_id: None,
            })
        }
    }

    fn profile() -> SideChatRequestProfile {
        SideChatRequestProfile {
            base_url: "http://provider.local:1234".to_string(),
            model: "google/gemma-4-12b-qat".to_string(),
            provider_profile: ProviderProfile::LmStudio,
            request_timeout_ms: 60_000,
            connect_timeout_ms: 10_000,
            max_retries: 0,
            context_window: 131_072,
            api_key_env: None,
            extra_headers: BTreeMap::new(),
        }
    }

    async fn canonical_side_chat_fixture() -> (tempfile::TempDir, StoreBundle, SessionId) {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data dir");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let sqlite = SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let project_id = ProjectId::new();
        store
            .project_repo()
            .upsert_project(project_id, &data_dir, "side chat", "none")
            .await
            .expect("project");
        let session = store
            .session_repo()
            .create_session(NewSession {
                project_id,
                title: "hidden side chat conversation".to_string(),
                cwd: data_dir,
                model: "google/gemma-4-12b-qat".to_string(),
                base_url: "http://provider.local:1234".to_string(),
                access_mode: AccessMode::Default,
                provider_connection: None,
            })
            .await
            .expect("session");
        (temp, store, session.id)
    }

    #[tokio::test]
    async fn side_chat_is_toolless_and_does_not_inject_reasoning_or_sampling() {
        let client = FixtureClient {
            events: vec![
                LlmEvent::ReasoningSummaryDelta("provider-owned reasoning".to_string()),
                LlmEvent::TextDelta("短い".to_string()),
                LlmEvent::TextDelta("回答".to_string()),
            ],
            finish_reason: FinishReason::Stop,
        };
        let mut streamed = Vec::new();
        let output = run_side_chat_request(
            &client,
            profile(),
            vec![SideChatHistoryMessage::User("質問".to_string())],
            CancellationToken::new(),
            |event| streamed.push(event),
        )
        .await
        .expect("side response");
        assert_eq!(output.text, "短い回答");
        assert_eq!(
            streamed,
            vec![
                SideChatStreamEvent::TextDelta("短い".to_string()),
                SideChatStreamEvent::TextDelta("回答".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn side_chat_rejects_provider_tool_calls() {
        let client = FixtureClient {
            events: vec![LlmEvent::ToolCallStart {
                call_id: "call-1".to_string(),
                tool_name: "write".to_string(),
            }],
            finish_reason: FinishReason::Stop,
        };
        let error = run_side_chat_request(
            &client,
            profile(),
            vec![SideChatHistoryMessage::User("質問".to_string())],
            CancellationToken::new(),
            |_| {},
        )
        .await
        .expect_err("tool call must fail");
        assert!(matches!(error, LlmError::ToollessTextShape { .. }));
    }

    #[tokio::test]
    async fn post_admission_setup_failure_commits_failed_terminal() {
        let (_temp, store, conversation_session_id) = canonical_side_chat_fixture().await;
        let missing_env = format!("MOYAI_SIDE_CHAT_MISSING_KEY_{}", ulid::Ulid::new());
        assert!(std::env::var_os(&missing_env).is_none());
        let mut request_profile = profile();
        request_profile.api_key_env = Some(missing_env.clone());

        let error = execute_canonical_side_chat(
            store.clone(),
            conversation_session_id,
            request_profile,
            "短い質問".to_string(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .expect_err("missing configured key must fail the admitted turn");

        assert!(error.contains(&missing_env));
        let repository = store.session_repo();
        let session = repository
            .get_session(conversation_session_id)
            .await
            .expect("terminal session");
        assert_eq!(session.status, SessionStatus::Failed);
        assert_eq!(
            repository
                .session_projection_state(conversation_session_id)
                .await
                .expect("projection")
                .active_turn_id,
            None
        );
        let terminal_events = store
            .protocol_event_store()
            .list_runtime_events_for_session(conversation_session_id)
            .expect("runtime events")
            .into_iter()
            .filter_map(|event| match event.msg {
                RuntimeEventMsg::TurnTerminal { terminal } => Some(terminal),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(terminal_events.len(), 1);
        assert!(matches!(
            &terminal_events[0].outcome,
            TurnTerminalOutcome::Failed { error } if error.contains(&missing_env)
        ));
        assert_eq!(terminal_events[0].metrics.model_request_count, 0);
    }

    #[tokio::test]
    async fn cancellation_immediately_after_admission_commits_interrupted_terminal() {
        let (_temp, store, conversation_session_id) = canonical_side_chat_fixture().await;
        let cancel = CancellationToken::new();
        cancel.cancel();

        execute_canonical_side_chat(
            store.clone(),
            conversation_session_id,
            profile(),
            "停止する質問".to_string(),
            cancel,
            |_| {},
        )
        .await
        .expect("user Stop is a canonical interrupted outcome");

        let session = store
            .session_repo()
            .get_session(conversation_session_id)
            .await
            .expect("terminal session");
        assert_eq!(session.status, SessionStatus::Cancelled);
        let terminal_events = store
            .protocol_event_store()
            .list_runtime_events_for_session(conversation_session_id)
            .expect("runtime events")
            .into_iter()
            .filter_map(|event| match event.msg {
                RuntimeEventMsg::TurnTerminal { terminal } => Some(terminal),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(terminal_events.len(), 1);
        assert!(matches!(
            &terminal_events[0].outcome,
            TurnTerminalOutcome::Interrupted {
                cause: TurnInterruptionCause::UserStop
            }
        ));
        assert_eq!(terminal_events[0].metrics.model_request_count, 0);
    }
}
