use std::future::Future;
use tokio_util::sync::CancellationToken;

use crate::config::{ResolvedConfig, ResolvedTurnConfig};
use crate::error::LlmError;
use crate::llm::model_policy::ProviderCapabilities;
use crate::llm::{
    ChatRequest, ConfigModelCatalog, LlmClient, LlmEvent, LlmEventSink, ModelCatalog, ModelMessage,
    OpenAiCompatClient, check_model_availability, resolve_api_key_from_env,
    validate_model_availability_report,
};
use crate::session::{ActiveTurnExpectation, SessionId, SessionService};

const PROMPT_ENHANCER_SYSTEM_PROMPT: &str = include_str!("../../assets/prompts/prompt-enhancer.md");

pub async fn enhance_prompt(
    config: &ResolvedConfig,
    raw_prompt: &str,
    cancellation: CancellationToken,
) -> Result<String, LlmError> {
    validate_prompt_enhance_readiness(config).await?;
    let turn_config = ResolvedTurnConfig::from_effective(config)
        .map_err(|error| LlmError::Message(error.to_string()))?;
    let runtime_config = turn_config.runtime_config();
    let provider_target = turn_config.provider();
    let api_key = resolve_api_key_from_env(runtime_config.model.api_key_env.as_deref())?;
    let client = OpenAiCompatClient::new(api_key);
    let model = ConfigModelCatalog::new(runtime_config.clone()).resolve(None)?;
    let provider_capabilities = ProviderCapabilities::from_config(runtime_config);
    let mut request = ChatRequest::new(
        provider_target.clone(),
        model,
        PROMPT_ENHANCER_SYSTEM_PROMPT.trim().to_string(),
        vec![ModelMessage::User {
            content: raw_prompt.to_string(),
        }],
        Vec::new(),
        None,
        provider_capabilities.reasoning,
        runtime_config.model.extra_headers.clone(),
    );
    request.temperature = runtime_config.model.temperature;
    request.top_p = runtime_config.model.top_p;
    request.top_k = runtime_config.model.top_k;
    request.presence_penalty = runtime_config.model.presence_penalty;
    request.frequency_penalty = runtime_config.model.frequency_penalty;
    request.seed = runtime_config.model.seed;
    request.stop_sequences = runtime_config.model.stop_sequences.clone();
    request.extra_body = runtime_config.model.extra_body_json.clone();
    let mut sink = PromptEnhanceSink::default();
    let summary = client.stream_chat(request, cancellation, &mut sink).await?;
    crate::llm::validate_toolless_text_response("prompt enhancer", &summary, sink.saw_tool_call)?;
    let output = sink.output.trim().to_string();
    if output.is_empty() {
        return Err(LlmError::Message(
            "prompt enhancer returned an empty draft".to_string(),
        ));
    }
    Ok(output)
}

pub(crate) async fn enhance_prompt_for_captured_idle<F, Fut>(
    session_service: &SessionService,
    session_id: Option<SessionId>,
    expected_active_turn: ActiveTurnExpectation,
    provider: F,
) -> Result<String, String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    if !matches!(expected_active_turn, ActiveTurnExpectation::Idle { .. }) {
        return Err("prompt enhancement requires a captured idle session owner".to_string());
    }
    if let Some(session_id) = session_id {
        let actual = session_service
            .active_turn_expectation_for_session(session_id)
            .await
            .map_err(|error| error.to_string())?;
        if actual != Some(expected_active_turn) {
            return Err(
                "the durable session owner changed before prompt enhancement started".to_string(),
            );
        }
    }
    let draft = provider().await?;
    if let Some(session_id) = session_id {
        let actual = session_service
            .active_turn_expectation_for_session(session_id)
            .await
            .map_err(|error| error.to_string())?;
        if actual != Some(expected_active_turn) {
            return Err(
                "the durable session owner changed while the prompt was enhanced".to_string(),
            );
        }
    }
    Ok(draft)
}

async fn validate_prompt_enhance_readiness(config: &ResolvedConfig) -> Result<(), LlmError> {
    let report = check_model_availability(config, None, None, false).await;
    validate_model_availability_report(&config.model, &report, false)
}

#[derive(Default)]
struct PromptEnhanceSink {
    output: String,
    saw_tool_call: bool,
}

impl LlmEventSink for PromptEnhanceSink {
    fn push(&mut self, event: LlmEvent) -> Result<(), LlmError> {
        match event {
            LlmEvent::TextDelta(delta) => {
                self.output.push_str(&delta);
            }
            LlmEvent::ReasoningSummaryDelta(_) => {}
            LlmEvent::ToolCallStart { .. } | LlmEvent::ToolCallArgsDelta { .. } => {
                self.saw_tool_call = true;
            }
            LlmEvent::Finished { .. } => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use camino::Utf8PathBuf;

    use super::*;
    use crate::config::ResolvedConfig;
    use crate::session::repository::ProjectRepository;
    use crate::session::{SessionSelector, SessionStartRequest};
    use crate::storage::{SqliteStore, StoragePaths, StoreBundle};

    async fn idle_session_fixture() -> (
        tempfile::TempDir,
        SessionService,
        StoreBundle,
        crate::session::SessionId,
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        std::fs::create_dir_all(data_dir.as_std_path()).expect("data dir");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let sqlite = SqliteStore::open(&paths).expect("store");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let config = ResolvedConfig::default();
        let workspace =
            crate::workspace::WorkspaceDiscovery::discover_fixed_root(&data_dir, &config)
                .expect("workspace");
        store
            .project_repo()
            .upsert_project(workspace.project_id, &workspace.root, "test", "none")
            .await
            .expect("project");
        let service = SessionService::new(store.clone());
        let provider_connection =
            crate::session::SessionProviderConnection::from_model_config(&config.model);
        let session = service
            .start_or_resume(
                SessionStartRequest {
                    selector: SessionSelector::New,
                    title: Some("enhance owner".to_string()),
                    cwd: workspace.cwd.clone(),
                    model: config.model.model,
                    base_url: config.model.base_url,
                    access_mode: config.permissions.access_mode,
                    provider_connection: Some(provider_connection),
                },
                workspace,
            )
            .await
            .expect("session");
        (temp, service, store, session.session.id)
    }

    #[tokio::test]
    async fn known_stale_idle_owner_rejects_before_provider_side_effect() {
        let (_temp, service, store, session_id) = idle_session_fixture().await;
        let expected = service
            .active_turn_expectation_for_session(session_id)
            .await
            .expect("expectation")
            .expect("session");
        store
            .session_repo()
            .admit_session_turn(session_id, crate::protocol::TurnId::new())
            .await
            .expect("admission")
            .expect("turn admitted");
        let calls = Arc::new(AtomicUsize::new(0));
        let provider_calls = Arc::clone(&calls);

        let error =
            enhance_prompt_for_captured_idle(&service, Some(session_id), expected, || async move {
                provider_calls.fetch_add(1, Ordering::SeqCst);
                Ok("draft".to_string())
            })
            .await
            .expect_err("stale owner");

        assert!(error.contains("before prompt enhancement"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn owner_drift_during_provider_never_publishes_the_draft() {
        let (_temp, service, store, session_id) = idle_session_fixture().await;
        let expected = service
            .active_turn_expectation_for_session(session_id)
            .await
            .expect("expectation")
            .expect("session");
        let repository = store.session_repo();
        let calls = Arc::new(AtomicUsize::new(0));
        let provider_calls = Arc::clone(&calls);

        let error =
            enhance_prompt_for_captured_idle(&service, Some(session_id), expected, || async move {
                provider_calls.fetch_add(1, Ordering::SeqCst);
                repository
                    .admit_session_turn(session_id, crate::protocol::TurnId::new())
                    .await
                    .expect("admission")
                    .expect("turn admitted");
                Ok("must not be published".to_string())
            })
            .await
            .expect_err("post-provider owner drift");

        assert!(error.contains("while the prompt was enhanced"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
