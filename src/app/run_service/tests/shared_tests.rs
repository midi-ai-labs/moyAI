use super::*;
use crate::agent::shared::{
    SharedCheckpointSettlement, SharedResume, SharedRunContext, SharedRunOutcome, SharedYield,
};
use crate::llm::{ChatRequest, LlmEvent, LlmEventSink, LlmResponseSummary, ModelMessage};
use crate::session::{FinishReason, SessionId, SessionRepository, SessionStatus};
use serde_json::json;
use std::sync::Mutex;

#[derive(Default)]
struct ChildThenAnswer {
    requests: Mutex<Vec<ChatRequest>>,
    delegate_after_result: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait(?Send)]
impl crate::llm::LlmClient for ChildThenAnswer {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        _cancel: tokio_util::sync::CancellationToken,
        sink: &mut dyn LlmEventSink,
    ) -> Result<LlmResponseSummary, crate::error::LlmError> {
        let has_result = request
            .messages
            .iter()
            .any(|message| matches!(message, ModelMessage::Tool { .. }));
        assert!(
            request
                .tools
                .iter()
                .any(|tool| tool.name == "shared_delegate")
        );
        assert!(
            !request
                .tools
                .iter()
                .any(|tool| matches!(tool.name.as_str(), "wait_remote_tasks" | "spawn_agent"))
        );
        self.requests.lock().unwrap().push(request);
        let finish_reason = if has_result
            && !self
                .delegate_after_result
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            sink.push(LlmEvent::TextDelta(
                "The child result was consumed exactly once.".into(),
            ))?;
            FinishReason::Stop
        } else {
            let call_id = if has_result {
                "next-shared-child-call"
            } else {
                "shared-child-call"
            };
            sink.push(LlmEvent::ToolCallStart {
                call_id: call_id.into(),
                tool_name: "shared_delegate".into(),
            })?;
            sink.push(LlmEvent::ToolCallArgsDelta {
                call_id: call_id.into(),
                delta: json!({"environment_id":"solver","title":"Solve","prompt":"Compute 2 + 2"})
                    .to_string(),
            })?;
            FinishReason::ToolCall
        };
        Ok(LlmResponseSummary {
            finish_reason,
            usage: None,
            response_id: None,
        })
    }
}

fn context() -> SharedRunContext {
    SharedRunContext {
        job_id: "parent-job".into(),
        project_context: None,
        attempt_id: "test-attempt".into(),
        generation: 1,
        project_id: "shared-project".into(),
        environment_id: "general".into(),
        allowed_child_environments: vec!["solver".into()],
        allowed_child_candidates: Vec::new(),
        resume: None,
        continuation: None,
    }
}

fn request(
    config: ResolvedConfig,
    workspace: &crate::workspace::Workspace,
) -> crate::app::RunRequest {
    let mut request = control_run_request(
        config,
        workspace,
        SessionId::new(),
        "Delegate the computation and use its result",
    );
    request.session_id = None;
    request.agent_confirmation = Some(crate::cli::SharedConfirmationPrompt::new(NoPrompt));
    request
}

fn resume(yielded: &SharedYield) -> SharedRunContext {
    SharedRunContext {
        resume: Some(SharedResume {
            archive: None,
            checkpoint: yielded.checkpoint.clone(),
            child_result: json!({"id":"child-job","parent_id":"parent-job","project_id":"shared-project","environment_id":"solver","input":yielded.child.input,"state":"succeeded","result":{"answer":4}}),
        }),
        ..context()
    }
}

fn shared_turn_clocks(
    store: &StoreBundle,
    yielded: &SharedYield,
) -> Vec<crate::context::current_time::CurrentTimeSnapshot> {
    store
        .protocol_event_store()
        .list_history_items(yielded.session_id, yielded.turn_id)
        .unwrap()
        .into_iter()
        .filter_map(|item| match item.payload {
            crate::protocol::HistoryItemPayload::WorldState { snapshot, .. } => Some(
                serde_json::from_value(snapshot.sections["current_time"]["snapshot"].clone())
                    .unwrap(),
            ),
            _ => None,
        })
        .collect()
}

fn seed_world_state_clock(
    store: &StoreBundle,
    session_id: SessionId,
    turn_id: crate::protocol::TurnId,
    sequence_no: i64,
    clock_section: Option<serde_json::Value>,
) {
    let mut snapshot = crate::context::world_state::WorldStateSnapshot::default();
    if let Some(section) = clock_section {
        snapshot.sections.insert("current_time".into(), section);
    }
    store
        .protocol_event_store()
        .seed_history_item_for_test(&crate::protocol::HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            scope: crate::protocol::HistoryScope::Turn { turn_id },
            sequence_no,
            created_at_ms: 0,
            payload: crate::protocol::HistoryItemPayload::WorldState {
                snapshot,
                rendered: String::new(),
            },
        })
        .unwrap();
}

#[tokio::test]
async fn shared_turn_start_time_uses_first_snapshot_of_exact_session_and_turn() {
    let (_service, store, _workspace, _runtime) =
        run_service_fixture(ResolvedConfig::default()).await;
    let session = SessionId::new();
    let other_session = SessionId::new();
    let turn = crate::protocol::TurnId::new();
    let other_turn = crate::protocol::TurnId::new();
    let first = crate::context::current_time::CurrentTimeSnapshot {
        utc: "1970-01-01T00:00:00Z".into(),
        local: "1970-01-01T00:00:00+00:00".into(),
        timezone: "+00:00".into(),
        unix_ms: 0,
    };
    let later = crate::context::current_time::CurrentTimeSnapshot {
        utc: "1970-01-01T00:00:01Z".into(),
        local: "1970-01-01T00:00:01+00:00".into(),
        unix_ms: 1000,
        ..first.clone()
    };
    for (session_id, turn_id, sequence, clock) in [
        (session, turn, 1, &first),
        (session, turn, 2, &later),
        (session, other_turn, 1, &later),
        (other_session, turn, 1, &later),
    ] {
        seed_world_state_clock(
            &store,
            session_id,
            turn_id,
            sequence,
            Some(json!({"snapshot": clock})),
        );
    }
    let repo = store.session_repo();
    assert_eq!(
        repo.shared_turn_start_time(session, turn).unwrap(),
        Some(first)
    );
    assert_eq!(
        repo.shared_turn_start_time(session, other_turn).unwrap(),
        Some(later.clone())
    );
    assert_eq!(
        repo.shared_turn_start_time(other_session, turn).unwrap(),
        Some(later)
    );
    assert_eq!(
        repo.shared_turn_start_time(session, crate::protocol::TurnId::new())
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn shared_turn_start_time_allows_missing_or_legacy_clock_snapshots() {
    let (_service, store, _workspace, _runtime) =
        run_service_fixture(ResolvedConfig::default()).await;
    let session = SessionId::new();
    for clock in [
        None,
        Some(json!({})),
        Some(json!({"snapshot": {"utc": "1970-01-01T00:00:00Z"}})),
    ] {
        let turn = crate::protocol::TurnId::new();
        seed_world_state_clock(&store, session, turn, 1, clock);
        assert_eq!(
            store
                .session_repo()
                .shared_turn_start_time(session, turn)
                .unwrap(),
            None,
            "older clock projections must leave the existing current-time fallback available"
        );
    }
}

fn hub_parent(yielded: &SharedYield, state: &str) -> serde_json::Value {
    json!({"id":"parent-job","project_id":"shared-project","environment_id":"general",
        "checkpoint":yielded.checkpoint,"state":state,"result":null})
}

async fn pause() -> (
    Arc<super::super::RunService>,
    StoreBundle,
    crate::workspace::Workspace,
    Arc<crate::app::AgentRuntime>,
    Arc<ChildThenAnswer>,
    ResolvedConfig,
    SharedYield,
) {
    pause_named("parent-job").await
}

async fn pause_named(
    job: &str,
) -> (
    Arc<super::super::RunService>,
    StoreBundle,
    crate::workspace::Workspace,
    Arc<crate::app::AgentRuntime>,
    Arc<ChildThenAnswer>,
    ResolvedConfig,
    SharedYield,
) {
    pause_with_project(job, None).await
}

async fn pause_with_project(
    job: &str,
    project_context: Option<crate::context::world_state::SharedProjectContext>,
) -> (
    Arc<super::super::RunService>,
    StoreBundle,
    crate::workspace::Workspace,
    Arc<crate::app::AgentRuntime>,
    Arc<ChildThenAnswer>,
    ResolvedConfig,
    SharedYield,
) {
    let mut config = ResolvedConfig::default();
    config.model.model = "scripted".into();
    config.model.base_url = "http://local".into();
    config.model.provider_profile = crate::config::ProviderProfile::OpenAiCompatible;
    config.multi_agent.enabled = false;
    let llm = Arc::new(ChildThenAnswer::default());
    let (service, store, workspace, runtime) =
        run_service_fixture_with_llm(config.clone(), llm.clone()).await;
    let outcome = service
        .execute_shared(
            request(config.clone(), &workspace),
            SharedRunContext {
                job_id: job.into(),
                project_context,
                ..context()
            },
            &mut crate::cli::HumanRenderer::new(),
            &mut NoPrompt,
        )
        .await
        .unwrap();
    let SharedRunOutcome::Yielded(yielded) = outcome else {
        panic!("a child wait must not manufacture a completed turn")
    };
    (service, store, workspace, runtime, llm, config, yielded)
}

#[tokio::test]
async fn shared_project_snapshot_reaches_provider_on_initial_and_checkpoint_resumed_steps() {
    shared_project_checkpoint_fixture(false).await;
}

#[tokio::test]
async fn shared_root_checkpoint_keeps_canonical_request_without_state_duplication() {
    shared_project_checkpoint_fixture(true).await;
}

async fn shared_project_checkpoint_fixture(root: bool) {
    let project = crate::context::world_state::SharedProjectContext {
        project_id: "shared-project".into(),
        label: "Text analysis team".into(),
        overview: "PROJECT_OVERVIEW_SNAPSHOT: general is API, solver is Worker".into(),
        revision: "17".into(),
        root_prompt: if root {
            ""
        } else {
            "Build and verify the distributed application"
        }
        .into(),
        origin_device_id: Some("WinA".into()),
    };
    let (service, _store, workspace, _runtime, llm, config, yielded) =
        pause_with_project("parent-job", Some(project.clone())).await;
    let mut resumed = resume(&yielded);
    resumed.project_context = Some(project);
    let outcome = service
        .execute_shared(
            request(config, &workspace),
            resumed,
            &mut crate::cli::HumanRenderer::new(),
            &mut NoPrompt,
        )
        .await
        .unwrap();
    assert!(matches!(outcome, SharedRunOutcome::Completed(_)));
    let requests = llm.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        assert_eq!(
            request
                .system_prompt
                .matches("PROJECT_OVERVIEW_SNAPSHOT")
                .count(),
            1
        );
        assert!(
            request
                .system_prompt
                .contains("<origin_device_id>WinA</origin_device_id>")
        );
        assert!(
            request
                .system_prompt
                .contains("<current_environment_id>general</current_environment_id>")
        );
        assert!(
            request
                .system_prompt
                .contains("<overview_revision>17</overview_revision>")
        );
        if root {
            assert!(!request.system_prompt.contains("<overall_request>"));
            assert!(
                serde_json::to_string(&request.messages)
                    .unwrap()
                    .contains("Delegate the computation and use its result")
            );
        } else {
            assert!(
                request
                    .system_prompt
                    .contains("Build and verify the distributed application")
            );
        }
    }
}

#[derive(Default)]
struct ContextAnswer {
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait::async_trait(?Send)]
impl crate::llm::LlmClient for ContextAnswer {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        _cancel: tokio_util::sync::CancellationToken,
        sink: &mut dyn LlmEventSink,
    ) -> Result<LlmResponseSummary, crate::error::LlmError> {
        self.requests.lock().unwrap().push(request);
        sink.push(LlmEvent::TextDelta("Completed the assigned scope.".into()))?;
        Ok(LlmResponseSummary {
            finish_reason: FinishReason::Stop,
            usage: None,
            response_id: None,
        })
    }
}

#[tokio::test]
async fn child_and_redelegated_context_keep_origin_but_identify_their_own_execution_environment() {
    let project = crate::context::world_state::SharedProjectContext {
        project_id: "shared-project".into(),
        label: "Text analysis team".into(),
        overview: "general=API; solver=Worker; database=DB".into(),
        revision: "18".into(),
        root_prompt: "Build and verify the async application".into(),
        origin_device_id: Some("WinA".into()),
    };
    for environment in ["solver", "database"] {
        let mut config = ResolvedConfig::default();
        config.model.model = "scripted".into();
        config.model.base_url = "http://local".into();
        config.model.provider_profile = crate::config::ProviderProfile::OpenAiCompatible;
        config.multi_agent.enabled = false;
        let llm = Arc::new(ContextAnswer::default());
        let (service, _store, workspace, _runtime) =
            run_service_fixture_with_llm(config.clone(), llm.clone()).await;
        let mut child_request = request(config, &workspace);
        child_request.prompt = "全体目標: 非同期解析。工程: 接続契約の検証。担当: この環境の保存を調べる。完了条件: 永続化を確認。返す成果: 試験結果と未解決事項。".into();
        let child_context = SharedRunContext {
            job_id: format!("child-{environment}"),
            environment_id: environment.into(),
            project_context: Some(project.clone()),
            ..context()
        };
        let outcome = service
            .execute_shared(
                child_request.clone(),
                child_context,
                &mut crate::cli::HumanRenderer::new(),
                &mut NoPrompt,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, SharedRunOutcome::Completed(_)));
        let requests = llm.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let provider = &requests[0];
        assert!(provider.system_prompt.contains(&format!(
            "<current_environment_id>{environment}</current_environment_id>"
        )));
        assert!(
            provider
                .system_prompt
                .contains("<origin_device_id>WinA</origin_device_id>")
        );
        assert!(
            provider
                .system_prompt
                .contains("general=API; solver=Worker; database=DB")
        );
        assert!(
            provider
                .system_prompt
                .contains("Build and verify the async application")
        );
        assert!(
            serde_json::to_string(&provider.messages)
                .unwrap()
                .contains(&child_request.prompt)
        );
    }
}

#[tokio::test]
async fn shared_archive_restores_into_another_store_without_replaying_completed_effects() {
    let (_source, source_store, _source_workspace, _source_runtime, source_llm, _, yielded) =
        pause().await;
    let original_clock = shared_turn_clocks(&source_store, &yielded)[0].clone();
    let archive = source_store
        .session_repo()
        .export_shared_archive("parent-job", source_store.paths())
        .unwrap()
        .unwrap();
    let (target, target_store, workspace, _target_runtime, target_llm, config, other) =
        pause_named("unrelated-job").await;
    let mut continuation = resume(&yielded);
    continuation.resume.as_mut().unwrap().archive = Some(archive.clone());
    assert!(
        target_store
            .session_repo()
            .get_session(yielded.session_id)
            .await
            .is_err()
    );
    let result = target
        .execute_shared(
            request(config.clone(), &workspace),
            continuation.clone(),
            &mut crate::cli::HumanRenderer::new(),
            &mut NoPrompt,
        )
        .await
        .unwrap();
    assert!(matches!(result, SharedRunOutcome::Completed(_)));
    assert_eq!(
        shared_turn_clocks(&target_store, &yielded),
        vec![original_clock.clone(), original_clock],
        "portable resume must retain this turn's clock instead of the target store's clock"
    );
    assert_eq!(source_llm.requests.lock().unwrap().len(), 1);
    assert_eq!(target_llm.requests.lock().unwrap().len(), 2); // unrelated pause, then only the resumed model response
    assert_eq!(
        target_store
            .session_repo()
            .get_session(other.session_id)
            .await
            .unwrap()
            .status,
        SessionStatus::Running
    );
    assert!(
        target
            .execute_shared(
                request(config, &workspace),
                continuation,
                &mut crate::cli::HumanRenderer::new(),
                &mut NoPrompt
            )
            .await
            .is_err()
    );
    assert_eq!(target_llm.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn shared_archive_completed_history_continues_as_a_new_job_without_old_effects() {
    let (source, source_store, source_workspace, _source_runtime, source_llm, config, yielded) =
        pause().await;
    source
        .execute_shared(
            request(config.clone(), &source_workspace),
            resume(&yielded),
            &mut crate::cli::HumanRenderer::new(),
            &mut NoPrompt,
        )
        .await
        .unwrap();
    let archive = source_store
        .session_repo()
        .export_shared_archive("parent-job", source_store.paths())
        .unwrap()
        .unwrap();
    let target_llm = Arc::new(ChildThenAnswer::default());
    let (target, target_store, workspace, _runtime) =
        run_service_fixture_with_llm(config.clone(), target_llm.clone()).await;
    let continued = SharedRunContext {
        job_id: "continued-job".into(),
        continuation: Some(crate::agent::shared::SharedContinuation {
            previous_job_id: "parent-job".into(),
            archive,
        }),
        ..context()
    };
    let mut next = request(config, &workspace);
    next.prompt = "Explain the previous result".into();
    let outcome = target
        .execute_shared(
            next,
            continued,
            &mut crate::cli::HumanRenderer::new(),
            &mut NoPrompt,
        )
        .await
        .unwrap();
    let SharedRunOutcome::Completed(summary) = outcome else {
        panic!("continuation must use the retained child result")
    };
    assert_ne!(summary.session_id(), yielded.session_id);
    assert_eq!(source_llm.requests.lock().unwrap().len(), 2);
    assert_eq!(target_llm.requests.lock().unwrap().len(), 1);
    let saved = target_store
        .session_repo()
        .export_shared_archive("continued-job", target_store.paths())
        .unwrap()
        .unwrap();
    let transcript = saved["transcript"].to_string();
    assert!(transcript.contains("Explain the previous result"));
    assert!(transcript.contains("shared-child-call"));
    assert!(
        target_store
            .session_repo()
            .get_session(yielded.session_id)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn shared_archive_output_sidecar_survives_a_different_store_and_retains_session_ownership() {
    let (_, source_store, _, _source_runtime, _, _, yielded) = pause().await;
    let original = source_store
        .paths()
        .truncation_dir
        .join("shared-long-output.txt");
    let bytes = b"a retained tool output\nwith its complete original contents";
    std::fs::write(&original, bytes).unwrap();
    let db = rusqlite::Connection::open(&source_store.paths().database_path).unwrap();
    assert_eq!(db.execute("UPDATE tool_calls SET truncated_output_path=?1 WHERE history_item_id IN (SELECT id FROM protocol_history_items WHERE session_id=?2)",rusqlite::params![original.as_str(),yielded.session_id.to_string()]).unwrap(),1);
    drop(db);
    let archive = source_store
        .session_repo()
        .export_shared_archive("parent-job", source_store.paths())
        .unwrap()
        .unwrap();
    let (_, target_store, workspace, _target_runtime, _, config, unrelated) =
        pause_named("other-job").await;
    target_store
        .session_repo()
        .restore_shared_archive(
            &resume(&yielded),
            &archive,
            &workspace,
            &config,
            target_store.paths(),
        )
        .unwrap();
    let (local, _) = target_store
        .session_repo()
        .shared_history_file(yielded.session_id, &original)
        .unwrap()
        .unwrap();
    assert_ne!(local, original);
    assert_eq!(std::fs::read(&local).unwrap(), bytes);
    assert!(
        target_store
            .session_repo()
            .shared_history_file(unrelated.session_id, &original)
            .unwrap()
            .is_none()
    );
    let reopened = SqliteStore::open(target_store.paths()).unwrap();
    reopened.cleanup_orphan_internal_files().unwrap();
    assert!(
        local.exists(),
        "retention must preserve the imported session's output files"
    );
    let exported = target_store
        .session_repo()
        .export_shared_archive("parent-job", target_store.paths())
        .unwrap()
        .unwrap();
    assert_eq!(
        exported["sidecars"], archive["sidecars"],
        "re-export reads the imported local file, retaining its original reference"
    );
}

#[tokio::test]
async fn shared_archive_rejects_foreign_rows_and_changed_history_atomically() {
    let (_, source_store, _, _source_runtime, _, _, yielded) = pause().await;
    let archive = source_store
        .session_repo()
        .export_shared_archive("parent-job", source_store.paths())
        .unwrap()
        .unwrap();
    let (_, target_store, workspace, _target_runtime, _, config, _) =
        pause_named("other-job").await;
    for mutate in [0, 1, 2] {
        let mut changed = archive.clone();
        match mutate {
            0 => {
                changed["tables"]["protocol_history_items"][0]["session_id"] =
                    json!(SessionId::new().to_string())
            }
            1 => changed["tables"]["protocol_history_items"][0]["payload_json"] = json!("{}"),
            _ => changed["session"]["access_mode"] = json!("full_access"),
        }
        assert!(
            target_store
                .session_repo()
                .restore_shared_archive(
                    &resume(&yielded),
                    &changed,
                    &workspace,
                    &config,
                    target_store.paths()
                )
                .is_err()
        );
        assert!(
            target_store
                .session_repo()
                .get_session(yielded.session_id)
                .await
                .is_err()
        );
    }
    let encoded = serde_json::to_string(&archive).unwrap();
    assert!(!encoded.contains("provider_connection_json"));
    assert!(!encoded.contains("api_key_env"));
}

#[tokio::test]
async fn shared_archive_v68_forward_migration_preserves_v67_paused_execution() {
    let (_, store, _, _runtime, _, _, yielded) = pause().await;
    let db = rusqlite::Connection::open(&store.paths().database_path).unwrap();
    db.execute_batch(
        "DROP TABLE shared_history_files; DELETE FROM moyai_schema_migrations WHERE version=68;",
    )
    .unwrap();
    drop(db);
    let reopened = SqliteStore::open(store.paths()).unwrap();
    reopened.migrate().unwrap();
    reopened.migrate().unwrap();
    let reopened = StoreBundle::new(reopened);
    let saved = reopened
        .session_repo()
        .export_shared_archive("parent-job", reopened.paths())
        .unwrap()
        .unwrap();
    assert_eq!(saved["schema_version"], 68);
    assert!(saved["sidecars"].as_array().unwrap().is_empty());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            saved["tables"]["shared_run_checkpoints"][0]["checkpoint_json"]
                .as_str()
                .unwrap()
        )
        .unwrap(),
        yielded.checkpoint
    );
}

#[tokio::test]
async fn shared_checkpoint_resumes_same_turn_and_consumes_child_once_after_reopen() {
    let (service, store, workspace, _runtime, llm, config, yielded) = pause().await;
    let original_clock = shared_turn_clocks(&store, &yielded)[0].clone();
    assert_eq!(llm.requests.lock().unwrap().len(), 1);
    assert_eq!(
        store
            .session_repo()
            .get_session(yielded.session_id)
            .await
            .unwrap()
            .status,
        SessionStatus::Running
    );
    assert!(
        store
            .session_repo()
            .durable_terminal_for_turn(yielded.session_id, yielded.turn_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!store.active_runs().is_active(yielded.session_id));
    let lease = store
        .try_acquire_run_process_lease(yielded.session_id)
        .unwrap();
    drop(lease);
    let reopened = SqliteStore::open(store.paths()).unwrap();
    reopened.migrate().unwrap();
    let reopened = StoreBundle::new(reopened);
    crate::session::SessionService::new(reopened.clone())
        .mark_stale_running_sessions("startup recovery")
        .await
        .unwrap();
    assert_eq!(
        reopened
            .session_repo()
            .get_session(yielded.session_id)
            .await
            .unwrap()
            .status,
        SessionStatus::Running
    );
    assert!(
        reopened
            .session_repo()
            .admit_session_turn(yielded.session_id, crate::protocol::TurnId::new())
            .await
            .unwrap()
            .is_none(),
        "ordinary local admission cannot take a shared paused turn"
    );
    let updated_instruction = "When reporting checks, include the executed command.";
    std::fs::write(workspace.root.join("AGENTS.md"), updated_instruction).unwrap();
    let outcome = service
        .execute_shared(
            request(config.clone(), &workspace),
            resume(&yielded),
            &mut crate::cli::HumanRenderer::new(),
            &mut NoPrompt,
        )
        .await
        .unwrap();
    let SharedRunOutcome::Completed(summary) = outcome else {
        panic!("expected continuation completion")
    };
    assert_eq!(summary.turn_id(), yielded.turn_id);
    assert_eq!(summary.session_id(), yielded.session_id);
    assert_eq!(summary.status(), SessionStatus::Completed);
    assert_eq!(summary.tool_call_count(), 1);
    assert_eq!(summary.metrics().model_request_count, 2);
    assert_eq!(
        shared_turn_clocks(&store, &yielded),
        vec![original_clock.clone(), original_clock],
        "resuming the same canonical turn must keep its original clock, including milliseconds"
    );
    assert_eq!(
        store
            .session_repo()
            .settle_shared_checkpoint_terminal(
                &yielded.checkpoint,
                &hub_parent(&yielded, "waiting_child"),
            )
            .unwrap(),
        SharedCheckpointSettlement::NoLongerPaused,
        "an old accepted yield is retired even while the Hub still has a nonterminal job"
    );
    assert_eq!(
        store
            .session_repo()
            .settle_shared_checkpoint_terminal(
                &yielded.checkpoint,
                &hub_parent(&yielded, "cancelled"),
            )
            .unwrap(),
        SharedCheckpointSettlement::NoLongerPaused,
        "a late terminal for the previous checkpoint cannot replace a resumed turn's result"
    );
    let requests = llm.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].system_prompt.contains(updated_instruction));
    assert!(requests[1].system_prompt.contains(updated_instruction));
    assert_eq!(
        requests[1]
            .messages
            .iter()
            .filter(|message| matches!(message, ModelMessage::Tool { .. }))
            .count(),
        1
    );
    drop(requests);
    assert!(
        service
            .execute_shared(
                request(config, &workspace),
                resume(&yielded),
                &mut crate::cli::HumanRenderer::new(),
                &mut NoPrompt
            )
            .await
            .is_err()
    );
    assert_eq!(llm.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn shared_cancelled_checkpoint_settles_after_reopen_and_releases_local_delete_gates() {
    let (service, store, workspace, _runtime, llm, _config, yielded) = pause().await;
    assert!(
        service
            .session_service
            .delete_session(yielded.session_id)
            .await
            .is_err()
    );
    assert!(
        service
            .session_service
            .delete_project(workspace.project_id)
            .await
            .is_err()
    );
    let Some(crate::session::ActiveTurnExpectation::Turn { revision, .. }) = store
        .session_repo()
        .active_turn_expectation_for_session(yielded.session_id)
        .await
        .unwrap()
    else {
        panic!("paused shared turn retains its exact admission")
    };
    service
        .cancel_exact_root_execution(yielded.session_id, yielded.turn_id, revision)
        .await
        .unwrap();
    assert!(
        store
            .session_repo()
            .durable_terminal_for_turn(yielded.session_id, yielded.turn_id)
            .await
            .unwrap()
            .is_none()
    );

    let reopened = SqliteStore::open(store.paths()).unwrap();
    reopened.migrate().unwrap();
    let reopened = StoreBundle::new(reopened);
    let sessions = crate::session::SessionService::new(reopened.clone());
    sessions
        .mark_stale_running_sessions("startup recovery")
        .await
        .unwrap();
    assert_eq!(
        sessions
            .get_session(yielded.session_id)
            .await
            .unwrap()
            .status,
        SessionStatus::Running
    );
    assert_eq!(
        reopened
            .session_repo()
            .settle_shared_checkpoint_terminal(
                &yielded.checkpoint,
                &hub_parent(&yielded, "cancelled"),
            )
            .unwrap(),
        SharedCheckpointSettlement::Applied
    );
    assert_eq!(
        sessions
            .get_session(yielded.session_id)
            .await
            .unwrap()
            .status,
        SessionStatus::Cancelled
    );
    let terminal = reopened
        .session_repo()
        .durable_terminal_for_turn(yielded.session_id, yielded.turn_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        terminal.outcome,
        crate::protocol::TurnTerminalOutcome::Interrupted {
            cause: crate::protocol::TurnInterruptionCause::UserStop,
        }
    ));
    assert_eq!(terminal.tool_call_count, 1);
    assert_eq!(terminal.metrics.model_request_count, 1);
    assert!(
        reopened
            .session_repo()
            .mutation_blocker_in_session_tree(yielded.session_id)
            .await
            .unwrap()
            .is_none()
    );
    let connection = rusqlite::Connection::open(&store.paths().database_path).unwrap();
    let released: bool = connection.query_row(
        "SELECT active_run_id IS NULL AND active_turn_id IS NULL AND active_run_lease_expires_at_ms IS NULL FROM sessions WHERE id=?1",
        [yielded.session_id.to_string()], |row| row.get(0),
    ).unwrap();
    assert!(released);
    let tool_status: String = connection
        .query_row(
            "SELECT status FROM tool_calls WHERE id=?1",
            [yielded.checkpoint["tool_call_id"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tool_status, "cancelled");
    let pending: u32 = connection
        .query_row(
            "SELECT COUNT(*) FROM shared_run_checkpoints WHERE state='paused'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0);
    assert_eq!(
        reopened
            .session_repo()
            .settle_shared_checkpoint_terminal(
                &yielded.checkpoint,
                &hub_parent(&yielded, "cancelled")
            )
            .unwrap(),
        SharedCheckpointSettlement::NoLongerPaused
    );
    let terminals: u32 = connection.query_row("SELECT COUNT(*) FROM protocol_runtime_events WHERE session_id=?1 AND json_extract(msg_json,'$.kind')='turn_terminal'",
        [yielded.session_id.to_string()], |row| row.get(0)).unwrap();
    assert_eq!(terminals, 1);
    assert_eq!(llm.requests.lock().unwrap().len(), 1);
    assert!(
        reopened
            .session_repo()
            .admit_session_turn(yielded.session_id, crate::protocol::TurnId::new())
            .await
            .unwrap()
            .is_none()
    );
    sessions.delete_session(yielded.session_id).await.unwrap();
    sessions.delete_project(workspace.project_id).await.unwrap();
}

#[tokio::test]
async fn shared_failed_checkpoint_settles_pending_child_and_allows_project_delete() {
    let (_service, store, workspace, _runtime, llm, _config, yielded) = pause().await;
    assert_eq!(
        store
            .session_repo()
            .settle_shared_checkpoint_terminal(&yielded.checkpoint, &hub_parent(&yielded, "failed"))
            .unwrap(),
        SharedCheckpointSettlement::Applied
    );
    let terminal = store
        .session_repo()
        .durable_terminal_for_turn(yielded.session_id, yielded.turn_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        terminal.outcome,
        crate::protocol::TurnTerminalOutcome::Failed { .. }
    ));
    assert_eq!(terminal.failed_tool_count, 1);
    assert_eq!(
        store
            .session_repo()
            .get_session(yielded.session_id)
            .await
            .unwrap()
            .status,
        SessionStatus::Failed
    );
    let connection = rusqlite::Connection::open(&store.paths().database_path).unwrap();
    let tool_status: String = connection
        .query_row(
            "SELECT status FROM tool_calls WHERE id=?1",
            [yielded.checkpoint["tool_call_id"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tool_status, "failed");
    crate::session::SessionService::new(store)
        .delete_project(workspace.project_id)
        .await
        .unwrap();
    assert_eq!(llm.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn shared_rejected_second_yield_settles_with_the_hubs_previous_checkpoint() {
    let (service, store, workspace, _runtime, llm, config, first) = pause().await;
    llm.delegate_after_result
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let outcome = service
        .execute_shared(
            request(config.clone(), &workspace),
            resume(&first),
            &mut crate::cli::HumanRenderer::new(),
            &mut NoPrompt,
        )
        .await
        .unwrap();
    let SharedRunOutcome::Yielded(second) = outcome else {
        panic!("the same parent must reach a second real child checkpoint")
    };
    assert_eq!(first.session_id, second.session_id);
    assert_eq!(first.turn_id, second.turn_id);
    assert_ne!(
        first.checkpoint["checkpoint_id"],
        second.checkpoint["checkpoint_id"]
    );
    assert_eq!(llm.requests.lock().unwrap().len(), 2);
    assert!(
        service
            .execute_shared(
                request(config, &workspace),
                resume(&first),
                &mut crate::cli::HumanRenderer::new(),
                &mut NoPrompt,
            )
            .await
            .is_err(),
        "resume must still reject the old checkpoint"
    );

    // A rejected second Yield does not replace Hub.job.checkpoint. Its fallback Finished
    // terminal retains the first accepted checkpoint even though local storage has the second.
    let hub_failed = hub_parent(&first, "failed");
    assert_eq!(
        store
            .session_repo()
            .settle_shared_checkpoint_terminal(&second.checkpoint, &hub_failed,)
            .expect("confirmed job failure must settle the exact local second checkpoint"),
        SharedCheckpointSettlement::Applied
    );
    let terminal = store
        .session_repo()
        .durable_terminal_for_turn(second.session_id, second.turn_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        terminal.outcome,
        crate::protocol::TurnTerminalOutcome::Failed { .. }
    ));
    assert_eq!(terminal.tool_call_count, 2);
    assert_eq!(terminal.failed_tool_count, 1);
    assert_eq!(terminal.metrics.model_request_count, 2);
    assert_eq!(
        store
            .session_repo()
            .get_session(second.session_id)
            .await
            .unwrap()
            .status,
        SessionStatus::Failed
    );
    let connection = rusqlite::Connection::open(&store.paths().database_path).unwrap();
    let tool_states: Vec<String> = [
        first.checkpoint["tool_call_id"].as_str().unwrap(),
        second.checkpoint["tool_call_id"].as_str().unwrap(),
    ]
    .into_iter()
    .map(|id| {
        connection
            .query_row("SELECT status FROM tool_calls WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap()
    })
    .collect();
    assert_eq!(tool_states, vec!["completed", "failed"]);
    assert_eq!(llm.requests.lock().unwrap().len(), 2);
    assert_eq!(
        store
            .session_repo()
            .settle_shared_checkpoint_terminal(&second.checkpoint, &hub_failed)
            .unwrap(),
        SharedCheckpointSettlement::NoLongerPaused
    );
    crate::session::SessionService::new(store)
        .delete_project(workspace.project_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn shared_checkpoint_settlement_rejects_wrong_hub_identity_and_unconfirmed_success() {
    let (_service, store, _workspace, _runtime, llm, _config, yielded) = pause().await;
    let repo = store.session_repo();
    for state in [
        "waiting_child",
        "queued",
        "assigned",
        "running",
        "cancelling",
    ] {
        assert_eq!(
            repo.settle_shared_checkpoint_terminal(
                &yielded.checkpoint,
                &hub_parent(&yielded, state)
            )
            .unwrap(),
            SharedCheckpointSettlement::Pending
        );
    }
    for (field, value) in [
        ("id", json!("another-job")),
        ("project_id", json!("another-project")),
        ("environment_id", json!("another-environment")),
        ("state", json!("succeeded")),
    ] {
        let mut hub = hub_parent(&yielded, "cancelled");
        hub[field] = value;
        assert!(
            repo.settle_shared_checkpoint_terminal(&yielded.checkpoint, &hub)
                .is_err()
        );
    }
    let mut changed = yielded.checkpoint.clone();
    changed["history_digest"] = json!("changed");
    assert!(
        repo.settle_shared_checkpoint_terminal(&changed, &hub_parent(&yielded, "cancelled"))
            .is_err()
    );
    assert_eq!(
        repo.get_session(yielded.session_id).await.unwrap().status,
        SessionStatus::Running
    );
    assert!(
        repo.durable_terminal_for_turn(yielded.session_id, yielded.turn_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(llm.requests.lock().unwrap().len(), 1);
    assert_eq!(
        repo.settle_shared_checkpoint_terminal(
            &yielded.checkpoint,
            &hub_parent(&yielded, "cancelled")
        )
        .unwrap(),
        SharedCheckpointSettlement::Applied
    );
}

#[tokio::test]
async fn shared_resume_rejects_changed_or_unfinished_child_without_reentering_model() {
    let (service, _store, workspace, _runtime, llm, config, yielded) = pause().await;
    for (field, value) in [
        ("parent_id", json!("different-parent")),
        ("state", json!("running")),
        ("environment_id", json!("private")),
        ("input", json!({"version":1,"prompt":"changed"})),
    ] {
        let mut shared = resume(&yielded);
        shared.resume.as_mut().unwrap().child_result[field] = value;
        assert!(
            service
                .execute_shared(
                    request(config.clone(), &workspace),
                    shared,
                    &mut crate::cli::HumanRenderer::new(),
                    &mut NoPrompt
                )
                .await
                .is_err()
        );
    }
    assert_eq!(llm.requests.lock().unwrap().len(), 1);
    assert!(matches!(
        service
            .execute_shared(
                request(config, &workspace),
                resume(&yielded),
                &mut crate::cli::HumanRenderer::new(),
                &mut NoPrompt
            )
            .await
            .unwrap(),
        SharedRunOutcome::Completed(_)
    ));
}

#[tokio::test]
async fn shared_resume_rejects_missing_or_altered_local_checkpoint() {
    let (service, store, workspace, _runtime, llm, config, yielded) = pause().await;
    let mut altered = resume(&yielded);
    altered.resume.as_mut().unwrap().checkpoint["history_digest"] = json!("0".repeat(64));
    assert!(
        service
            .execute_shared(
                request(config.clone(), &workspace),
                altered,
                &mut crate::cli::HumanRenderer::new(),
                &mut NoPrompt
            )
            .await
            .is_err()
    );
    let connection = rusqlite::Connection::open(&store.paths().database_path).unwrap();
    connection
        .execute(
            "DELETE FROM shared_run_checkpoints WHERE job_id = 'parent-job'",
            [],
        )
        .unwrap();
    assert!(
        service
            .execute_shared(
                request(config, &workspace),
                resume(&yielded),
                &mut crate::cli::HumanRenderer::new(),
                &mut NoPrompt
            )
            .await
            .is_err()
    );
    assert_eq!(llm.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn shared_checkpoint_preserves_fixed_permission_and_rejects_changed_environment_mode() {
    let (service, store, workspace, _runtime, llm, mut config, yielded) = pause().await;
    assert!(
        store
            .session_repo()
            .compare_and_set_root_session_access_mode(
                yielded.session_id,
                crate::config::AccessMode::Default,
                crate::config::AccessMode::FullAccess,
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .session_repo()
            .get_session(yielded.session_id)
            .await
            .unwrap()
            .access_mode,
        crate::config::AccessMode::Default
    );
    config.permissions.access_mode = crate::config::AccessMode::FullAccess;
    assert!(
        service
            .execute_shared(
                request(config, &workspace),
                resume(&yielded),
                &mut crate::cli::HumanRenderer::new(),
                &mut NoPrompt
            )
            .await
            .is_err()
    );
    assert_eq!(llm.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn shared_checkpoint_never_resumes_after_an_exact_stop_request() {
    let (service, store, workspace, _runtime, llm, config, yielded) = pause().await;
    let connection = rusqlite::Connection::open(&store.paths().database_path).unwrap();
    let revision: u64 = connection
        .query_row(
            "SELECT revision FROM session_admission_revisions WHERE session_id = ?1",
            [yielded.session_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let outcome = store
        .session_repo()
        .request_exact_execution_interrupt(
            yielded.session_id,
            yielded.turn_id,
            revision,
            crate::protocol::TurnInterruptionCause::UserStop,
        )
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        crate::storage::session_repo::ExactExecutionInterruptRequestSettlement::Recorded
    ));
    assert!(
        service
            .execute_shared(
                request(config, &workspace),
                resume(&yielded),
                &mut crate::cli::HumanRenderer::new(),
                &mut NoPrompt
            )
            .await
            .is_err()
    );
    assert_eq!(llm.requests.lock().unwrap().len(), 1);
}

struct RunningShellThenChild {
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait::async_trait(?Send)]
impl crate::llm::LlmClient for RunningShellThenChild {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        _cancel: tokio_util::sync::CancellationToken,
        sink: &mut dyn LlmEventSink,
    ) -> Result<LlmResponseSummary, crate::error::LlmError> {
        let mut requests = self.requests.lock().unwrap();
        let step = requests.len();
        requests.push(request);
        let finish_reason = match step {
            0 | 1 => {
                let (tool_name, arguments) = if step == 0 {
                    (
                        "shell_start",
                        json!({"command":if cfg!(windows) { "Start-Sleep -Seconds 30" } else { "sleep 30" },"timeout_ms":60000}),
                    )
                } else {
                    assert!(
                        format!("{:?}", requests[step].messages).contains("running"),
                        "the real shell must have started before delegation"
                    );
                    (
                        "shared_delegate",
                        json!({"environment_id":"solver","title":"Solve","prompt":"Compute 2 + 2"}),
                    )
                };
                let call_id = format!("managed-step-{step}");
                sink.push(LlmEvent::ToolCallStart {
                    call_id: call_id.clone(),
                    tool_name: tool_name.into(),
                })?;
                sink.push(LlmEvent::ToolCallArgsDelta {
                    call_id,
                    delta: arguments.to_string(),
                })?;
                FinishReason::ToolCall
            }
            2 => {
                let blocked = requests[step]
                    .messages
                    .iter()
                    .find_map(|message| match message {
                        ModelMessage::Tool {
                            call_id,
                            tool_name,
                            result,
                            ..
                        } if call_id == "managed-step-1" && tool_name == "shared_delegate" => {
                            Some(result.as_str())
                        }
                        _ => None,
                    })
                    .expect("the blocked delegation must return a tool result");
                assert!(
                    blocked.contains("ordinary managed shell commands finish"),
                    "the tool result must explain why an ordinary running shell blocks delegation: {blocked}"
                );
                sink.push(LlmEvent::TextDelta(
                    "Delegation could not release the running shell's resource.".into(),
                ))?;
                FinishReason::Stop
            }
            _ => panic!("unexpected extra sample"),
        };
        Ok(LlmResponseSummary {
            finish_reason,
            usage: None,
            response_id: None,
        })
    }
}

#[tokio::test]
async fn shared_delegate_does_not_checkpoint_while_a_real_managed_process_is_running() {
    let mut config = ResolvedConfig::default();
    config.model.model = "scripted".into();
    config.model.base_url = "http://local".into();
    config.model.provider_profile = crate::config::ProviderProfile::OpenAiCompatible;
    config.permissions.access_mode = crate::config::AccessMode::FullAccess;
    config.multi_agent.enabled = false;
    let llm = Arc::new(RunningShellThenChild {
        requests: Mutex::new(Vec::new()),
    });
    let shells = crate::tool::shell::ManagedShells::default();
    let (service, store, workspace, _runtime) =
        run_service_fixture_with_llm_and_shells(config.clone(), llm.clone(), shells.clone()).await;
    let outcome = service
        .execute_shared(
            request(config, &workspace),
            context(),
            &mut crate::cli::HumanRenderer::new(),
            &mut NoPrompt,
        )
        .await;
    // Confirm real process cleanup even when an assertion about the agent outcome fails below.
    tokio::time::timeout(std::time::Duration::from_secs(10), shells.shutdown())
        .await
        .unwrap();
    let SharedRunOutcome::Completed(summary) = outcome.unwrap() else {
        panic!("a process still holding the resource must prevent a child checkpoint")
    };
    assert_eq!(summary.tool_call_count(), 2);
    assert_eq!(summary.failed_tool_count(), 1);
    assert_eq!(llm.requests.lock().unwrap().len(), 3);
    let connection = rusqlite::Connection::open(&store.paths().database_path).unwrap();
    let paused: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM shared_run_checkpoints WHERE state = 'paused')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!paused);
}

#[tokio::test]
async fn shared_checkpoint_forward_migration_preserves_preexisting_local_sessions() {
    let config = ResolvedConfig::default();
    let (_service, store, workspace, _runtime) = run_service_fixture(config.clone()).await;
    let local = store
        .session_repo()
        .create_session(crate::session::NewSession {
            project_id: workspace.project_id,
            title: "Existing private work".into(),
            cwd: workspace.cwd,
            model: config.model.model,
            base_url: config.model.base_url,
            access_mode: crate::config::AccessMode::Default,
            provider_connection: None,
        })
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(&store.paths().database_path).unwrap();
    // Reconstruct the released V66 endpoint: this forward-only extension owns no old data.
    connection.execute_batch("DROP TRIGGER shared_run_fixed_access_mode; DROP TABLE shared_run_checkpoints; DELETE FROM moyai_schema_migrations WHERE version = 67;").unwrap();
    drop(connection);
    let reopened = SqliteStore::open(store.paths()).unwrap();
    reopened.migrate().unwrap();
    reopened.migrate().unwrap();
    let record = reopened.session_repo().get_session(local.id).await.unwrap();
    assert_eq!(record.title, "Existing private work");
    assert_eq!(record.status, SessionStatus::Idle);
    assert!(
        reopened
            .session_repo()
            .compare_and_set_root_session_access_mode(
                local.id,
                crate::config::AccessMode::Default,
                crate::config::AccessMode::FullAccess,
            )
            .await
            .unwrap()
            .is_some(),
        "ordinary local permission settings remain editable"
    );
    let connection = rusqlite::Connection::open(&store.paths().database_path).unwrap();
    let shared_rows: u64 = connection
        .query_row("SELECT COUNT(*) FROM shared_run_checkpoints", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        shared_rows, 0,
        "migration must not publish or bind old local conversations"
    );
}

#[tokio::test]
async fn shared_execution_rejects_layered_configuration_before_creating_a_session() {
    let config = ResolvedConfig::default();
    let (service, store, workspace, _runtime) = run_service_fixture(config.clone()).await;
    let mut request = request(config, &workspace);
    request.config = crate::app::RunConfigInput::Layered {
        model: String::new(),
        base_url: String::new(),
        config_override: None,
    };
    assert!(
        service
            .execute_shared(
                request,
                context(),
                &mut crate::cli::HumanRenderer::new(),
                &mut NoPrompt
            )
            .await
            .is_err()
    );
    assert!(
        store
            .session_repo()
            .latest_session(workspace.project_id)
            .await
            .unwrap()
            .is_none()
    );
}
