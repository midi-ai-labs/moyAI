use super::*;
use crate::agent::shared::{SharedRunContext, SharedRunOutcome};
use crate::llm::{ChatRequest, LlmEvent, LlmEventSink, LlmResponseSummary, ModelMessage};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Mutex;

struct PublishThenAnswer {
    shared: bool,
    store: Mutex<Option<StoreBundle>>,
}

#[async_trait::async_trait(?Send)]
impl crate::llm::LlmClient for PublishThenAnswer {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        _cancel: tokio_util::sync::CancellationToken,
        sink: &mut dyn LlmEventSink,
    ) -> Result<LlmResponseSummary, crate::error::LlmError> {
        assert_eq!(
            request
                .tools
                .iter()
                .any(|tool| tool.name == "shared_publish_artifact"),
            self.shared
        );
        let has_result = request
            .messages
            .iter()
            .any(|message| matches!(message, ModelMessage::Tool { .. }));
        let store = self.store.lock().unwrap().clone().unwrap();
        let db = rusqlite::Connection::open(&store.paths().database_path).unwrap();
        let (session, turn): (String, String) = db
            .query_row(
                "SELECT id,active_turn_id FROM sessions WHERE active_turn_id IS NOT NULL LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let (session, turn) = (session.parse().unwrap(), turn.parse().unwrap());
        assert_eq!(
            store
                .session_repo()
                .require_active_shared_tool("artifact-job", session, turn)
                .is_ok(),
            self.shared
        );
        assert!(
            store
                .session_repo()
                .require_active_shared_tool("another-job", session, turn)
                .is_err()
        );
        assert!(
            store
                .session_repo()
                .require_active_shared_tool("artifact-job", session, crate::protocol::TurnId::new())
                .is_err()
        );
        let finish_reason = if self.shared && !has_result {
            sink.push(LlmEvent::ToolCallStart {
                call_id: "publish-solver-result".into(),
                tool_name: "shared_publish_artifact".into(),
            })?;
            sink.push(LlmEvent::ToolCallArgsDelta {
                call_id: "publish-solver-result".into(),
                delta: json!({"path":"solver.bin","name":"results/解.bin"}).to_string(),
            })?;
            crate::session::FinishReason::ToolCall
        } else {
            // The source may change after a successful publication; the canonical snapshot cannot.
            std::fs::write(store.paths().data_dir.join("solver.bin"), b"later contents").unwrap();
            sink.push(LlmEvent::TextDelta("Done".into()))?;
            crate::session::FinishReason::Stop
        };
        Ok(LlmResponseSummary {
            finish_reason,
            usage: None,
            response_id: None,
        })
    }
}

#[tokio::test]
async fn shared_artifact_normal_agent_persists_binary_snapshot_in_canonical_archive() {
    let mut config = ResolvedConfig::default();
    config.model.model = "scripted".into();
    config.model.base_url = "http://local".into();
    config.model.provider_profile = ProviderProfile::OpenAiCompatible;
    config.multi_agent.enabled = false;
    let llm = Arc::new(PublishThenAnswer {
        shared: true,
        store: Mutex::new(None),
    });
    let (service, store, workspace, _runtime) =
        run_service_fixture_with_llm(config.clone(), llm.clone()).await;
    *llm.store.lock().unwrap() = Some(store.clone());
    assert_eq!(workspace.authority_root(), store.paths().data_dir.as_path());
    let original = [0, 255, 128, 13, 10, 42];
    // Simulates a shell/solver output: no write tool or file_changes record exists.
    std::fs::write(workspace.authority_root().join("solver.bin"), original).unwrap();
    let mut request = control_run_request(
        config,
        &workspace,
        crate::session::SessionId::new(),
        "Attach the solver result",
    );
    request.session_id = None;
    request.agent_confirmation = Some(crate::cli::SharedConfirmationPrompt::new(NoPrompt));
    let result = service
        .execute_shared(
            request,
            SharedRunContext {
                job_id: "artifact-job".into(),
                attempt_id: "test-attempt".into(),
                generation: 1,
                project_id: "project".into(),
                environment_id: "solver".into(),
                allowed_child_environments: Vec::new(),
                allowed_child_candidates: Vec::new(),
                resume: None,
                continuation: None,
            },
            &mut crate::cli::HumanRenderer::new(),
            &mut NoPrompt,
        )
        .await
        .unwrap();
    let SharedRunOutcome::Completed(summary) = result else {
        panic!("artifact tool must not suspend")
    };
    assert_eq!(summary.failed_tool_count(), 0);
    assert_eq!(summary.change_count(), 0);
    assert_eq!(summary.tool_call_count(), 1);
    assert!(
        store
            .session_repo()
            .require_active_shared_tool("artifact-job", summary.session_id(), summary.turn_id())
            .is_err()
    );
    let archive = store
        .session_repo()
        .export_shared_archive("artifact-job", store.paths())
        .unwrap()
        .unwrap();
    let tools = archive["tables"]["tool_calls"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    let snapshot = tools[0]["truncated_output_path"].as_str().unwrap();
    assert_eq!(std::fs::read(snapshot).unwrap(), original);
    assert_eq!(archive["sidecars"].as_array().unwrap().len(), 1);
    assert_eq!(archive["sidecars"][0]["original_path"], snapshot);
    assert_eq!(
        archive["sidecars"][0]["sha256"],
        format!("{:x}", Sha256::digest(original))
    );
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(archive["sidecars"][0]["content_base64"].as_str().unwrap())
            .unwrap(),
        original
    );
    let encoded = serde_json::to_string(&archive).unwrap();
    assert!(encoded.contains("results/解.bin"));
    assert!(encoded.contains(&format!("{:x}", Sha256::digest(original))));
    assert!(
        archive["tables"]["file_changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn shared_artifact_is_not_available_or_authorized_in_local_agent_turn() {
    let config = ResolvedConfig::default();
    let llm = Arc::new(PublishThenAnswer {
        shared: false,
        store: Mutex::new(None),
    });
    let (service, store, workspace, _runtime) =
        run_service_fixture_with_llm(config.clone(), llm.clone()).await;
    *llm.store.lock().unwrap() = Some(store);
    let mut request = control_run_request(
        config,
        &workspace,
        crate::session::SessionId::new(),
        "Answer locally",
    );
    request.session_id = None;
    request.agent_confirmation = Some(crate::cli::SharedConfirmationPrompt::new(NoPrompt));
    service
        .execute(
            crate::app::AppCommand::Run(request),
            &mut crate::cli::HumanRenderer::new(),
            &mut NoPrompt,
        )
        .await
        .unwrap();
}

struct PublishBatch;

#[async_trait::async_trait(?Send)]
impl crate::llm::LlmClient for PublishBatch {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        _cancel: tokio_util::sync::CancellationToken,
        sink: &mut dyn LlmEventSink,
    ) -> Result<LlmResponseSummary, crate::error::LlmError> {
        let finish_reason = if request
            .messages
            .iter()
            .any(|message| matches!(message, ModelMessage::Tool { .. }))
        {
            sink.push(LlmEvent::TextDelta(
                "Finished publishing the files that fit.".into(),
            ))?;
            crate::session::FinishReason::Stop
        } else {
            // One model response asks for five reads. The Agent must commit each result
            // before the next capacity check; four fit, and the fifth is a tool failure.
            for index in 0..5 {
                let call_id = format!("artifact-{index}");
                sink.push(LlmEvent::ToolCallStart {
                    call_id: call_id.clone(),
                    tool_name: "shared_publish_artifact".into(),
                })?;
                sink.push(LlmEvent::ToolCallArgsDelta {
                    call_id,
                    delta: json!({"path":"solver.bin","name":format!("result-{index}.bin")})
                        .to_string(),
                })?;
            }
            crate::session::FinishReason::ToolCall
        };
        Ok(LlmResponseSummary {
            finish_reason,
            usage: None,
            response_id: None,
        })
    }
}

#[tokio::test]
async fn shared_artifact_batch_stays_within_archive_sidecar_budget() {
    let mut config = ResolvedConfig::default();
    config.multi_agent.enabled = false;
    let (service, store, workspace, _runtime) =
        run_service_fixture_with_llm(config.clone(), Arc::new(PublishBatch)).await;
    std::fs::write(
        workspace.authority_root().join("solver.bin"),
        vec![42; 8 * 1024 * 1024],
    )
    .unwrap();
    let mut request = control_run_request(
        config,
        &workspace,
        crate::session::SessionId::new(),
        "Attach the solver output files",
    );
    request.session_id = None;
    request.agent_confirmation = Some(crate::cli::SharedConfirmationPrompt::new(NoPrompt));
    let outcome = service
        .execute_shared(
            request,
            SharedRunContext {
                job_id: "artifact-batch".into(),
                attempt_id: "test-attempt".into(),
                generation: 1,
                project_id: "project".into(),
                environment_id: "solver".into(),
                allowed_child_environments: Vec::new(),
                allowed_child_candidates: Vec::new(),
                resume: None,
                continuation: None,
            },
            &mut crate::cli::HumanRenderer::new(),
            &mut NoPrompt,
        )
        .await
        .unwrap();
    let SharedRunOutcome::Completed(summary) = outcome else {
        panic!("artifact tool must not suspend")
    };
    assert_eq!(summary.tool_call_count(), 5);
    assert_eq!(
        summary.failed_tool_count(),
        1,
        "capacity must be rejected before reporting a successful publication"
    );
    let archive = store
        .session_repo()
        .export_shared_archive("artifact-batch", store.paths())
        .unwrap()
        .unwrap();
    assert_eq!(archive["sidecars"].as_array().unwrap().len(), 4);
    assert_eq!(
        archive["tables"]["tool_calls"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|call| call["truncated_output_path"].is_string())
            .count(),
        4
    );
}
