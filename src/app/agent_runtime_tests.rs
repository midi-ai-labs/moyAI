use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::{
    Json, Router,
    routing::{get, post},
};
use camino::Utf8PathBuf;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::agent::{AgentLoop, PromptBuilder};
use crate::cli::{ConfirmationPrompt, OutputMode, ReviewDecision};
use crate::config::{
    AccessMode, MultiAgentMode, ProviderApiMode, ProviderMetadataMode, ResolvedConfig,
};
use crate::error::{CliPromptError, LlmError};
use crate::llm::{
    ChatRequest, LlmClient, LlmEvent, LlmEventSink, LlmResponseSummary, ModelMessage,
};
use crate::protocol::{
    ContentPart, HistoryItem, HistoryItemId, HistoryItemPayload, HistoryScope, ModelResponseId,
    ProtocolEventStore, SubAgentActivityKind, TurnId, TurnTerminalOutcome,
    project_sub_agent_activity,
};
use crate::runtime::{
    AgentStatus, InactiveAgentStatus, RunCancelOutcome, RunCancellationCause, RunControl,
    SessionRuntimeEventHub, SystemClock,
};
use crate::session::{
    AdmissionId, DurableTurnTerminal, FinishReason, ProjectRepository, RunEvent, SessionSelector,
    SessionStartRequest, SessionStatus, ThreadGoalStatus, TokenUsage, ToolCallId,
};
use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
use crate::tool::context::{RunMutationFence, ToolContext, ToolServices};
use crate::tool::multi_agent::{
    WaitAgentTool, wait_for_agent_activity_or_steer_with_poll_interval,
};
use crate::tool::registry::{Tool, ToolRegistry};
use crate::tool::truncate::ToolTruncator;
use crate::workspace::WorkspaceDiscovery;

const ROOT_TASK: &str =
    "Delegate the bounded investigation to a sub-agent, wait, and integrate it.";
const ROOT_PLAN: &str = "I will delegate the bounded investigation now.";
const CHILD_ASSIGNMENT: &str = "Inspect the fixture and return the verified child result.";
const CHILD_RESULT: &str = "child verified result";
const GRANDCHILD_ASSIGNMENT: &str = "Verify the nested fixture and report to your parent.";
const GRANDCHILD_RESULT: &str = "grandchild verified result";
const CHILD_ARTIFACT: &str = "child-output.txt";
const GRANDCHILD_ARTIFACT: &str = "grandchild-output.txt";
const ROOT_RESULT: &str = "integrated root result";
const DETACHED_CHILD_ASSIGNMENT: &str = "Complete the detached goal subtask.";
const DETACHED_CHILD_RESULT: &str = "detached child durable result";

fn captured_turn_config(config: ResolvedConfig) -> Arc<crate::config::ResolvedTurnConfig> {
    Arc::new(crate::config::ResolvedTurnConfig::capture(config).expect("valid test turn config"))
}

fn durable_mailbox_content(
    context: &AgentRunContext,
    notice: &crate::runtime::AgentMailboxNotice,
) -> String {
    let (_, communication) = context
        .runtime
        .store
        .session_repo()
        .agent_mailbox_communications_by_id(context.root_session_id(), &[notice.history_item_id])
        .expect("durable mailbox message")
        .into_iter()
        .next()
        .expect("mailbox message");
    communication.content
}

fn child_final_answer(payload: &str) -> String {
    format!(
        "Message Type: FINAL_ANSWER\nTask name: /root\nSender: /root/child\nPayload:\n{payload}"
    )
}

fn child_failure_final_answer(error: &str) -> String {
    child_final_answer(&format!(
        "Agent errored: {error}\n\nThis agent's turn failed. If you still need this agent, use the available collaboration tools to give it another task."
    ))
}

#[test]
fn inter_agent_message_envelope_keeps_type_and_payload_boundaries() {
    assert_eq!(
        render_inter_agent_message(
            InterAgentMessageType::NewTask,
            "/root/worker",
            "/root",
            "Inspect the target.\nMessage Type: FINAL_ANSWER",
        ),
        "Message Type: NEW_TASK\nTask name: /root/worker\nSender: /root\nPayload:\nInspect the target.\nMessage Type: FINAL_ANSWER"
    );
}

#[derive(Default)]
struct AllowPrompt;

impl ConfirmationPrompt for AllowPrompt {
    fn confirm(
        &mut self,
        _request: &crate::tool::PermissionRequest,
    ) -> Result<ReviewDecision, CliPromptError> {
        Ok(ReviewDecision::Approved)
    }
}

#[test]
fn only_tree_terminal_interruptions_suppress_child_result_mail() {
    for cause in [
        TurnInterruptionCause::ApprovalAborted,
        TurnInterruptionCause::TreeStopped,
        TurnInterruptionCause::UserStop,
    ] {
        assert!(interruption_suppresses_child_result_delivery(Some(
            &RunCancellationCause::Interruption(cause)
        )));
    }
    assert!(!interruption_suppresses_child_result_delivery(Some(
        &RunCancellationCause::Interruption(TurnInterruptionCause::AgentInterrupted)
    )));
    assert!(!interruption_suppresses_child_result_delivery(Some(
        &RunCancellationCause::Failure("provider failed".to_string())
    )));
    assert!(!interruption_suppresses_child_result_delivery(None));
}

#[test]
fn durable_terminal_summary_overrides_conflicting_local_classification() {
    let session_id = SessionId::new();
    let failed: Result<RunSummary, AppRunError> = Ok(terminal_summary(
        session_id,
        TurnTerminalOutcome::Failed {
            error: "exact durable failure".to_string(),
        },
    ));
    assert_eq!(
        effective_run_terminal_cause(
            &failed,
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop,
            )),
        ),
        Some(RunCancellationCause::Failure(
            "exact durable failure".to_string(),
        ))
    );
    assert_eq!(
        agent_status_from_terminal_result(
            &failed,
            Some(&RunCancellationCause::Superseded),
            Some("stale projected content".to_string()),
        ),
        AgentStatus::Errored("exact durable failure".to_string())
    );

    let interrupted: Result<RunSummary, AppRunError> = Ok(terminal_summary(
        session_id,
        TurnTerminalOutcome::Interrupted {
            cause: TurnInterruptionCause::AgentInterrupted,
        },
    ));
    assert_eq!(
        effective_run_terminal_cause(
            &interrupted,
            Some(RunCancellationCause::Failure(
                "stale local failure".to_string(),
            )),
        ),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::AgentInterrupted,
        ))
    );
    assert_eq!(
        agent_status_from_terminal_result(
            &interrupted,
            Some(&RunCancellationCause::Failure(
                "stale local failure".to_string(),
            )),
            Some("stale projected content".to_string()),
        ),
        AgentStatus::Interrupted
    );

    let completed: Result<RunSummary, AppRunError> =
        Ok(terminal_summary(session_id, TurnTerminalOutcome::Completed));
    assert_eq!(
        effective_run_terminal_cause(
            &completed,
            Some(RunCancellationCause::Failure(
                "stale local failure".to_string(),
            )),
        ),
        None
    );
    assert_eq!(
        agent_status_from_terminal_result(
            &completed,
            Some(&RunCancellationCause::Failure(
                "stale local failure".to_string(),
            )),
            Some("exact completed content".to_string()),
        ),
        AgentStatus::Completed(Some("exact completed content".to_string()))
    );
}

#[derive(Default)]
struct AbortPrompt;

impl ConfirmationPrompt for AbortPrompt {
    fn confirm(
        &mut self,
        _request: &crate::tool::PermissionRequest,
    ) -> Result<ReviewDecision, CliPromptError> {
        Ok(ReviewDecision::Abort)
    }
}

#[test]
fn child_approval_abort_interrupts_only_requesting_child_before_prompt_returns() {
    let root_session_id = SessionId::new();
    let root_control = RunControl::new();
    let (control, _root_lease) =
        AgentControl::with_root_control(root_session_id, 3, root_control.clone())
            .expect("root control");
    let (_, requesting_child) = control
        .register_child(
            &AgentPath::root(),
            "requester",
            SessionId::new(),
            Some("waiting for approval".to_string()),
        )
        .expect("requesting child");
    let (_, sibling) = control
        .register_child(
            &AgentPath::root(),
            "sibling",
            SessionId::new(),
            Some("ready for another provider request".to_string()),
        )
        .expect("sibling child");
    let confirmation =
        SharedConfirmationPrompt::new_with_root_control(AbortPrompt, root_control.clone());
    bind_execution_confirmation(&confirmation);
    let request = crate::tool::PermissionRequest {
        access: crate::workspace::AccessKind::Edit,
        summary: "write protected file".to_string(),
        details: Vec::new(),
        targets: Vec::new(),
        outside_workspace: false,
        risks: Vec::new(),
        agent_path: Some("/root/requester".to_string()),
        agent_task_name: Some("requester".to_string()),
    };
    let mut child_prompt = confirmation;

    let outcome = child_prompt
        .confirm_with_control(&request, &requesting_child.run_control())
        .expect("approval abort outcome");

    assert_eq!(outcome, crate::cli::ConfirmationOutcome::Aborted);
    assert_eq!(root_control.cause(), None);
    assert!(matches!(
        requesting_child.run_control().cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted
        ))
    ));
    assert_eq!(sibling.run_control().cause(), None);
    assert!(!control.tree_is_cancelled());
    let sibling_provider_starts = AtomicUsize::new(0);
    if !sibling.run_control().is_cancelled() {
        sibling_provider_starts.fetch_add(1, Ordering::SeqCst);
    }
    assert_eq!(sibling_provider_starts.load(Ordering::SeqCst), 1);
}

#[test]
fn child_approval_abort_preserves_root_success_commit_and_sibling() {
    let root_session_id = SessionId::new();
    let root_control = RunControl::new();
    let (control, root_lease) =
        AgentControl::with_root_control(root_session_id, 3, root_control.clone())
            .expect("root control");
    let root_turn_control = root_lease.run_control();
    let (_, requesting_child) = control
        .register_child(
            &AgentPath::root(),
            "requester",
            SessionId::new(),
            Some("waiting for approval".to_string()),
        )
        .expect("requesting child");
    let (_, sibling) = control
        .register_child(
            &AgentPath::root(),
            "sibling",
            SessionId::new(),
            Some("unrelated work".to_string()),
        )
        .expect("sibling child");
    let tree = AgentTreeRuntime {
        root_session_id,
        control,
        limits: AgentTreeLimits {
            max_concurrent_agents: 3,
            max_concurrent_model_requests: 2,
        },
        model_request_gate: Arc::new(tokio::sync::Semaphore::new(2)),
        active_root_turn_owner: Mutex::new(None),
        metadata: Mutex::new(HashMap::new()),
    };
    let confirmation =
        SharedConfirmationPrompt::new_with_root_control(AbortPrompt, root_control.clone());
    bind_execution_confirmation(&confirmation);
    let success_commit = root_turn_control
        .begin_success_commit()
        .expect("reserve root success");
    let request = crate::tool::PermissionRequest {
        access: crate::workspace::AccessKind::Edit,
        summary: "write protected file".to_string(),
        details: Vec::new(),
        targets: Vec::new(),
        outside_workspace: false,
        risks: Vec::new(),
        agent_path: Some("/root/requester".to_string()),
        agent_task_name: Some("requester".to_string()),
    };

    let outcome = confirmation
        .clone()
        .confirm_with_control(&request, &requesting_child.run_control())
        .expect("requesting child receives its abort");

    assert_eq!(outcome, crate::cli::ConfirmationOutcome::Aborted);
    assert_eq!(root_control.cause(), None);
    assert_eq!(
        requesting_child.run_control().cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted
        ))
    );
    assert_eq!(sibling.run_control().cause(), None);
    assert!(!tree.control.tree_is_cancelled());
    assert!(success_commit.seal());
    assert!(root_turn_control.success_is_sealed());
}

#[test]
fn detached_child_approval_abort_preserves_sealed_root_success_and_sibling() {
    let root_session_id = SessionId::new();
    let root_control = RunControl::new();
    let (control, root_lease) =
        AgentControl::with_root_control(root_session_id, 3, root_control.clone())
            .expect("root control");
    let root_turn_control = root_lease.run_control();
    let (_, requesting_child) = control
        .register_child(
            &AgentPath::root(),
            "requester",
            SessionId::new(),
            Some("waiting for approval".to_string()),
        )
        .expect("requesting child");
    let (_, sibling) = control
        .register_child(
            &AgentPath::root(),
            "sibling",
            SessionId::new(),
            Some("unrelated work".to_string()),
        )
        .expect("sibling child");
    let tree = AgentTreeRuntime {
        root_session_id,
        control,
        limits: AgentTreeLimits {
            max_concurrent_agents: 3,
            max_concurrent_model_requests: 2,
        },
        model_request_gate: Arc::new(tokio::sync::Semaphore::new(2)),
        active_root_turn_owner: Mutex::new(None),
        metadata: Mutex::new(HashMap::new()),
    };
    assert!(root_turn_control.seal_success());
    let confirmation =
        SharedConfirmationPrompt::new_with_root_control(AbortPrompt, root_control.clone());
    bind_execution_confirmation(&confirmation);
    let request = crate::tool::PermissionRequest {
        access: crate::workspace::AccessKind::Edit,
        summary: "abort detached child".to_string(),
        details: Vec::new(),
        targets: Vec::new(),
        outside_workspace: false,
        risks: Vec::new(),
        agent_path: Some("/root/requester".to_string()),
        agent_task_name: Some("requester".to_string()),
    };

    let outcome = confirmation
        .clone()
        .confirm_with_control(&request, &requesting_child.run_control())
        .expect("detached child Abort");

    assert_eq!(outcome, crate::cli::ConfirmationOutcome::Aborted);
    assert_eq!(root_control.cause(), None);
    assert!(root_turn_control.success_is_sealed());
    assert_eq!(
        requesting_child.run_control().cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted
        ))
    );
    assert_eq!(sibling.run_control().cause(), None);
    assert!(!tree.control.tree_is_cancelled());
}

#[test]
fn child_approval_abort_remains_exact_despite_a_competing_root_terminal_cause() {
    for existing_cause in [
        RunCancellationCause::Failure("provider transport failed".to_string()),
        RunCancellationCause::Interruption(TurnInterruptionCause::UserStop),
        RunCancellationCause::Interruption(TurnInterruptionCause::ApprovalAborted),
        RunCancellationCause::Superseded,
    ] {
        let root_session_id = SessionId::new();
        let root_control = RunControl::new();
        let (control, root_lease) =
            AgentControl::with_root_control(root_session_id, 3, root_control.clone())
                .expect("root control");
        let root_turn_control = root_lease.run_control();
        let (_, requesting_child) = control
            .register_child(
                &AgentPath::root(),
                "requester",
                SessionId::new(),
                Some("waiting for approval".to_string()),
            )
            .expect("requesting child");
        let (_, sibling) = control
            .register_child(
                &AgentPath::root(),
                "sibling",
                SessionId::new(),
                Some("unrelated work".to_string()),
            )
            .expect("sibling child");
        let tree = AgentTreeRuntime {
            root_session_id,
            control,
            limits: AgentTreeLimits {
                max_concurrent_agents: 3,
                max_concurrent_model_requests: 2,
            },
            model_request_gate: Arc::new(tokio::sync::Semaphore::new(2)),
            active_root_turn_owner: Mutex::new(None),
            metadata: Mutex::new(HashMap::new()),
        };
        assert!(root_turn_control.cancel(existing_cause.clone()));
        let confirmation =
            SharedConfirmationPrompt::new_with_root_control(AbortPrompt, root_control.clone());
        bind_execution_confirmation(&confirmation);
        let request = crate::tool::PermissionRequest {
            access: crate::workspace::AccessKind::Edit,
            summary: "write protected file".to_string(),
            details: Vec::new(),
            targets: Vec::new(),
            outside_workspace: false,
            risks: Vec::new(),
            agent_path: Some("/root/requester".to_string()),
            agent_task_name: Some("requester".to_string()),
        };

        let outcome = confirmation
            .clone()
            .confirm_with_control(&request, &requesting_child.run_control())
            .expect("requesting child receives its abort");

        assert_eq!(outcome, crate::cli::ConfirmationOutcome::Aborted);
        assert_eq!(root_control.cause(), None);
        assert_eq!(root_turn_control.cause(), Some(existing_cause));
        assert_eq!(
            requesting_child.run_control().cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::ApprovalAborted
            ))
        );
        assert_eq!(sibling.run_control().cause(), None);
        assert!(!tree.control.tree_is_cancelled());
    }
}

async fn direct_runtime_fixture(
    test_name: &str,
    max_concurrent_agents: usize,
) -> (Arc<AgentRuntime>, SessionContext, ResolvedConfig) {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.keep()).expect("utf8 tempdir");
    let storage_paths = StoragePaths {
        data_dir: root.join(".moyai-data"),
        database_path: root.join(".moyai-data/moyai.sqlite3"),
        truncation_dir: root.join(".moyai-data/truncation"),
    };
    let sqlite = SqliteStore::open(&storage_paths).expect("store");
    sqlite.migrate().expect("migrate");
    let store = StoreBundle::new(sqlite);
    let mut config = ResolvedConfig::default();
    config.multi_agent.enabled = true;
    config.multi_agent.max_concurrent_agents = max_concurrent_agents;
    let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
    store
        .project_repo()
        .upsert_project(workspace.project_id, &workspace.root, test_name, "none")
        .await
        .expect("project");
    let session_service = crate::session::SessionService::new(store.clone());
    let session = session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some(test_name.to_string()),
                cwd: root,
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            workspace,
        )
        .await
        .expect("session");
    (
        Arc::new(AgentRuntime::new(store, session_service)),
        session,
        config,
    )
}

#[tokio::test]
async fn wait_agent_returns_immediately_for_steer_queued_before_wait_starts() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("wait-agent-prequeued-steer", 2).await;
    let run_control = RunControl::new();
    let root_execution = runtime
        .begin_root(
            &root_session,
            captured_turn_config(config.clone()),
            SharedConfirmationPrompt::new(AllowPrompt),
            run_control.clone(),
        )
        .await
        .expect("root execution");
    let owner = bind_test_root_turn(&runtime, &root_execution).await;
    let active_run = runtime
        .store
        .active_runs()
        .try_start(root_session.session.id, run_control.clone())
        .expect("register active run");
    active_run
        .set_turn_id(owner.turn_id)
        .expect("bind active turn");
    runtime
        .store
        .session_repo()
        .accept_active_turn_steer(
            root_session.session.id,
            &crate::protocol::SteerTurn {
                expected_turn_id: owner.turn_id,
                items: vec![crate::protocol::UserInputItem::Text {
                    text: "already queued".to_string(),
                }],
                additional_context: Default::default(),
                client_user_message_id: Some("wait-agent-prequeued".to_string()),
            },
        )
        .await
        .expect("queue steer before wait");

    let services = ToolServices {
        edit_safety: crate::edit::EditSafety::default(),
        formatter: crate::edit::Formatter::new(config.format.clone()),
        change_tracker: crate::edit::ChangeTracker::default(),
        store: runtime.store.clone(),
        storage_paths: runtime.store.paths().clone(),
        truncator: ToolTruncator,
        mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
        skills: crate::skill::SkillsService::new(),
    };
    let mut prompt = AllowPrompt;
    let context = ToolContext {
        session: &root_session,
        workspace: &root_session.workspace,
        config: &config,
        tool_call_id: ToolCallId::new(),
        cancel: run_control.token(),
        run_control: run_control.clone(),
        run_mutation_fence: RunMutationFence::new(
            runtime.store.session_repo(),
            owner.session_id,
            owner.admission_id,
            owner.turn_id,
            run_control,
        ),
        prompt: &mut prompt,
        services: &services,
        agent: Some(&root_execution.context),
        permission_guardian: None,
    };

    let result = tokio::time::timeout(
        Duration::from_millis(250),
        WaitAgentTool.execute(json!({"timeout_ms": 10_000}), context),
    )
    .await
    .expect("prequeued durable steer must not enter the 10 second wait")
    .expect("wait_agent result");
    let output =
        serde_json::from_str::<serde_json::Value>(&result.output_text).expect("typed wait output");
    assert_eq!(
        output["message"],
        json!("Wait interrupted by new user input.")
    );
    assert_eq!(output["timed_out"], json!(false));
}

#[tokio::test]
async fn wait_agent_polls_cross_store_steer_and_delivers_it_to_the_prompt_once() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("wait-agent-cross-store-steer", 2).await;
    let run_control = RunControl::new();
    let root_execution = runtime
        .begin_root(
            &root_session,
            captured_turn_config(config.clone()),
            SharedConfirmationPrompt::new(AllowPrompt),
            run_control.clone(),
        )
        .await
        .expect("root execution");
    let owner = bind_test_root_turn(&runtime, &root_execution).await;
    let active_run = runtime
        .store
        .active_runs()
        .try_start(root_session.session.id, run_control.clone())
        .expect("register active run");
    active_run
        .set_turn_id(owner.turn_id)
        .expect("bind active turn");

    let second_sqlite =
        SqliteStore::open(runtime.store.paths()).expect("second process-like sqlite connection");
    second_sqlite.migrate().expect("second store migration");
    let second_store = StoreBundle::new(second_sqlite);
    let services = ToolServices {
        edit_safety: crate::edit::EditSafety::default(),
        formatter: crate::edit::Formatter::new(config.format.clone()),
        change_tracker: crate::edit::ChangeTracker::default(),
        store: runtime.store.clone(),
        storage_paths: runtime.store.paths().clone(),
        truncator: ToolTruncator,
        mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
        skills: crate::skill::SkillsService::new(),
    };
    let mut prompt = AllowPrompt;
    let context = ToolContext {
        session: &root_session,
        workspace: &root_session.workspace,
        config: &config,
        tool_call_id: ToolCallId::new(),
        cancel: run_control.token(),
        run_control: run_control.clone(),
        run_mutation_fence: RunMutationFence::new(
            runtime.store.session_repo(),
            owner.session_id,
            owner.admission_id,
            owner.turn_id,
            run_control,
        ),
        prompt: &mut prompt,
        services: &services,
        agent: Some(&root_execution.context),
        permission_guardian: None,
    };
    let wait = async {
        tokio::time::timeout(
            Duration::from_secs(1),
            WaitAgentTool.execute(json!({"timeout_ms": 10_000}), context),
        )
        .await
        .expect("cross-store durable poll must beat the tool timeout")
        .expect("wait_agent result")
    };
    let enqueue = async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        second_store
            .session_repo()
            .accept_active_turn_steer(
                root_session.session.id,
                &crate::protocol::SteerTurn {
                    expected_turn_id: owner.turn_id,
                    items: vec![crate::protocol::UserInputItem::Text {
                        text: "cross-store steer".to_string(),
                    }],
                    additional_context: Default::default(),
                    client_user_message_id: Some("cross-store-client".to_string()),
                },
            )
            .await
            .expect("queue steer through the second store")
    };
    let (result, input_id) = tokio::join!(wait, enqueue);
    let output =
        serde_json::from_str::<serde_json::Value>(&result.output_text).expect("typed wait output");
    assert_eq!(
        output["message"],
        json!("Wait interrupted by new user input.")
    );
    assert_eq!(output["timed_out"], json!(false));
    assert_eq!(
        runtime
            .store
            .session_repo()
            .deliver_all_pending_turn_steers_for_admitted_turn(
                owner.session_id,
                owner.admission_id,
                owner.turn_id,
            )
            .expect("deliver cross-store steer"),
        vec![input_id]
    );
    assert!(
        runtime
            .store
            .session_repo()
            .deliver_all_pending_turn_steers_for_admitted_turn(
                owner.session_id,
                owner.admission_id,
                owner.turn_id,
            )
            .expect("repeat delivery is empty")
            .is_empty()
    );

    let mut context_builder =
        crate::agent::context_manager::ContextManager::active_history_builder();
    let snapshot = runtime
        .store
        .protocol_event_store()
        .visit_active_history_pages_for_session(
            owner.session_id,
            crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
            &mut |page| {
                context_builder.ingest_page(page.items);
                Ok(())
            },
        )
        .expect("active history after cross-store delivery");
    let prompt_context = context_builder.finish(snapshot.append_fence, snapshot.canonical_count);
    assert_eq!(
        prompt_context
            .model_messages(true)
            .iter()
            .filter(|message| matches!(
                message,
                ModelMessage::User { content } if content == "cross-store steer"
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn wait_agent_final_recheck_observes_cross_store_steer_near_timeout() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("wait-agent-cross-store-timeout-edge", 2).await;
    let run_control = RunControl::new();
    let root_execution = runtime
        .begin_root(
            &root_session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            run_control.clone(),
        )
        .await
        .expect("root execution");
    let owner = bind_test_root_turn(&runtime, &root_execution).await;
    let active_run = runtime
        .store
        .active_runs()
        .try_start(root_session.session.id, run_control)
        .expect("register active run");
    active_run
        .set_turn_id(owner.turn_id)
        .expect("bind active turn");
    let second_sqlite =
        SqliteStore::open(runtime.store.paths()).expect("second process-like sqlite connection");
    second_sqlite.migrate().expect("second store migration");
    let second_store = StoreBundle::new(second_sqlite);

    let wait = wait_for_agent_activity_or_steer_with_poll_interval(
        &root_execution.context,
        runtime.store.active_runs(),
        owner.session_id,
        200,
        Duration::from_secs(1),
    );
    let enqueue = async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        second_store
            .session_repo()
            .accept_active_turn_steer(
                owner.session_id,
                &crate::protocol::SteerTurn {
                    expected_turn_id: owner.turn_id,
                    items: vec![crate::protocol::UserInputItem::Text {
                        text: "near-timeout steer".to_string(),
                    }],
                    additional_context: Default::default(),
                    client_user_message_id: Some("near-timeout-client".to_string()),
                },
            )
            .await
            .expect("queue near-timeout steer")
    };
    let (result, input_id) = tokio::join!(wait, enqueue);
    let result = result.expect("near-timeout wait result");
    assert_eq!(result.message, "Wait interrupted by new user input.");
    assert!(!result.timed_out);
    assert!(result.updated_agents.is_empty());
    assert_eq!(
        runtime
            .store
            .session_repo()
            .deliver_all_pending_turn_steers_for_admitted_turn(
                owner.session_id,
                owner.admission_id,
                owner.turn_id,
            )
            .expect("deliver near-timeout steer"),
        vec![input_id]
    );
}

async fn retained_agent_session(
    runtime: &AgentRuntime,
    root_session: &SessionContext,
    config: &ResolvedConfig,
    parent_session_id: SessionId,
    agent_path: &str,
    task_name: &str,
) -> SessionContext {
    let session = runtime
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some(task_name.to_string()),
                cwd: root_session.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            root_session.workspace.clone(),
        )
        .await
        .expect("retained agent session");
    runtime
        .store
        .session_repo()
        .insert_session_spawn_edge(
            root_session.session.id,
            parent_session_id,
            session.session.id,
            agent_path,
            task_name,
        )
        .await
        .expect("retained agent edge");
    session
}

#[tokio::test]
async fn ready_followup_at_capacity_surfaces_limit_without_history_append() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("ready-followup-capacity", 2).await;
    let root_execution = runtime
        .begin_root(
            &root_session,
            captured_turn_config(config.clone()),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    bind_test_root_turn(&runtime, &root_execution).await;
    let target_session = retained_agent_session(
        &runtime,
        &root_session,
        &config,
        root_session.session.id,
        "/root/target",
        "target",
    )
    .await;
    let sibling_session = retained_agent_session(
        &runtime,
        &root_session,
        &config,
        root_session.session.id,
        "/root/sibling",
        "sibling",
    )
    .await;
    let tree = root_execution.context.tree.clone();
    let target = tree
        .control
        .restore_inactive_child(
            &AgentPath::root(),
            "target",
            target_session.session.id,
            InactiveAgentStatus::PendingInit,
            None,
        )
        .expect("retained target");
    let (_sibling, _sibling_execution) = tree
        .control
        .register_child(
            &AgentPath::root(),
            "sibling",
            sibling_session.session.id,
            None,
        )
        .expect("capacity-filling sibling");
    let history_before = runtime
        .store
        .protocol_event_store()
        .list_history_items_for_session(target_session.session.id)
        .expect("target history before")
        .len();

    let error = root_execution
        .context
        .send_message(
            target.path.as_str(),
            "ready work must wait for capacity".to_string(),
            true,
            "capacity-reject".to_string(),
        )
        .await
        .expect_err("ready follow-up must reject at capacity");
    assert!(
        error.contains("agent limit reached (root included; max 2)"),
        "typed capacity denial must remain clear to the collaboration tool: {error}"
    );
    assert_eq!(
        runtime
            .store
            .protocol_event_store()
            .list_history_items_for_session(target_session.session.id)
            .expect("target history after")
            .len(),
        history_before,
        "capacity denial must roll back before canonical history append"
    );
    let target_after = tree
        .control
        .list_agents(Some(&target.path))
        .expect("target after capacity rejection")
        .into_iter()
        .next()
        .expect("target");
    assert_eq!(target_after.pending_mail_count, 0);
    assert!(!target_after.is_active);
}

#[tokio::test]
async fn trigger_followup_uses_durable_terminal_state_when_live_projection_still_runs() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("trigger-terminal-live-race", 2).await;
    let root_execution = runtime
        .begin_root(
            &root_session,
            captured_turn_config(config.clone()),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    bind_test_root_turn(&runtime, &root_execution).await;
    let child = retained_agent_session(
        &runtime,
        &root_session,
        &config,
        root_session.session.id,
        "/root/child",
        "child",
    )
    .await;
    let tree = root_execution.context.tree.clone();
    let (child_snapshot, child_lease) = tree
        .control
        .register_child(
            &AgentPath::root(),
            "child",
            child.session.id,
            Some("terminal race fixture".to_string()),
        )
        .expect("live child");
    child_lease
        .set_status(ActiveAgentStatus::Running)
        .expect("running child projection");
    let child_turn_id = TurnId::new();
    let child_admission = runtime
        .store
        .session_repo()
        .admit_session_turn(child.session.id, child_turn_id)
        .await
        .expect("child admission")
        .expect("admitted child");
    assert!(
        runtime
            .store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.session.id,
                child_admission.admission_id,
                &terminal_event(child.session.id, TurnTerminalOutcome::Completed, None,),
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("durable child terminal")
            .was_applied()
    );
    let stale_live = tree
        .control
        .list_agents(Some(&child_snapshot.path))
        .expect("stale child projection")
        .into_iter()
        .next()
        .expect("child projection");
    assert!(stale_live.is_active);
    assert_eq!(stale_live.status, AgentStatus::Running);

    assert_eq!(
        root_execution
            .context
            .send_message(
                child_snapshot.path.as_str(),
                "follow-up committed after the durable terminal".to_string(),
                true,
                "terminal-race-followup".to_string(),
            )
            .await
            .expect("terminal-race follow-up"),
        child_snapshot.path
    );
    assert!(
        runtime
            .store
            .session_repo()
            .has_pending_agent_mailbox_messages(child.session.id)
            .expect("durable child follow-up")
    );
    assert!(
        runtime
            .store
            .protocol_event_store()
            .list_history_items_for_session(child.session.id)
            .expect("child history")
            .iter()
            .all(|item| !matches!(
                item.payload,
                HistoryItemPayload::InterAgentCommunication { .. }
            )),
        "terminal-race follow-up must remain queued for the next admitted turn"
    );

    tree.control
        .complete_execution(
            child_lease,
            InactiveAgentStatus::Completed(None),
            Some("completed before follow-up".to_string()),
        )
        .expect("settle stale child projection");
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root");
}

#[tokio::test]
async fn rehydrated_crash_owner_accepts_explicit_recovery_with_immediate_precedence() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("rehydrated-crash-explicit-recovery", 2).await;
    let owner = retained_agent_session(
        &runtime,
        &root_session,
        &config,
        root_session.session.id,
        "/root/owner",
        "owner",
    )
    .await;
    let leaf = retained_agent_session(
        &runtime,
        &root_session,
        &config,
        owner.session.id,
        "/root/owner/leaf",
        "leaf",
    )
    .await;
    let leaf_turn = TurnId::new();
    terminalize_test_session(
        &runtime,
        leaf.session.id,
        leaf_turn,
        &terminal_event(
            leaf.session.id,
            TurnTerminalOutcome::Failed {
                error: "leaf failed".to_string(),
            },
            None,
        ),
    )
    .await;
    let leaf_handoff = runtime
        .store
        .session_repo()
        .agent_completion_handoff(leaf.session.id, leaf_turn)
        .expect("leaf completion handoff")
        .expect("leaf FINAL receipt");
    assert_eq!(
        runtime
            .store
            .session_repo()
            .schedulable_owner_resume_request_id(owner.session.id)
            .expect("normal completion owner resume"),
        None
    );
    let claimed_trigger = runtime
        .store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            owner.session.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner",
                    "/root",
                    "run the turn that will crash",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("first explicit crash trigger");
    assert!(claimed_trigger.schedule_turn);
    let crashed_turn = TurnId::new();
    let crashed_admission = runtime
        .store
        .session_repo()
        .admit_agent_triggered_turn(
            owner.session.id,
            crashed_turn,
            claimed_trigger.history_item_id,
        )
        .await
        .expect("explicit crash admission")
        .expect("explicit crash turn admitted");
    let delivered = runtime
        .store
        .session_repo()
        .deliver_pending_agent_mail_for_admitted_turn(
            owner.session.id,
            crashed_admission.admission_id,
            crashed_turn,
            128,
        )
        .expect("safe claimed-turn delivery")
        .history_item_ids;
    assert_eq!(delivered.len(), 2);
    assert!(delivered.contains(&leaf_handoff.history_item_id));
    assert!(delivered.contains(&claimed_trigger.history_item_id));
    let target = runtime
        .store
        .session_repo()
        .captured_running_terminal_target(owner.session.id)
        .await
        .expect("capture running owner")
        .expect("running owner target");
    assert!(
        runtime
            .store
            .session_repo()
            .recover_captured_running_session_with_protocol_event(
                owner.session.id,
                &terminal_event(
                    owner.session.id,
                    TurnTerminalOutcome::Failed {
                        error: "owner worker crashed".to_string(),
                    },
                    None,
                ),
                target,
            )
            .await
            .expect("recover crashed owner")
    );
    let recovery_trigger = runtime
        .store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            owner.session.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner",
                    "/root",
                    "recover with explicit work",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("durable explicit crash recovery");
    assert!(recovery_trigger.schedule_turn);

    let store = runtime.store.clone();
    drop(runtime);
    let resumed_runtime = Arc::new(AgentRuntime::new(
        store.clone(),
        crate::session::SessionService::new(store.clone()),
    ));
    let root_execution = resumed_runtime
        .begin_root(
            &root_session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("rehydrated root");
    let tree = root_execution.context.tree.clone();
    let owner_path = AgentPath::try_from("/root/owner").expect("owner path");
    let restored = tree
        .control
        .list_agents(Some(&owner_path))
        .expect("rehydrated owner")
        .into_iter()
        .next()
        .expect("owner snapshot");
    assert_eq!(
        restored.status,
        AgentStatus::Errored("owner worker crashed".to_string())
    );
    assert!(!restored.is_active);
    assert_eq!(restored.pending_mail_count, 1);
    let mut scheduled = tree
        .control
        .schedule_pending_triggered_executions()
        .expect("schedule rehydrated crash recovery");
    assert_eq!(scheduled.len(), 1);
    let execution = scheduled.pop().expect("immediate crash recovery");
    assert_eq!(execution.path(), &owner_path);
    assert_eq!(
        execution.trigger_history_item_id(),
        Some(recovery_trigger.history_item_id)
    );
    assert_eq!(execution.owner_resume_request_id(), None);
    let recovery_turn_id = TurnId::new();
    let _admission = store
        .session_repo()
        .admit_agent_triggered_turn(
            owner.session.id,
            recovery_turn_id,
            recovery_trigger.history_item_id,
        )
        .await
        .expect("explicit recovery admission")
        .expect("explicit recovery admitted");
    tree.control
        .mark_execution_admitted(
            &execution.scope(),
            AgentExecutionWakeCause::ExplicitTask(recovery_trigger.history_item_id),
            recovery_turn_id,
            Some("explicit crash recovery admitted".to_string()),
            || Ok(None),
        )
        .expect("project explicit admission");
    assert_eq!(
        store
            .session_repo()
            .schedulable_owner_resume_request_id(owner.session.id)
            .expect("coalesced OwnerResume"),
        None
    );
}

#[tokio::test]
async fn rehydrated_orphan_crash_starts_ready_explicit_recovery() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("rehydrated-orphan-crash-recovery", 2).await;
    let owner = retained_agent_session(
        &runtime,
        &root_session,
        &config,
        root_session.session.id,
        "/root/owner",
        "owner",
    )
    .await;
    let leaf = retained_agent_session(
        &runtime,
        &root_session,
        &config,
        owner.session.id,
        "/root/owner/leaf",
        "leaf",
    )
    .await;
    runtime
        .store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            leaf.session.id,
            InterAgentCommunication {
                author: "/root/owner".to_string(),
                recipient: "/root/owner/leaf".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner/leaf",
                    "/root/owner",
                    "keep descendant work pending",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("pending descendant trigger");
    let owner_turn = TurnId::new();
    runtime
        .store
        .session_repo()
        .admit_session_turn(owner.session.id, owner_turn)
        .await
        .expect("owner admission")
        .expect("owner admitted");
    let target = runtime
        .store
        .session_repo()
        .captured_running_terminal_target(owner.session.id)
        .await
        .expect("capture running owner")
        .expect("running owner target");
    assert!(
        runtime
            .store
            .session_repo()
            .recover_captured_running_session_with_protocol_event(
                owner.session.id,
                &terminal_event(
                    owner.session.id,
                    TurnTerminalOutcome::Failed {
                        error: "owner worker crashed without OwnerResume".to_string(),
                    },
                    None,
                ),
                target,
            )
            .await
            .expect("recover orphan crash")
    );
    assert!(
        runtime
            .store
            .session_repo()
            .schedulable_owner_resume_request_id(owner.session.id)
            .expect("orphan crash OwnerResume projection")
            .is_none()
    );
    let explicit = runtime
        .store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            owner.session.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner",
                    "/root",
                    "recover orphan crash explicitly",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("orphan crash explicit trigger");
    assert!(explicit.schedule_turn);

    let store = runtime.store.clone();
    drop(runtime);
    let resumed_runtime = Arc::new(AgentRuntime::new(
        store.clone(),
        crate::session::SessionService::new(store.clone()),
    ));
    let root_execution = resumed_runtime
        .begin_root(
            &root_session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("rehydrated root");
    let owner_path = AgentPath::try_from("/root/owner").expect("owner path");
    let tree = root_execution.context.tree.clone();
    let owner_snapshot = tree
        .control
        .list_agents(Some(&owner_path))
        .expect("rehydrated orphan owner")
        .into_iter()
        .next()
        .expect("orphan owner");
    assert_eq!(owner_snapshot.status, AgentStatus::AwaitingDescendants);
    assert_eq!(owner_snapshot.pending_mail_count, 1);
    assert!(
        tree.control
            .mailbox_has_ready_trigger_turn(&owner_path)
            .expect("rehydrated orphan readiness")
    );

    let mut scheduled = tree
        .control
        .schedule_pending_triggered_executions()
        .expect("schedule orphan explicit recovery");
    assert_eq!(scheduled.len(), 1);
    let execution = scheduled.pop().expect("orphan recovery execution");
    assert_eq!(execution.path(), &owner_path);
    assert_eq!(
        execution.trigger_history_item_id(),
        Some(explicit.history_item_id)
    );
    assert_eq!(execution.owner_resume_request_id(), None);
    store
        .session_repo()
        .admit_agent_triggered_turn(owner.session.id, TurnId::new(), explicit.history_item_id)
        .await
        .expect("orphan explicit admission")
        .expect("orphan explicit admitted");
}

#[tokio::test]
async fn completed_owner_followup_is_ready_before_live_descendant_settles() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("completed-owner-explicit-ready", 3).await;
    let owner = retained_agent_session(
        &runtime,
        &root_session,
        &config,
        root_session.session.id,
        "/root/owner",
        "owner",
    )
    .await;
    let leaf = retained_agent_session(
        &runtime,
        &root_session,
        &config,
        owner.session.id,
        "/root/owner/leaf",
        "leaf",
    )
    .await;
    let _leaf_trigger = runtime
        .store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            leaf.session.id,
            InterAgentCommunication {
                author: "/root/owner".to_string(),
                recipient: "/root/owner/leaf".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner/leaf",
                    "/root/owner",
                    "finish descendant work",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("pending descendant trigger");
    let owner_turn = TurnId::new();
    terminalize_test_session(
        &runtime,
        owner.session.id,
        owner_turn,
        &terminal_event(owner.session.id, TurnTerminalOutcome::Completed, None),
    )
    .await;
    assert!(
        runtime
            .store
            .session_repo()
            .pending_deferred_completion(owner.session.id)
            .expect("completed owner deferred state")
            .is_none(),
        "normal completion is independent of descendant liveness"
    );
    let explicit = runtime
        .store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            owner.session.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/owner".to_string(),
                content: render_inter_agent_message(
                    InterAgentMessageType::NewTask,
                    "/root/owner",
                    "/root",
                    "run without waiting for descendant settlement",
                ),
                trigger_turn: true,
            },
            false,
        )
        .expect("append completed-owner follow-up");
    assert!(
        explicit.schedule_turn,
        "an explicit follow-up to a completed owner is immediately schedulable"
    );
    let explicit_trigger = explicit.history_item_id;

    let store = runtime.store.clone();
    drop(runtime);
    let resumed_runtime = Arc::new(AgentRuntime::new(
        store.clone(),
        crate::session::SessionService::new(store.clone()),
    ));
    let root_execution = resumed_runtime
        .begin_root(
            &root_session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("rehydrated root");
    let tree = root_execution.context.tree.clone();
    let owner_path = AgentPath::try_from("/root/owner").expect("owner path");
    let leaf_path = AgentPath::try_from("/root/owner/leaf").expect("leaf path");
    assert_eq!(
        tree.control
            .status(&owner_path)
            .expect("rehydrated completed owner"),
        AgentStatus::Completed(None)
    );
    let queued_owner = tree
        .control
        .list_agents(Some(&owner_path))
        .expect("queued owner snapshot")
        .into_iter()
        .next()
        .expect("queued owner");
    assert_eq!(queued_owner.status, AgentStatus::Completed(None));
    assert!(!queued_owner.is_active);
    assert_eq!(queued_owner.pending_mail_count, 1);
    assert!(
        tree.control
            .mailbox_has_ready_trigger_turn(&owner_path)
            .expect("ready rehydrated owner trigger")
    );
    let scheduled = tree
        .control
        .schedule_pending_triggered_executions()
        .expect("schedule independent follow-up");
    let execution = scheduled
        .into_iter()
        .find(|execution| execution.path() == &owner_path)
        .expect("completed owner follow-up execution");
    assert_eq!(execution.path(), &owner_path);
    assert_eq!(
        execution.trigger_history_item_id(),
        Some(explicit_trigger),
        "explicit owner work does not wait for the live descendant"
    );
    assert_eq!(execution.owner_resume_request_id(), None);
    assert!(
        tree.control
            .list_agents(Some(&leaf_path))
            .expect("live descendant projection")
            .into_iter()
            .any(|agent| agent.path == leaf_path),
        "the descendant remains independently retained"
    );
}

#[tokio::test]
async fn durable_release_promotes_explicit_when_live_owner_snapshot_still_runs() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("durable-release-live-owner-race", 3).await;
    let root_execution = runtime
        .begin_root(
            &root_session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    let tree = root_execution.context.tree.clone();
    let owner_session_id = SessionId::new();
    let (owner, owner_lease) = tree
        .control
        .register_child(
            &AgentPath::root(),
            "owner",
            owner_session_id,
            Some("finishing completed-early owner".to_string()),
        )
        .expect("live owner");
    let owner_trigger = HistoryItemId::new();
    let owner_turn_id = TurnId::new();
    let owner_lease = owner_lease
        .try_bind_trigger_history_item_id(owner_trigger)
        .map_err(drop)
        .expect("bind active owner trigger");
    tree.control
        .mark_execution_admitted(
            &owner_lease.scope(),
            AgentExecutionWakeCause::ExplicitTask(owner_trigger),
            owner_turn_id,
            Some("stale running owner snapshot".to_string()),
            || Ok(None),
        )
        .expect("bind active owner durable generation");
    let leaf_session_id = SessionId::new();
    let leaf = tree
        .control
        .restore_inactive_child(
            &owner.path,
            "leaf",
            leaf_session_id,
            InactiveAgentStatus::Completed(None),
            None,
        )
        .expect("completed leaf");
    let explicit_id = HistoryItemId::new();
    let AgentMailDeliveryOutcome::Enqueued { scheduled, .. } = tree
        .control
        .commit_and_enqueue_mail(&AgentPath::root(), &owner.path, true, || {
            Ok(AgentMailCommit {
                history_item_id: explicit_id,
                schedule_turn: false,
                owner_resume_request_id: None,
            })
        })
        .expect("dormant explicit task");
    assert!(scheduled.is_empty());
    assert!(
        !tree
            .control
            .mailbox_has_ready_trigger_turn(&owner.path)
            .expect("explicit remains dormant before durable release")
    );

    let handoff = StoredAgentCompletionHandoff {
        child_session_id: leaf_session_id,
        child_turn_id: TurnId::new(),
        parent_session_id: owner_session_id,
        parent_agent_path: owner.path.clone(),
        history_item_id: HistoryItemId::new(),
        released_owner_deferred_turn_id: Some(owner_turn_id),
    };
    assert!(
        runtime
            .project_completion_handoff(&tree, &leaf.path, &handoff)
            .expect("project exact durable release")
            .is_empty(),
        "the still-active owner keeps its current execution until local completion"
    );
    let mut scheduled = tree
        .control
        .complete_execution(
            owner_lease,
            InactiveAgentStatus::AwaitingDescendants(owner_turn_id),
            None,
        )
        .expect("publish eventual completed-early owner state");
    assert_eq!(scheduled.len(), 1);
    let explicit_execution = scheduled.pop().expect("promoted explicit owner task");
    assert_eq!(explicit_execution.path(), &owner.path);
    assert_eq!(
        explicit_execution.trigger_history_item_id(),
        Some(explicit_id)
    );
    assert_eq!(explicit_execution.owner_resume_request_id(), None);
    tree.control
        .drain_mailbox(&owner.path)
        .expect("clear projected owner notices");
    tree.control
        .complete_execution(
            explicit_execution,
            InactiveAgentStatus::Completed(None),
            None,
        )
        .expect("complete explicit owner fixture");
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root");
}

#[tokio::test]
async fn live_session_scoped_child_handoff_queues_parent_final_without_resuming_it() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("live-owner-resume-handoff", 2).await;
    let owner_session = retained_agent_session(
        &runtime,
        &root_session,
        &config,
        root_session.session.id,
        "/root/owner",
        "owner",
    )
    .await;
    let leaf_session = retained_agent_session(
        &runtime,
        &root_session,
        &config,
        owner_session.session.id,
        "/root/owner/leaf",
        "leaf",
    )
    .await;
    terminalize_test_session(
        &runtime,
        owner_session.session.id,
        TurnId::new(),
        &terminal_event(
            owner_session.session.id,
            TurnTerminalOutcome::Completed,
            None,
        ),
    )
    .await;
    let leaf_turn_id = TurnId::new();
    terminalize_test_session(
        &runtime,
        leaf_session.session.id,
        leaf_turn_id,
        &terminal_event(
            leaf_session.session.id,
            TurnTerminalOutcome::Completed,
            None,
        ),
    )
    .await;
    let handoff = runtime
        .store
        .session_repo()
        .agent_completion_handoff(leaf_session.session.id, leaf_turn_id)
        .expect("durable child handoff")
        .expect("child completion receipt");
    assert_eq!(
        runtime
            .store
            .session_repo()
            .schedulable_owner_resume_request_id(owner_session.session.id)
            .expect("normal completion owner resume"),
        None
    );
    let root_execution = runtime
        .begin_root(
            &root_session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    let tree = root_execution.context.tree.clone();
    let owner_path = AgentPath::try_from("/root/owner").expect("owner path");
    let leaf_path = AgentPath::try_from("/root/owner/leaf").expect("leaf path");

    let scheduled = runtime
        .project_completion_handoff(&tree, &leaf_path, &handoff)
        .expect("live handoff projection");
    assert!(
        scheduled.is_empty(),
        "an informational child FINAL must not start a parent turn"
    );
    assert!(
        tree.control
            .schedule_pending_triggered_executions()
            .expect("normal completion scheduler pass")
            .is_empty()
    );
    let owner_notices = tree
        .control
        .drain_mailbox(&owner_path)
        .expect("owner mailbox");
    assert_eq!(owner_notices.len(), 1);
    assert_eq!(owner_notices[0].history_item_id, handoff.history_item_id);
    assert!(!owner_notices[0].trigger_turn);
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root");
}

#[tokio::test]
async fn completion_handoff_survives_live_mailbox_projection_backpressure_without_resume() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("owner-resume-mailbox-backpressure", 2).await;
    let owner_session = retained_agent_session(
        &runtime,
        &root_session,
        &config,
        root_session.session.id,
        "/root/owner",
        "owner",
    )
    .await;
    let leaf_session = retained_agent_session(
        &runtime,
        &root_session,
        &config,
        owner_session.session.id,
        "/root/owner/leaf",
        "leaf",
    )
    .await;
    terminalize_test_session(
        &runtime,
        owner_session.session.id,
        TurnId::new(),
        &terminal_event(
            owner_session.session.id,
            TurnTerminalOutcome::Completed,
            None,
        ),
    )
    .await;
    let leaf_turn_id = TurnId::new();
    terminalize_test_session(
        &runtime,
        leaf_session.session.id,
        leaf_turn_id,
        &terminal_event(
            leaf_session.session.id,
            TurnTerminalOutcome::Completed,
            None,
        ),
    )
    .await;
    let handoff = runtime
        .store
        .session_repo()
        .agent_completion_handoff(leaf_session.session.id, leaf_turn_id)
        .expect("durable child handoff")
        .expect("child completion receipt");
    assert_eq!(
        runtime
            .store
            .session_repo()
            .schedulable_owner_resume_request_id(owner_session.session.id)
            .expect("normal completion owner resume"),
        None
    );
    let root_execution = runtime
        .begin_root(
            &root_session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    let tree = root_execution.context.tree.clone();
    let owner_path = AgentPath::try_from("/root/owner").expect("owner path");
    let leaf_path = AgentPath::try_from("/root/owner/leaf").expect("leaf path");
    for _ in 0..128 {
        let _ = tree
            .control
            .commit_and_enqueue_mail(&leaf_path, &owner_path, false, || {
                Ok(AgentMailCommit {
                    history_item_id: HistoryItemId::new(),
                    schedule_turn: false,
                    owner_resume_request_id: None,
                })
            })
            .expect("fill informational mailbox");
    }

    let scheduled = runtime
        .project_completion_handoff(&tree, &leaf_path, &handoff)
        .expect("full-mailbox handoff projection");
    assert!(
        scheduled.is_empty(),
        "mailbox backpressure must not turn a FINAL into an implicit resume"
    );
    assert_eq!(
        tree.control
            .list_agents(Some(&owner_path))
            .expect("owner snapshot")
            .into_iter()
            .next()
            .expect("owner")
            .pending_mail_count,
        128,
        "failed identity-only notice projection must not discard existing mail"
    );
    let saturated_mail = tree
        .control
        .drain_mailbox(&owner_path)
        .expect("drain saturated informational mailbox");
    assert_eq!(saturated_mail.len(), 128);
    assert!(
        saturated_mail
            .iter()
            .all(|notice| notice.history_item_id != handoff.history_item_id)
    );
    assert!(
        runtime
            .project_completion_handoff(&tree, &leaf_path, &handoff)
            .expect("retry durable handoff projection")
            .is_empty()
    );
    let retried_notice = tree
        .control
        .drain_mailbox(&owner_path)
        .expect("retried completion mailbox");
    assert_eq!(retried_notice.len(), 1);
    assert_eq!(retried_notice[0].history_item_id, handoff.history_item_id);
    assert!(!retried_notice[0].trigger_turn);
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root");
}

#[tokio::test]
async fn late_child_final_is_retained_without_resuming_terminal_parent() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("late-final-terminal-parent", 2).await;
    let root_execution = runtime
        .begin_root(
            &root_session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    let tree = root_execution.context.tree.clone();
    let parent_session_id = SessionId::new();
    let parent = tree
        .control
        .restore_inactive_child(
            &AgentPath::root(),
            "parent",
            parent_session_id,
            InactiveAgentStatus::Errored("provider failure".to_string()),
            None,
        )
        .expect("terminal parent");
    let leaf_session_id = SessionId::new();
    let leaf = tree
        .control
        .restore_inactive_child(
            &parent.path,
            "leaf",
            leaf_session_id,
            InactiveAgentStatus::Completed(None),
            None,
        )
        .expect("late leaf");
    let history_item_id = HistoryItemId::new();
    let handoff = StoredAgentCompletionHandoff {
        child_session_id: leaf_session_id,
        child_turn_id: TurnId::new(),
        parent_session_id,
        parent_agent_path: parent.path.clone(),
        history_item_id,
        released_owner_deferred_turn_id: None,
    };

    assert!(
        runtime
            .project_completion_handoff(&tree, &leaf.path, &handoff)
            .expect("late direct-parent projection")
            .is_empty()
    );
    let notices = tree
        .control
        .drain_mailbox(&parent.path)
        .expect("terminal parent mailbox");
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].history_item_id, history_item_id);
    assert!(!notices[0].trigger_turn);
    assert!(
        tree.control
            .schedule_pending_triggered_executions()
            .expect("terminal parent is not resumed")
            .is_empty()
    );
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root");
}

async fn child_finish_fixture(
    test_name: &str,
) -> (
    Arc<AgentRuntime>,
    AgentRuntimeExecution,
    AgentRunContext,
    AgentExecutionLease,
    crate::session::SessionContext,
) {
    child_finish_fixture_with_capacity(test_name, 2).await
}

async fn child_finish_fixture_with_capacity(
    test_name: &str,
    max_concurrent_agents: usize,
) -> (
    Arc<AgentRuntime>,
    AgentRuntimeExecution,
    AgentRunContext,
    AgentExecutionLease,
    crate::session::SessionContext,
) {
    let (runtime, root_session, config) =
        direct_runtime_fixture(test_name, max_concurrent_agents).await;
    runtime
        .store
        .session_repo()
        .admit_session_turn(root_session.session.id, TurnId::new())
        .await
        .expect("admit root mail recipient")
        .expect("root mail recipient admission");
    let root_execution = runtime
        .begin_root(
            &root_session,
            captured_turn_config(config.clone()),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    let tree = root_execution.context.tree.clone();
    let child = runtime
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some(format!("{test_name}-child")),
                cwd: root_session.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            root_session.workspace.clone(),
        )
        .await
        .expect("child session");
    let child_path = AgentPath::root().join("child").expect("child path");
    runtime
        .store
        .session_repo()
        .insert_session_spawn_edge(
            root_session.session.id,
            root_session.session.id,
            child.session.id,
            child_path.as_str(),
            "child",
        )
        .await
        .expect("child spawn edge");
    let (_, child_lease) = tree
        .control
        .register_child(
            &AgentPath::root(),
            "child",
            child.session.id,
            Some("durable terminal authority test".to_string()),
        )
        .expect("child registration");
    let child_context = AgentRunContext {
        runtime: runtime.clone(),
        tree,
        path: child_path,
        session_id: child.session.id,
        wake_cause: None,
        execution: child_lease.scope(),
        turn_owner: Arc::new(OnceLock::new()),
        config: captured_turn_config(config),
        workspace: child.workspace.clone(),
        confirmation: root_execution.context.confirmation.clone(),
        run_service: root_execution.context.run_service.clone(),
    };
    (runtime, root_execution, child_context, child_lease, child)
}

#[tokio::test]
async fn completed_root_keeps_slow_child_live_and_retains_its_late_direct_result() {
    let (runtime, root_execution, child_context, child_lease, child) =
        child_finish_fixture("independent-root-child-terminal").await;
    let tree = child_context.tree.clone();
    let child_cancel = child_lease.cancel_token();
    let root_session_id = root_execution.context.session_id();
    let root_target = runtime
        .store
        .session_repo()
        .captured_running_terminal_target(root_session_id)
        .await
        .expect("capture root terminal owner")
        .expect("running root");
    let root_terminal = DurableTurnTerminal {
        outcome: TurnTerminalOutcome::Completed,
        final_response_id: None,
        tool_call_count: 0,
        failed_tool_count: 0,
        change_count: 0,
        metrics: Default::default(),
    };
    let root_event = RunEvent::TurnTerminal {
        session_id: root_session_id,
        terminal: Box::new(root_terminal.clone()),
    };
    assert!(
        runtime
            .store
            .session_repo()
            .terminalize_captured_running_session_with_protocol_event(
                root_session_id,
                &root_event,
                root_target,
            )
            .await
            .expect("terminalize root while child remains active")
    );
    let root_result = Ok(RunSummary::from_terminal(
        root_session_id,
        root_target.turn_id(),
        root_terminal,
    ));
    assert!(root_execution.run_control().seal_success());
    runtime.complete_root(root_execution, &root_result, None);

    let root_snapshot = tree
        .control
        .list_agents(Some(&AgentPath::root()))
        .expect("root projection")
        .into_iter()
        .next()
        .expect("root");
    assert_eq!(root_snapshot.status, AgentStatus::Completed(None));
    assert!(!root_snapshot.is_active);
    assert!(!child_cancel.is_cancelled());
    assert!(
        tree.control
            .list_agents(Some(child_context.path()))
            .expect("child projection")
            .into_iter()
            .next()
            .expect("slow child")
            .is_active,
        "root terminal must not consume the child's execution owner"
    );

    let result = Ok(terminalize_child_summary(
        &runtime,
        child.session.id,
        TurnTerminalOutcome::Completed,
        Some((ModelResponseId::new(), CHILD_RESULT)),
    )
    .await);
    let completion = runtime
        .finish_agent_turn(&child_context, &result, None)
        .await;
    assert!(matches!(completion.status, AgentStatus::Completed(_)));
    let child_turn_id = result.as_ref().expect("child summary").turn_id();
    let handoff = runtime
        .store
        .session_repo()
        .agent_completion_handoff(child.session.id, child_turn_id)
        .expect("late child handoff lookup")
        .expect("late child result is durable");
    assert_eq!(handoff.parent_session_id, root_session_id);
    let root_after_handoff = tree
        .control
        .list_agents(Some(&AgentPath::root()))
        .expect("root after late result")
        .into_iter()
        .next()
        .expect("retained root");
    assert_eq!(root_after_handoff.status, AgentStatus::Completed(None));
    assert!(!root_after_handoff.is_active);
    assert_eq!(root_after_handoff.pending_mail_count, 1);
    assert!(
        runtime
            .store
            .session_repo()
            .schedulable_owner_resume_request_id(root_session_id)
            .expect("root owner-resume lookup")
            .is_none()
    );

    tree.control
        .complete_execution(
            child_lease,
            inactive_agent_status(completion.status, completion.awaiting_deferred_turn_id)
                .expect("completed child status"),
            Some(CHILD_RESULT.to_string()),
        )
        .expect("complete child");
}

#[tokio::test]
async fn child_permission_uses_live_root_mode_and_keeps_the_admitted_process_plan() {
    let (runtime, _root_execution, child_context, _child_lease, child) =
        child_finish_fixture("child-live-root-permission").await;
    let root_session_id = child_context.root_session_id();
    runtime
        .store
        .session_repo()
        .compare_and_set_root_session_access_mode(
            root_session_id,
            AccessMode::Default,
            AccessMode::FullAccess,
        )
        .await
        .expect("root mode update")
        .expect("root access owner");
    assert_eq!(child.session.access_mode, AccessMode::Default);

    let config = child_context.effective_config();
    let services = ToolServices {
        edit_safety: crate::edit::EditSafety::default(),
        formatter: crate::edit::Formatter::new(config.format.clone()),
        change_tracker: crate::edit::ChangeTracker::default(),
        store: runtime.store.clone(),
        storage_paths: runtime.store.paths().clone(),
        truncator: ToolTruncator,
        mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
        skills: crate::skill::SkillsService::new(),
    };
    let control = RunControl::new();
    let mut prompt = AllowPrompt;
    let mut context = ToolContext {
        session: &child,
        workspace: &child.workspace,
        config: &config,
        tool_call_id: ToolCallId::new(),
        cancel: control.token(),
        run_control: control.clone(),
        run_mutation_fence: RunMutationFence::new(
            runtime.store.session_repo(),
            child.session.id,
            AdmissionId::new(),
            TurnId::new(),
            control,
        ),
        prompt: &mut prompt,
        services: &services,
        agent: Some(&child_context),
        permission_guardian: None,
    };

    let admitted_before_switch = context
        .confirm_if_needed(
            crate::workspace::AccessKind::Shell,
            "child shell before root downgrade".to_string(),
            Vec::new(),
            false,
            Vec::new(),
        )
        .await
        .expect("child full-access admission");
    assert!(matches!(
        admitted_before_switch.sandbox_plan(),
        crate::tool::os_sandbox::ProcessSandboxPlan::Unrestricted
    ));

    runtime
        .store
        .session_repo()
        .compare_and_set_root_session_access_mode(
            root_session_id,
            AccessMode::FullAccess,
            AccessMode::Default,
        )
        .await
        .expect("root mode downgrade")
        .expect("root access owner");

    let admitted_after_switch = context
        .confirm_if_needed(
            crate::workspace::AccessKind::Shell,
            "child shell after root downgrade".to_string(),
            Vec::new(),
            false,
            Vec::new(),
        )
        .await
        .expect("child workspace-write admission");
    assert!(matches!(
        admitted_after_switch.sandbox_plan(),
        crate::tool::os_sandbox::ProcessSandboxPlan::WorkspaceWrite(_)
    ));
    assert!(matches!(
        admitted_before_switch.sandbox_plan(),
        crate::tool::os_sandbox::ProcessSandboxPlan::Unrestricted
    ));
}

#[tokio::test]
async fn child_guardian_authority_owner_is_the_durable_root_not_the_forked_child_history() {
    let (_runtime, _root_execution, child_context, _child_lease, child) =
        child_finish_fixture("child-root-guardian-authority").await;

    assert_ne!(child.session.id, child_context.root_session_id());
    assert_eq!(
        crate::agent::permission_guardian_authority_session_id(
            child.session.id,
            Some(&child_context),
        ),
        child_context.root_session_id(),
        "child model-context fork contents must not own real-user authorization"
    );
    assert_eq!(
        crate::agent::permission_guardian_authority_session_id(child.session.id, None),
        child.session.id,
        "a top-level request owns its own durable real-user history"
    );
}

fn terminal_summary(session_id: SessionId, outcome: TurnTerminalOutcome) -> RunSummary {
    RunSummary::from_terminal(
        session_id,
        TurnId::new(),
        DurableTurnTerminal {
            outcome,
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        },
    )
}

async fn terminalize_child_summary(
    runtime: &AgentRuntime,
    session_id: SessionId,
    outcome: TurnTerminalOutcome,
    final_response: Option<(ModelResponseId, &str)>,
) -> RunSummary {
    let turn_id = TurnId::new();
    let admission = runtime
        .store
        .session_repo()
        .admit_session_turn(session_id, turn_id)
        .await
        .expect("admit child terminal fixture")
        .expect("child terminal fixture admission");
    let final_response_id = final_response.map(|(response_id, text)| {
        runtime
            .store
            .protocol_event_store()
            .seed_history_item_for_test(&HistoryItem {
                id: HistoryItemId::new(),
                session_id,
                scope: HistoryScope::Turn { turn_id },
                sequence_no: 1,
                created_at_ms: SystemClock::now_ms(),
                payload: HistoryItemPayload::AssistantMessage {
                    response_id,
                    content: vec![ContentPart::Text {
                        text: text.to_string(),
                    }],
                },
            })
            .expect("exact child final response");
        response_id
    });
    let terminal = DurableTurnTerminal {
        outcome,
        final_response_id,
        tool_call_count: 0,
        failed_tool_count: 0,
        change_count: 0,
        metrics: Default::default(),
    };
    let event = RunEvent::TurnTerminal {
        session_id,
        terminal: Box::new(terminal.clone()),
    };
    terminalize_admitted_test_session(runtime, session_id, admission.admission_id, turn_id, &event)
        .await;
    RunSummary::from_terminal(session_id, turn_id, terminal)
}

fn admitted_turn(outcome: crate::app::AppCommandOutcome) -> RunSummary {
    match outcome {
        crate::app::AppCommandOutcome::Turn(summary) => summary,
        crate::app::AppCommandOutcome::ControlCompleted => {
            panic!("expected an admitted turn, got a control-only completion")
        }
    }
}

fn terminal_event(
    session_id: SessionId,
    outcome: TurnTerminalOutcome,
    final_response_id: Option<ModelResponseId>,
) -> RunEvent {
    RunEvent::TurnTerminal {
        session_id,
        terminal: Box::new(DurableTurnTerminal {
            outcome,
            final_response_id,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
    }
}

async fn terminalize_test_session(
    runtime: &AgentRuntime,
    session_id: SessionId,
    turn_id: TurnId,
    event: &RunEvent,
) {
    let admission_id = runtime
        .store
        .session_repo()
        .admit_session_turn(session_id, turn_id)
        .await
        .expect("admit terminal fixture")
        .expect("terminal fixture admission");
    terminalize_admitted_test_session(
        runtime,
        session_id,
        admission_id.admission_id,
        turn_id,
        event,
    )
    .await;
}

async fn terminalize_admitted_test_session(
    runtime: &AgentRuntime,
    session_id: SessionId,
    admission_id: crate::session::AdmissionId,
    turn_id: TurnId,
    event: &RunEvent,
) {
    assert!(
        runtime
            .store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                event,
                turn_id,
                None,
                None,
            )
            .await
            .expect("terminalize fixture")
            .was_applied()
    );
}

async fn bind_test_root_turn(
    runtime: &AgentRuntime,
    execution: &AgentRuntimeExecution,
) -> AgentDurableTurnOwner {
    let turn_id = TurnId::new();
    let admission = runtime
        .store
        .session_repo()
        .admit_session_turn(execution.context.session_id(), turn_id)
        .await
        .expect("admit test root turn")
        .expect("test root turn admission");
    execution
        .context
        .bind_durable_turn_owner(admission.admission_id, turn_id)
        .expect("bind test durable turn owner");
    AgentDurableTurnOwner {
        session_id: execution.context.session_id(),
        admission_id: admission.admission_id,
        turn_id,
    }
}

fn append_child_history(
    runtime: &AgentRuntime,
    session_id: SessionId,
    payload: HistoryItemPayload,
) {
    runtime
        .store
        .protocol_event_store()
        .seed_history_item_for_test(&HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn {
                turn_id: TurnId::new(),
            },
            sequence_no: 0,
            created_at_ms: SystemClock::now_ms(),
            payload,
        })
        .expect("child history");
}

#[tokio::test]
async fn durable_child_tree_terminal_interruptions_suppress_mail_despite_stale_local_state() {
    for (index, cause) in [
        TurnInterruptionCause::ApprovalAborted,
        TurnInterruptionCause::UserStop,
        TurnInterruptionCause::TreeStopped,
    ]
    .into_iter()
    .enumerate()
    {
        for local_cause in [None, Some(RunCancellationCause::Superseded)] {
            let (runtime, root_execution, context, child_lease, mut child) =
                child_finish_fixture(&format!(
                    "durable-child-suppression-{index}-{}",
                    local_cause.is_some()
                ))
                .await;
            let result = if cause == TurnInterruptionCause::TreeStopped {
                let child_turn_id = TurnId::new();
                let child_admission = runtime
                    .store
                    .session_repo()
                    .admit_session_turn(child.session.id, child_turn_id)
                    .await
                    .expect("admit child before explicit tree Stop")
                    .expect("child tree-Stop admission");
                assert!(
                    runtime
                        .store
                        .session_repo()
                        .record_agent_tree_stop_fence(
                            root_execution.context.session_id(),
                            TurnInterruptionCause::UserStop,
                        )
                        .await
                        .expect("record explicit root tree-Stop boundary")
                        .is_some()
                );
                let terminal = DurableTurnTerminal {
                    outcome: TurnTerminalOutcome::Interrupted { cause },
                    final_response_id: None,
                    tool_call_count: 0,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: Default::default(),
                };
                terminalize_admitted_test_session(
                    &runtime,
                    child.session.id,
                    child_admission.admission_id,
                    child_turn_id,
                    &RunEvent::TurnTerminal {
                        session_id: child.session.id,
                        terminal: Box::new(terminal.clone()),
                    },
                )
                .await;
                Ok(RunSummary::from_terminal(
                    child.session.id,
                    child_turn_id,
                    terminal,
                ))
            } else {
                Ok(terminalize_child_summary(
                    &runtime,
                    child.session.id,
                    TurnTerminalOutcome::Interrupted { cause },
                    None,
                )
                .await)
            };

            let status = runtime
                .finish_agent_turn(&context, &result, local_cause)
                .await
                .status;

            assert_eq!(status, AgentStatus::Interrupted);
            child.session.status = SessionStatus::Cancelled;
            assert_eq!(
                rehydrated_agent_state(child.session.id, child.session.status, None, Some(cause))
                    .expect("typed cancellation must rehydrate"),
                status
            );
            assert!(
                context
                    .tree
                    .control
                    .drain_mailbox(&AgentPath::root())
                    .expect("root mailbox")
                    .is_empty()
            );
            assert!(
                runtime
                    .store
                    .session_repo()
                    .agent_completion_handoff(child.session.id, result.as_ref().unwrap().turn_id())
                    .expect("interrupted handoff lookup")
                    .is_none()
            );
            context
                .tree
                .control
                .complete_execution(
                    child_lease,
                    inactive_agent_status(status, None).expect("inactive child status"),
                    None,
                )
                .expect("complete child");
            root_execution
                .complete(AgentStatus::Completed(None))
                .expect("complete root");
        }
    }
}

#[tokio::test]
async fn durable_child_agent_interruption_has_no_completion_handoff_or_parent_notification() {
    let (runtime, root_execution, context, child_lease, child) =
        child_finish_fixture("durable-child-agent-interrupted").await;
    let result = Ok(terminalize_child_summary(
        &runtime,
        child.session.id,
        TurnTerminalOutcome::Interrupted {
            cause: TurnInterruptionCause::AgentInterrupted,
        },
        None,
    )
    .await);

    let status = runtime
        .finish_agent_turn(&context, &result, None)
        .await
        .status;

    assert_eq!(status, AgentStatus::Interrupted);
    let mail = context
        .tree
        .control
        .drain_mailbox(&AgentPath::root())
        .expect("root mailbox");
    assert!(mail.is_empty());
    assert!(
        runtime
            .store
            .session_repo()
            .agent_completion_handoff(child.session.id, result.as_ref().unwrap().turn_id())
            .expect("interrupted handoff lookup")
            .is_none()
    );
    context
        .tree
        .control
        .complete_execution(
            child_lease,
            inactive_agent_status(status, None).expect("inactive child status"),
            None,
        )
        .expect("complete child");
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root");
}

#[tokio::test]
async fn durable_child_failure_uses_terminal_error_despite_stale_history_and_local_stop() {
    let (runtime, root_execution, context, child_lease, child) =
        child_finish_fixture("durable-child-failed").await;
    append_child_history(
        &runtime,
        child.session.id,
        HistoryItemPayload::AssistantMessage {
            response_id: ModelResponseId::new(),
            content: vec![ContentPart::Text {
                text: "partial assistant text".to_string(),
            }],
        },
    );
    append_child_history(
        &runtime,
        child.session.id,
        HistoryItemPayload::Error {
            message: "durable final child failure".to_string(),
        },
    );
    let result = Ok(terminalize_child_summary(
        &runtime,
        child.session.id,
        TurnTerminalOutcome::Failed {
            error: "durable child failed".to_string(),
        },
        None,
    )
    .await);

    let status = runtime
        .finish_agent_turn(
            &context,
            &result,
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop,
            )),
        )
        .await
        .status;

    assert_eq!(
        status,
        AgentStatus::Errored("durable child failed".to_string())
    );
    let mail = context
        .tree
        .control
        .drain_mailbox(&AgentPath::root())
        .expect("root mailbox");
    assert_eq!(mail.len(), 1);
    assert_eq!(
        durable_mailbox_content(&context, &mail[0]),
        child_failure_final_answer("durable child failed")
    );
    context
        .tree
        .control
        .complete_execution(
            child_lease,
            inactive_agent_status(status, None).expect("inactive child status"),
            None,
        )
        .expect("complete child");
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root");
}

#[tokio::test]
async fn durable_failed_child_live_and_restart_use_terminal_error_across_stale_history() {
    for (index, history_payloads) in [
        vec![
            HistoryItemPayload::AssistantMessage {
                response_id: ModelResponseId::new(),
                content: vec![ContentPart::Text {
                    text: "partial durable assistant output".to_string(),
                }],
            },
            HistoryItemPayload::Error {
                message: "stale durable history error".to_string(),
            },
        ],
        vec![HistoryItemPayload::AssistantMessage {
            response_id: ModelResponseId::new(),
            content: vec![ContentPart::Text {
                text: "partial durable assistant output".to_string(),
            }],
        }],
        Vec::new(),
    ]
    .into_iter()
    .enumerate()
    {
        let (runtime, root_execution, context, child_lease, child) =
            child_finish_fixture(&format!("durable-failed-equality-{index}")).await;
        let root_session_id = context.root_session_id();
        for payload in history_payloads {
            append_child_history(&runtime, child.session.id, payload);
        }
        let result = Ok(terminalize_child_summary(
            &runtime,
            child.session.id,
            TurnTerminalOutcome::Failed {
                error: "durable child failed".to_string(),
            },
            None,
        )
        .await);

        let live_status = runtime
            .finish_agent_turn(&context, &result, None)
            .await
            .status;
        let store = runtime.store.clone();
        let restarted_runtime =
            AgentRuntime::new(store.clone(), crate::session::SessionService::new(store));
        let restarted_status = restarted_runtime
            .durable_activity_records(root_session_id)
            .await
            .expect("restarted durable child projection")
            .into_iter()
            .find(|record| record.session_id == child.session.id)
            .expect("restarted failed child")
            .status;

        assert_eq!(
            live_status,
            AgentStatus::Errored("durable child failed".to_string())
        );
        assert_eq!(live_status, restarted_status);
        let mail = context
            .tree
            .control
            .drain_mailbox(&AgentPath::root())
            .expect("root mailbox");
        assert_eq!(mail.len(), 1);
        assert_eq!(
            durable_mailbox_content(&context, &mail[0]),
            child_failure_final_answer(&agent_status_result(&restarted_status))
        );
        context
            .tree
            .control
            .complete_execution(
                child_lease,
                inactive_agent_status(live_status, None).expect("inactive child status"),
                None,
            )
            .expect("complete child");
        root_execution
            .complete(AgentStatus::Completed(None))
            .expect("complete root");
    }
}

#[tokio::test]
async fn completed_child_without_final_response_does_not_scan_unrelated_history() {
    let (runtime, root_execution, context, child_lease, child) =
        child_finish_fixture("durable-child-success").await;
    let content = "durable assistant result".to_string();
    append_child_history(
        &runtime,
        child.session.id,
        HistoryItemPayload::AssistantMessage {
            response_id: ModelResponseId::new(),
            content: vec![ContentPart::Text {
                text: content.clone(),
            }],
        },
    );
    let projection = runtime
        .store
        .protocol_event_store()
        .durable_child_result_projection(child.session.id)
        .expect("completed child projection");
    assert_eq!(
        durable_child_result_from_projection(
            SessionStatus::Completed,
            projection.latest_assistant_content.as_deref(),
            projection.latest_error.as_deref(),
        ),
        Some(content.clone())
    );
    let result = Ok(terminalize_child_summary(
        &runtime,
        child.session.id,
        TurnTerminalOutcome::Completed,
        None,
    )
    .await);

    let status = runtime
        .finish_agent_turn(
            &context,
            &result,
            Some(RunCancellationCause::Failure(
                "stale local failure".to_string(),
            )),
        )
        .await
        .status;

    assert_eq!(status, AgentStatus::Completed(None));
    let mail = context
        .tree
        .control
        .drain_mailbox(&AgentPath::root())
        .expect("root mailbox");
    assert_eq!(mail.len(), 1);
    assert_eq!(
        durable_mailbox_content(&context, &mail[0]),
        child_final_answer("")
    );
    context
        .tree
        .control
        .complete_execution(
            child_lease,
            inactive_agent_status(status, None).expect("inactive child status"),
            None,
        )
        .expect("complete child");
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root");
}

#[tokio::test]
async fn completed_child_result_uses_terminal_response_identity_without_history_scan() {
    let (runtime, root_execution, context, child_lease, child) =
        child_finish_fixture("durable-child-terminal-response").await;
    let final_response_id = ModelResponseId::new();
    let result = Ok(terminalize_child_summary(
        &runtime,
        child.session.id,
        TurnTerminalOutcome::Completed,
        Some((final_response_id, "terminal response result")),
    )
    .await);
    append_child_history(
        &runtime,
        child.session.id,
        HistoryItemPayload::AssistantMessage {
            response_id: ModelResponseId::new(),
            content: vec![ContentPart::Text {
                text: "later non-terminal assistant text".to_string(),
            }],
        },
    );
    let completion = runtime.finish_agent_turn(&context, &result, None).await;

    assert_eq!(
        completion.status,
        AgentStatus::Completed(Some("terminal response result".to_string()))
    );
    let receipt = runtime
        .store
        .session_repo()
        .agent_completion_handoff(child.session.id, result.as_ref().unwrap().turn_id())
        .expect("completion handoff query")
        .expect("completion handoff receipt");
    assert_eq!(receipt.parent_agent_path, AgentPath::root());
    assert_eq!(receipt.parent_session_id, context.root_session_id());
    assert_eq!(
        runtime
            .store
            .session_repo()
            .schedulable_owner_resume_request_id(context.root_session_id())
            .expect("root continuation query"),
        None
    );
    assert!(
        runtime
            .store
            .protocol_event_store()
            .list_history_items_for_session(context.root_session_id())
            .expect("root canonical history before safe delivery")
            .into_iter()
            .all(|item| item.id != receipt.history_item_id),
        "durable receipt identity is owned by the mailbox until the parent samples it"
    );
    let mail = context
        .tree
        .control
        .drain_mailbox(&AgentPath::root())
        .expect("root mailbox");
    assert_eq!(mail.len(), 1);
    assert_eq!(mail[0].history_item_id, receipt.history_item_id);
    assert_eq!(
        durable_mailbox_content(&context, &mail[0]),
        child_final_answer("terminal response result")
    );
    context
        .tree
        .control
        .complete_execution(
            child_lease,
            inactive_agent_status(completion.status, completion.awaiting_deferred_turn_id)
                .expect("inactive child status"),
            completion.activity,
        )
        .expect("complete child");
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root");
}

#[tokio::test]
async fn nested_completion_handoff_targets_only_the_immediate_parent_without_resuming_it() {
    let (runtime, root_execution, child_context, child_lease, child) =
        child_finish_fixture_with_capacity("nested-completion-immediate-parent", 3).await;
    let grandchild = runtime
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("grandchild".to_string()),
                cwd: child.workspace.cwd.clone(),
                model: child.session.model.clone(),
                base_url: child.session.base_url.clone(),
                access_mode: child.session.access_mode,
            },
            child.workspace.clone(),
        )
        .await
        .expect("grandchild session");
    let grandchild_path = child_context
        .path
        .join("grandchild")
        .expect("grandchild path");
    runtime
        .store
        .session_repo()
        .insert_session_spawn_edge(
            child_context.root_session_id(),
            child.session.id,
            grandchild.session.id,
            grandchild_path.as_str(),
            "grandchild",
        )
        .await
        .expect("grandchild spawn edge");
    let (_, grandchild_lease) = child_context
        .tree
        .control
        .register_child(
            &child_context.path,
            "grandchild",
            grandchild.session.id,
            Some("nested completion".to_string()),
        )
        .expect("grandchild registration");
    let grandchild_context = AgentRunContext {
        runtime: runtime.clone(),
        tree: child_context.tree.clone(),
        path: grandchild_path,
        session_id: grandchild.session.id,
        wake_cause: None,
        execution: grandchild_lease.scope(),
        turn_owner: Arc::new(OnceLock::new()),
        config: child_context.config.clone(),
        workspace: grandchild.workspace.clone(),
        confirmation: child_context.confirmation.clone(),
        run_service: child_context.run_service.clone(),
    };
    let result = Ok(terminalize_child_summary(
        &runtime,
        grandchild.session.id,
        TurnTerminalOutcome::Completed,
        Some((ModelResponseId::new(), "nested result")),
    )
    .await);

    let completion = runtime
        .finish_agent_turn(&grandchild_context, &result, None)
        .await;

    assert_eq!(
        completion.status,
        AgentStatus::Completed(Some("nested result".to_string()))
    );
    let receipt = runtime
        .store
        .session_repo()
        .agent_completion_handoff(grandchild.session.id, result.as_ref().unwrap().turn_id())
        .expect("nested completion handoff query")
        .expect("nested completion handoff");
    assert_eq!(receipt.parent_agent_path, child_context.path);
    assert_eq!(receipt.parent_session_id, child.session.id);
    assert_eq!(
        runtime
            .store
            .session_repo()
            .schedulable_owner_resume_request_id(child.session.id)
            .expect("normal nested completion owner resume"),
        None
    );
    assert!(
        child_context
            .tree
            .control
            .drain_mailbox(&AgentPath::root())
            .expect("root mailbox")
            .is_empty()
    );
    assert!(
        !child_context
            .tree
            .control
            .mailbox_has_ready_trigger_turn(&child_context.path)
            .expect("nested FINAL readiness"),
        "the immediate-parent FINAL must not auto-resume its recipient"
    );
    let child_mail = child_context
        .tree
        .control
        .drain_mailbox(&child_context.path)
        .expect("child mailbox");
    assert_eq!(child_mail.len(), 1);
    assert_eq!(child_mail[0].history_item_id, receipt.history_item_id);
    assert!(
        runtime
            .store
            .protocol_event_store()
            .list_history_items_for_session(child.session.id)
            .expect("child history before safe delivery")
            .into_iter()
            .all(|item| item.id != receipt.history_item_id),
        "the immediate parent's mailbox owns the nested FINAL before its next safe boundary"
    );
    let (_, nested_communication) = runtime
        .store
        .session_repo()
        .agent_mailbox_communications_by_id(child.session.id, &[child_mail[0].history_item_id])
        .expect("nested durable mailbox message")
        .into_iter()
        .next()
        .expect("nested mailbox communication");
    assert!(!nested_communication.trigger_turn);
    assert_eq!(
        nested_communication.content,
        "Message Type: FINAL_ANSWER\nTask name: /root/child\nSender: /root/child/grandchild\nPayload:\nnested result"
    );
    assert!(
        runtime
            .store
            .protocol_event_store()
            .list_history_items_for_session(child_context.root_session_id())
            .expect("root history")
            .into_iter()
            .all(|item| !matches!(
                &item.payload,
                HistoryItemPayload::InterAgentCommunication { communication }
                    if communication.author == grandchild_context.path.as_str()
            ))
    );
    child_context
        .tree
        .control
        .complete_execution(
            grandchild_lease,
            inactive_agent_status(completion.status, completion.awaiting_deferred_turn_id)
                .expect("inactive grandchild status"),
            completion.activity,
        )
        .expect("complete grandchild");
    child_context
        .tree
        .control
        .complete_execution(child_lease, InactiveAgentStatus::Completed(None), None)
        .expect("complete child");
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root");
}

#[tokio::test]
async fn child_result_delivery_survives_parent_durable_success_before_marker_release() {
    let (runtime, root_execution, context, child_lease, child) =
        child_finish_fixture("child-result-parent-success-transition").await;
    let root_session_id = root_execution.context.session_id;
    let _root_turn_id = runtime
        .store
        .session_repo()
        .fresh_running_turn_for_session(root_session_id)
        .await
        .expect("active root turn")
        .expect("root turn remains admitted");
    let root_target = runtime
        .store
        .session_repo()
        .captured_running_terminal_target(root_session_id)
        .await
        .expect("capture root target")
        .expect("root running target");
    assert!(
        runtime
            .store
            .session_repo()
            .terminalize_captured_running_session_with_protocol_event(
                root_session_id,
                &terminal_event(root_session_id, TurnTerminalOutcome::Completed, None,),
                root_target,
            )
            .await
            .expect("terminalize durable root before marker release")
    );
    assert!(
        root_execution
            .context
            .tree
            .control
            .list_agents(Some(&AgentPath::root()))
            .expect("root snapshot")
            .into_iter()
            .find(|agent| agent.path.is_root())
            .expect("root agent")
            .is_active,
        "the regression requires the durable terminal/in-memory active transition"
    );

    let content = "child result after parent durable success".to_string();
    let result = Ok(terminalize_child_summary(
        &runtime,
        child.session.id,
        TurnTerminalOutcome::Completed,
        Some((ModelResponseId::new(), &content)),
    )
    .await);

    let status = runtime
        .finish_agent_turn(&context, &result, None)
        .await
        .status;

    assert_eq!(status, AgentStatus::Completed(Some(content.clone())));
    let mail = context
        .tree
        .control
        .drain_mailbox(&AgentPath::root())
        .expect("root mailbox");
    assert_eq!(mail.len(), 1);
    assert_eq!(
        runtime
            .store
            .session_repo()
            .agent_mailbox_communications_by_id(root_session_id, &[mail[0].history_item_id])
            .expect("pending child result")
            .len(),
        1
    );
    assert!(
        runtime
            .store
            .protocol_event_store()
            .list_history_items_for_session(root_session_id)
            .expect("root history before a future safe boundary")
            .iter()
            .all(|item| item.id != mail[0].history_item_id)
    );
    assert_eq!(
        durable_mailbox_content(&context, &mail[0]),
        child_final_answer(&content)
    );
    context
        .tree
        .control
        .complete_execution(
            child_lease,
            inactive_agent_status(status, None).expect("inactive child status"),
            None,
        )
        .expect("complete child");
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root");
}

#[tokio::test]
async fn explicit_tree_shutdown_terminalizes_active_child_without_a_completion_handoff() {
    let (runtime, root_execution, context, child_lease, child) =
        child_finish_fixture("child-result-dead-parent").await;
    let root_session_id = root_execution.context.session_id;
    let child_turn_id = TurnId::new();
    let child_admission = runtime
        .store
        .session_repo()
        .admit_session_turn(child.session.id, child_turn_id)
        .await
        .expect("admit child before parent Stop")
        .expect("child admitted");
    context
        .bind_durable_turn_owner(child_admission.admission_id, child_turn_id)
        .expect("bind child durable owner");
    child_lease
        .set_status(ActiveAgentStatus::Running)
        .expect("running child projection");
    assert!(
        runtime
            .store
            .session_repo()
            .record_agent_tree_stop_fence(root_session_id, TurnInterruptionCause::UserStop)
            .await
            .expect("record explicit root tree-Stop boundary")
            .is_some()
    );
    let root_target = runtime
        .store
        .session_repo()
        .captured_running_terminal_target(root_session_id)
        .await
        .expect("capture root target")
        .expect("root running target");
    assert!(
        runtime
            .store
            .session_repo()
            .terminalize_captured_running_session_with_protocol_event(
                root_session_id,
                &terminal_event(
                    root_session_id,
                    TurnTerminalOutcome::Interrupted {
                        cause: TurnInterruptionCause::UserStop,
                    },
                    None,
                ),
                root_target,
            )
            .await
            .expect("cancel durable parent")
    );
    root_execution
        .complete(AgentStatus::Shutdown)
        .expect("shutdown parent");
    let child_terminal = DurableTurnTerminal {
        outcome: TurnTerminalOutcome::Interrupted {
            cause: TurnInterruptionCause::TreeStopped,
        },
        final_response_id: None,
        tool_call_count: 0,
        failed_tool_count: 0,
        change_count: 0,
        metrics: Default::default(),
    };
    assert_eq!(
        runtime
            .store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child.session.id,
                child_admission.admission_id,
                &RunEvent::TurnTerminal {
                    session_id: child.session.id,
                    terminal: Box::new(child_terminal.clone()),
                },
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("tree-stopped child terminal"),
        crate::storage::session_repo::AdmittedTerminalCommit::Applied
    );
    let stored_child_terminal = runtime
        .store
        .session_repo()
        .durable_terminal_for_turn(child.session.id, child_turn_id)
        .await
        .expect("shutdown child terminal read")
        .expect("child terminal");
    assert!(matches!(
        stored_child_terminal.outcome,
        TurnTerminalOutcome::Interrupted {
            cause: TurnInterruptionCause::TreeStopped
        }
    ));
    let result = Ok(RunSummary::from_terminal(
        child.session.id,
        child_turn_id,
        child_terminal,
    ));

    let completion = runtime.finish_agent_turn(&context, &result, None).await;

    assert_eq!(completion.status, AgentStatus::Interrupted);
    assert!(
        runtime
            .store
            .protocol_event_store()
            .list_history_items_for_session(root_session_id)
            .expect("parent history")
            .iter()
            .all(|item| !matches!(
                item.payload,
                HistoryItemPayload::InterAgentCommunication { .. }
            ))
    );
    assert!(
        context
            .tree
            .control
            .drain_mailbox(&AgentPath::root())
            .expect("parent mailbox")
            .is_empty()
    );
    assert!(
        runtime
            .store
            .session_repo()
            .agent_completion_handoff(child.session.id, result.as_ref().unwrap().turn_id())
            .expect("cancelled-parent handoff lookup")
            .is_none()
    );
    context
        .tree
        .control
        .complete_execution(
            child_lease,
            inactive_agent_status(completion.status, completion.awaiting_deferred_turn_id)
                .expect("inactive child status"),
            completion.activity,
        )
        .expect("complete child");
}

#[tokio::test]
async fn rehydrated_child_followup_uses_current_root_config_and_workspace() {
    let (runtime, root_session, mut config) =
        direct_runtime_fixture("followup-admitted-access", 3).await;
    let child_path = AgentPath::root().join("research").expect("child path");
    let persisted_child_cwd = root_session.workspace.cwd.join("persisted-child-only-cwd");
    let child = runtime
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("research".to_string()),
                cwd: persisted_child_cwd.clone(),
                model: "persisted-old-child-model".to_string(),
                base_url: config.model.base_url.clone(),
                access_mode: AccessMode::Default,
            },
            root_session.workspace.clone(),
        )
        .await
        .expect("child session");
    runtime
        .store
        .session_repo()
        .insert_session_spawn_edge(
            root_session.session.id,
            root_session.session.id,
            child.session.id,
            child_path.as_str(),
            "research",
        )
        .await
        .expect("spawn edge");
    terminalize_test_session(
        &runtime,
        child.session.id,
        TurnId::new(),
        &terminal_event(child.session.id, TurnTerminalOutcome::Completed, None),
    )
    .await;
    assert_eq!(child.session.cwd, persisted_child_cwd);
    let store = runtime.store.clone();
    drop(runtime);
    let runtime = Arc::new(AgentRuntime::new(
        store.clone(),
        crate::session::SessionService::new(store),
    ));
    config.permissions.access_mode = AccessMode::FullAccess;
    config.model.model = "current-root-resume-model".to_string();
    let root = runtime
        .begin_root(
            &root_session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    bind_test_root_turn(&runtime, &root).await;
    assert!(matches!(
        root.context
            .tree
            .control
            .status(&child_path)
            .expect("rehydrated child status"),
        AgentStatus::Completed(None)
    ));
    let child_lease = root
        .context
        .tree
        .control
        .try_acquire_execution(&child_path)
        .expect("child execution");
    let child_context = runtime
        .context_for_execution(&root.context.tree, &child_lease)
        .expect("rehydrated child context");
    assert_eq!(child_context.workspace.cwd, root_session.workspace.cwd);
    assert_eq!(
        child_context.config.runtime_config().model.model,
        "current-root-resume-model"
    );
    assert!(
        runtime
            .activity_records(root_session.session.id)
            .iter()
            .find(|record| record.agent_path == child_path.to_string())
            .is_some_and(|record| !record.is_current_turn),
        "an unadmitted child execution must not borrow the root durable turn owner"
    );

    let materialized = runtime
        .materialize_context_config_and_sync_session(&child_context)
        .await
        .expect("materialized followup config");

    assert_eq!(materialized.permissions.access_mode, AccessMode::FullAccess);
    assert_eq!(materialized.model.model, "current-root-resume-model");
    assert_eq!(
        runtime
            .store
            .session_repo()
            .get_session(child.session.id)
            .await
            .expect("durable child")
            .access_mode,
        AccessMode::FullAccess
    );
    root.context
        .tree
        .control
        .complete_execution(child_lease, InactiveAgentStatus::Completed(None), None)
        .expect("complete config follow-up fixture");
    root.complete(AgentStatus::Completed(None))
        .expect("complete config root fixture");
}

#[tokio::test]
async fn root_broker_is_per_execution_and_quiescent_tree_keeps_immutable_limits() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");
    let storage_paths = StoragePaths {
        data_dir: root.join(".moyai-data"),
        database_path: root.join(".moyai-data/moyai.sqlite3"),
        truncation_dir: root.join(".moyai-data/truncation"),
    };
    let sqlite = SqliteStore::open(&storage_paths).expect("store");
    sqlite.migrate().expect("migrate");
    let store = StoreBundle::new(sqlite);
    let mut config = ResolvedConfig::default();
    config.multi_agent.enabled = true;
    config.multi_agent.max_concurrent_agents = 3;
    config.multi_agent.max_concurrent_model_requests = 2;
    let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
    store
        .project_repo()
        .upsert_project(
            workspace.project_id,
            &workspace.root,
            "agent-tree-broker-test",
            "none",
        )
        .await
        .expect("project");
    let session_service = crate::session::SessionService::new(store.clone());
    let session = session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("tree broker".to_string()),
                cwd: root,
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            workspace,
        )
        .await
        .expect("session");
    let runtime = Arc::new(AgentRuntime::new(store, session_service));
    let original = SharedConfirmationPrompt::new(AllowPrompt);
    let replacement = SharedConfirmationPrompt::new(AllowPrompt);
    assert!(!original.shares_broker_with(&replacement));

    let first = runtime
        .begin_root(
            &session,
            captured_turn_config(config.clone()),
            original.clone(),
            RunControl::new(),
        )
        .await
        .expect("first root turn");
    let first_context_broker = first.context.confirmation_prompt();
    let first_gate = first.context.model_request_gate();
    assert!(first_context_broker.shares_broker_with(&original));
    assert_eq!(first_gate.available_permits(), 2);
    first
        .complete(AgentStatus::Completed(None))
        .expect("complete first root");

    let resumed = runtime
        .begin_root(
            &session,
            captured_turn_config(config.clone()),
            replacement.clone(),
            RunControl::new(),
        )
        .await
        .expect("resumed root turn");
    let resumed_broker = resumed.context.confirmation_prompt();
    let resumed_gate = resumed.context.model_request_gate();
    assert!(!resumed_broker.shares_broker_with(&original));
    assert!(resumed_broker.shares_broker_with(&replacement));
    assert!(Arc::ptr_eq(&first_gate, &resumed_gate));
    assert_eq!(resumed_gate.available_permits(), 2);
    assert_eq!(
        resumed
            .context
            .tree
            .control
            .snapshot()
            .expect("resumed tree")
            .max_concurrent_agents,
        3
    );
    resumed
        .complete(AgentStatus::Completed(None))
        .expect("complete resumed root");

    let mut mismatched = config;
    mismatched.multi_agent.max_concurrent_agents = 1;
    mismatched.multi_agent.max_concurrent_model_requests = 1;
    let error = match runtime
        .begin_root(
            &session,
            captured_turn_config(mismatched),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
    {
        Ok(_) => panic!("a retained quiescent tree must not replace its immutable scheduler"),
        Err(error) => error,
    };
    assert!(error.contains("immutable limits"));
    assert!(error.contains("max_concurrent_agents=3"));
    assert!(error.contains("max_concurrent_model_requests=2"));
}

#[tokio::test]
async fn active_old_child_and_new_root_keep_isolated_permission_brokers() {
    let (runtime, session, mut config) =
        direct_runtime_fixture("active-child-root-reentry", 2).await;
    config.multi_agent.max_concurrent_model_requests = 2;
    config.model.model = "old-child-model".to_string();
    let original_broker = SharedConfirmationPrompt::new(AbortPrompt);
    let first = runtime
        .begin_root(
            &session,
            captured_turn_config(config.clone()),
            original_broker.clone(),
            RunControl::new(),
        )
        .await
        .expect("first root turn");
    let tree = first.context.tree.clone();
    let original_gate = first.context.model_request_gate();
    let child_session_id = SessionId::new();
    let (child, child_execution) = tree
        .control
        .register_child(
            &AgentPath::root(),
            "detached",
            child_session_id,
            Some("detached work".to_string()),
        )
        .expect("active child");
    tree.metadata.lock().expect("agent metadata").insert(
        child.path.clone(),
        AgentNodeMetadata {
            task_name: "detached".to_string(),
            task_preview: "detached work".to_string(),
            config: first.context.config.clone(),
            workspace: first.context.workspace.clone(),
            confirmation: first.context.confirmation.clone(),
            run_service: first.context.run_service.clone(),
            updated: false,
            activity_owner: None,
        },
    );
    let child_control = child_execution.run_control();
    assert!(first.run_control().seal_success());
    runtime.complete_root(
        first,
        &Ok(terminal_summary(
            session.session.id,
            TurnTerminalOutcome::Completed,
        )),
        None,
    );

    let mut mismatched = config.clone();
    mismatched.multi_agent.max_concurrent_agents = 1;
    mismatched.multi_agent.max_concurrent_model_requests = 1;
    let mismatch_error = match runtime
        .begin_root(
            &session,
            captured_turn_config(mismatched),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
    {
        Ok(_) => panic!("live scheduler limit mismatch must fail before model sampling"),
        Err(error) => error,
    };
    assert!(mismatch_error.contains("immutable limits"));
    assert!(mismatch_error.contains("max_concurrent_agents=2"));
    assert!(mismatch_error.contains("max_concurrent_model_requests=2"));

    let replacement_broker = SharedConfirmationPrompt::new(AllowPrompt);
    let replacement_scope = RunControl::new();
    let mut replacement_config = config;
    replacement_config.model.model = "new-root-model".to_string();
    let second = runtime
        .begin_root(
            &session,
            captured_turn_config(replacement_config),
            replacement_broker.clone(),
            replacement_scope.clone(),
        )
        .await
        .expect("root reentry while child remains active");
    let child_context = runtime
        .context_for_execution(&tree, &child_execution)
        .expect("existing child reconstructs its captured resources");

    assert!(
        child_context
            .confirmation_prompt()
            .shares_broker_with(&original_broker)
    );
    assert!(
        !child_context
            .confirmation_prompt()
            .shares_broker_with(&replacement_broker)
    );
    assert!(
        second
            .context
            .confirmation_prompt()
            .shares_broker_with(&replacement_broker)
    );
    assert!(
        !second
            .context
            .confirmation_prompt()
            .shares_broker_with(&original_broker)
    );
    assert_eq!(
        child_context.config.runtime_config().model.model,
        "old-child-model"
    );
    assert_eq!(
        second.context.config.runtime_config().model.model,
        "new-root-model"
    );
    assert!(Arc::ptr_eq(
        &original_gate,
        &second.context.model_request_gate()
    ));
    let request = crate::tool::PermissionRequest {
        access: crate::workspace::AccessKind::Edit,
        summary: "abort only the old child".to_string(),
        details: Vec::new(),
        targets: Vec::new(),
        outside_workspace: false,
        risks: Vec::new(),
        agent_path: Some(child.path.to_string()),
        agent_task_name: Some("detached".to_string()),
    };
    let outcome = child_context
        .confirmation_prompt()
        .confirm_with_control(&request, &child_control)
        .expect("old child permission abort");
    assert_eq!(outcome, crate::cli::ConfirmationOutcome::Aborted);
    assert_eq!(
        child_control.cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted
        ))
    );
    assert_eq!(replacement_scope.cause(), None);
    assert_eq!(second.run_control().cause(), None);
    tree.control
        .complete_execution(child_execution, InactiveAgentStatus::Interrupted, None)
        .expect("settle old child");
    second
        .complete(AgentStatus::Completed(None))
        .expect("settle new root");
}

#[tokio::test]
async fn process_host_survives_app_swap_across_running_pending_and_awaiting_states() {
    let temp = tempfile::tempdir().expect("tempdir");
    let first_workspace =
        Utf8PathBuf::from_path_buf(temp.path().join("workspace-a")).expect("utf8 workspace");
    let second_workspace =
        Utf8PathBuf::from_path_buf(temp.path().join("workspace-b")).expect("utf8 workspace");
    std::fs::create_dir_all(&first_workspace).expect("first workspace");
    std::fs::create_dir_all(&second_workspace).expect("second workspace");
    let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
    let storage_paths = StoragePaths {
        data_dir: data_dir.clone(),
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
    };
    let sqlite = SqliteStore::open(&storage_paths).expect("store");
    sqlite.migrate().expect("migrate");
    let mut config = ResolvedConfig::default();
    config.multi_agent.enabled = true;
    config.multi_agent.max_concurrent_agents = 3;
    let first = crate::app::AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
        &first_workspace,
        StoreBundle::new(sqlite),
        config.clone(),
    )
    .await
    .expect("initial app");
    let process_runtime = first.process_runtime.clone();
    let runtime = process_runtime.agent_runtime();
    let original_service = Arc::downgrade(&first.run_service);
    let root_session = first
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("root before navigation".to_string()),
                cwd: first.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            first.workspace.clone(),
        )
        .await
        .expect("root session");
    let root_execution = runtime
        .begin_root_with_run_service(
            &root_session,
            captured_turn_config(config.clone()),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
            Arc::clone(&first.run_service),
        )
        .await
        .expect("root execution");
    bind_test_root_turn(&runtime, &root_execution).await;
    let tree = root_execution.context.tree.clone();
    assert_eq!(
        tree.control
            .status(&AgentPath::root())
            .expect("running root"),
        AgentStatus::Running
    );

    let child_session = first
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("pending child".to_string()),
                cwd: first.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            first.workspace.clone(),
        )
        .await
        .expect("child session");
    let child_path = AgentPath::root().join("pending").expect("child path");
    first
        .store
        .session_repo()
        .insert_session_spawn_edge(
            root_session.session.id,
            root_session.session.id,
            child_session.session.id,
            child_path.as_str(),
            "pending",
        )
        .await
        .expect("child spawn edge");
    let (_, child_execution) = tree
        .control
        .register_child(
            &AgentPath::root(),
            "pending",
            child_session.session.id,
            Some("waiting for worker admission".to_string()),
        )
        .expect("pending child registration");
    tree.metadata.lock().expect("metadata").insert(
        child_path.clone(),
        AgentNodeMetadata {
            task_name: "pending".to_string(),
            task_preview: "waiting for worker admission".to_string(),
            config: root_execution.context.config.clone(),
            workspace: root_execution.context.workspace.clone(),
            confirmation: root_execution.context.confirmation.clone(),
            run_service: root_execution.context.run_service.clone(),
            updated: false,
            activity_owner: None,
        },
    );
    assert_eq!(
        tree.control.status(&child_path).expect("pending child"),
        AgentStatus::PendingInit
    );
    let awaiting_session = first
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("awaiting owner".to_string()),
                cwd: first.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            first.workspace.clone(),
        )
        .await
        .expect("awaiting owner session");
    let awaiting_path = AgentPath::root().join("awaiting").expect("awaiting path");
    first
        .store
        .session_repo()
        .insert_session_spawn_edge(
            root_session.session.id,
            root_session.session.id,
            awaiting_session.session.id,
            awaiting_path.as_str(),
            "awaiting",
        )
        .await
        .expect("awaiting owner spawn edge");
    let deferred_turn_id = TurnId::new();
    tree.control
        .restore_inactive_child(
            &AgentPath::root(),
            "awaiting",
            awaiting_session.session.id,
            InactiveAgentStatus::AwaitingDescendants(deferred_turn_id),
            Some("waiting for retained descendants".to_string()),
        )
        .expect("awaiting owner projection");
    tree.metadata.lock().expect("metadata").insert(
        awaiting_path.clone(),
        AgentNodeMetadata {
            task_name: "awaiting".to_string(),
            task_preview: "waiting for retained descendants".to_string(),
            config: root_execution.context.config.clone(),
            workspace: root_execution.context.workspace.clone(),
            confirmation: root_execution.context.confirmation.clone(),
            run_service: root_execution.context.run_service.clone(),
            updated: false,
            activity_owner: None,
        },
    );
    assert_eq!(
        tree.control.status(&awaiting_path).expect("awaiting owner"),
        AgentStatus::AwaitingDescendants
    );

    let rebuilt = crate::app::AppBootstrap::
        rebuild_for_directory_as_workspace_root_with_process_runtime_and_config(
            &second_workspace,
            process_runtime.clone(),
            config.clone(),
        )
        .await
        .expect("rebuilt app");
    assert!(process_runtime.ptr_eq(&rebuilt.process_runtime));
    assert!(Arc::ptr_eq(
        &runtime,
        &rebuilt.process_runtime.agent_runtime()
    ));
    assert!(
        !original_service
            .upgrade()
            .is_some_and(|service| Arc::ptr_eq(&service, &rebuilt.run_service))
    );
    drop(first);
    assert!(
        original_service.upgrade().is_some(),
        "the admitted tree must retain its exact pre-navigation run service"
    );
    assert!(runtime.has_tree_for_session(root_session.session.id));
    assert_eq!(
        tree.control
            .status(&AgentPath::root())
            .expect("root after app swap"),
        AgentStatus::Running
    );
    assert_eq!(
        tree.control
            .status(&child_path)
            .expect("child after app swap"),
        AgentStatus::PendingInit
    );
    assert_eq!(
        tree.control
            .status(&awaiting_path)
            .expect("awaiting owner after app swap"),
        AgentStatus::AwaitingDescendants
    );
    let child_context = runtime
        .context_for_execution(&tree, &child_execution)
        .expect("child captured context");
    assert!(Arc::ptr_eq(
        child_context
            .run_service
            .as_ref()
            .expect("child run service"),
        &original_service.upgrade().expect("original run service")
    ));

    assert!(root_execution.run_control().seal_success());
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root while child is pending");
    assert_eq!(
        tree.control
            .status(&AgentPath::root())
            .expect("awaiting root"),
        AgentStatus::Completed(None)
    );
    assert!(runtime.has_tree_for_session(root_session.session.id));

    let new_root_session = rebuilt
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("root after navigation".to_string()),
                cwd: rebuilt.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            rebuilt.workspace.clone(),
        )
        .await
        .expect("new root session");
    let new_root = runtime
        .begin_root_with_run_service(
            &new_root_session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
            Arc::clone(&rebuilt.run_service),
        )
        .await
        .expect("new root execution");
    assert!(Arc::ptr_eq(
        new_root
            .context
            .run_service
            .as_ref()
            .expect("new root run service"),
        &rebuilt.run_service
    ));
    assert!(!Arc::ptr_eq(
        new_root
            .context
            .run_service
            .as_ref()
            .expect("new root run service"),
        &original_service.upgrade().expect("original run service")
    ));

    tree.control
        .complete_execution(child_execution, InactiveAgentStatus::Interrupted, None)
        .expect("settle old child");
    new_root
        .complete(AgentStatus::Completed(None))
        .expect("settle new root");
}

#[tokio::test]
async fn process_restart_rehydrates_unclaimed_initial_task_as_one_pending_execution() {
    let (original_runtime, root_session, config) =
        direct_runtime_fixture("durable-pending-task", 3).await;
    let child_session = original_runtime
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("research".to_string()),
                cwd: root_session.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            root_session.workspace.clone(),
        )
        .await
        .expect("child session");
    original_runtime
        .store
        .session_repo()
        .insert_session_spawn_edge(
            root_session.session.id,
            root_session.session.id,
            child_session.session.id,
            "/root/research",
            "research",
        )
        .await
        .expect("spawn edge");
    let initial = original_runtime
        .store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            child_session.session.id,
            InterAgentCommunication {
                author: "/root".to_string(),
                recipient: "/root/research".to_string(),
                content: "Message Type: NEW_TASK\nTask name: /root/research\nSender: /root\nPayload:\nInspect the fixture.".to_string(),
                trigger_turn: true,
            },
            false,
        )
        .expect("durable initial task");
    assert!(initial.schedule_turn);

    let store = original_runtime.store.clone();
    drop(original_runtime);
    let resumed_runtime = Arc::new(AgentRuntime::new(
        store.clone(),
        crate::session::SessionService::new(store),
    ));
    let root_execution = resumed_runtime
        .begin_root(
            &root_session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("rehydrated root");
    let child_path = AgentPath::try_from("/root/research").expect("child path");
    let restored = root_execution
        .context
        .tree
        .control
        .list_agents(Some(&child_path))
        .expect("restored child")
        .into_iter()
        .next()
        .expect("child snapshot");
    assert_eq!(restored.status, AgentStatus::PendingInit);
    assert_eq!(restored.pending_mail_count, 1);
    assert!(!restored.is_active);

    let scheduled = root_execution
        .context
        .tree
        .control
        .schedule_pending_triggered_executions()
        .expect("schedule recovered task");
    assert_eq!(scheduled.len(), 1);
    assert_eq!(scheduled[0].path(), &child_path);
    assert_eq!(
        root_execution
            .context
            .tree
            .control
            .schedule_pending_triggered_executions()
            .expect("do not duplicate recovered task")
            .len(),
        0
    );
    drop(scheduled);
    drop(root_execution);
}

#[tokio::test]
async fn process_restart_runs_nested_pending_target_without_resuming_parent() {
    let (original_runtime, root_session, config) =
        direct_runtime_fixture("durable-direct-target-restart", 2).await;
    let parent_session = original_runtime
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("parent".to_string()),
                cwd: root_session.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            root_session.workspace.clone(),
        )
        .await
        .expect("parent session");
    let leaf_session = original_runtime
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("leaf".to_string()),
                cwd: root_session.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            root_session.workspace.clone(),
        )
        .await
        .expect("leaf session");
    original_runtime
        .store
        .session_repo()
        .insert_session_spawn_edge(
            root_session.session.id,
            root_session.session.id,
            parent_session.session.id,
            "/root/parent",
            "parent",
        )
        .await
        .expect("parent edge");
    original_runtime
        .store
        .session_repo()
        .insert_session_spawn_edge(
            root_session.session.id,
            parent_session.session.id,
            leaf_session.session.id,
            "/root/parent/leaf",
            "leaf",
        )
        .await
        .expect("leaf edge");
    let leaf_trigger = original_runtime
        .store
        .session_repo()
        .append_inter_agent_communication_with_protocol_bundle(
            leaf_session.session.id,
            InterAgentCommunication {
                author: "/root/parent".to_string(),
                recipient: "/root/parent/leaf".to_string(),
                content: "Message Type: NEW_TASK\nTask name: /root/parent/leaf\nSender: /root/parent\nPayload:\nfinish after restart".to_string(),
                trigger_turn: true,
            },
            false,
        )
        .expect("nested trigger before crash");
    assert!(leaf_trigger.schedule_turn);

    let store = original_runtime.store.clone();
    drop(original_runtime);
    let resumed_runtime = Arc::new(AgentRuntime::new(
        store.clone(),
        crate::session::SessionService::new(store.clone()),
    ));
    let root_execution = resumed_runtime
        .begin_root(
            &root_session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("rehydrated root");
    let tree = root_execution.context.tree.clone();
    let parent_path = AgentPath::try_from("/root/parent").expect("parent path");
    let leaf_path = AgentPath::try_from("/root/parent/leaf").expect("leaf path");
    let leaf_before_run = tree
        .control
        .list_agents(Some(&leaf_path))
        .expect("restored leaf")
        .into_iter()
        .next()
        .expect("leaf");
    assert!(!leaf_before_run.is_active);
    assert_eq!(leaf_before_run.pending_mail_count, 1);

    let mut scheduled = tree
        .control
        .schedule_pending_triggered_executions()
        .expect("direct-target restart scheduling pass");
    assert_eq!(scheduled.len(), 1);
    let leaf_execution = scheduled.pop().expect("nested leaf execution");
    assert_eq!(leaf_execution.path(), &leaf_path);
    assert_eq!(
        leaf_execution.trigger_history_item_id(),
        Some(leaf_trigger.history_item_id)
    );
    assert!(
        !tree
            .control
            .list_agents(Some(&parent_path))
            .expect("parent snapshot")
            .into_iter()
            .next()
            .expect("parent")
            .is_active,
        "restart must not manufacture an ancestor turn"
    );
    drop(leaf_execution);
    drop(root_execution);
}

#[tokio::test]
async fn process_restart_rehydrates_durable_child_for_listing_followup_and_name_collision() {
    let (original_runtime, root_session, config) =
        direct_runtime_fixture("durable-rehydrate", 3).await;
    let child_session = original_runtime
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("research".to_string()),
                cwd: root_session.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            root_session.workspace.clone(),
        )
        .await
        .expect("child session");
    original_runtime
        .store
        .session_repo()
        .insert_session_spawn_edge(
            root_session.session.id,
            root_session.session.id,
            child_session.session.id,
            "/root/research",
            "research",
        )
        .await
        .expect("spawn edge");
    let grandchild_session = original_runtime
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("reviewer".to_string()),
                cwd: root_session.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            root_session.workspace.clone(),
        )
        .await
        .expect("grandchild session");
    original_runtime
        .store
        .session_repo()
        .insert_session_spawn_edge(
            root_session.session.id,
            child_session.session.id,
            grandchild_session.session.id,
            "/root/research/reviewer",
            "reviewer",
        )
        .await
        .expect("nested spawn edge");
    let child_turn_id = TurnId::new();
    let child_admission = original_runtime
        .store
        .session_repo()
        .admit_session_turn(child_session.session.id, child_turn_id)
        .await
        .expect("admit parent child turn")
        .expect("parent child turn admission");
    let grandchild_turn_id = TurnId::new();
    terminalize_test_session(
        &original_runtime,
        grandchild_session.session.id,
        grandchild_turn_id,
        &terminal_event(
            grandchild_session.session.id,
            TurnTerminalOutcome::Completed,
            None,
        ),
    )
    .await;
    let grandchild_handoff = original_runtime
        .store
        .session_repo()
        .agent_completion_handoff(grandchild_session.session.id, grandchild_turn_id)
        .expect("grandchild handoff query")
        .expect("grandchild handoff");
    assert_eq!(
        original_runtime
            .store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                child_session.session.id,
                child_admission.admission_id,
                &terminal_event(
                    child_session.session.id,
                    TurnTerminalOutcome::Completed,
                    None,
                ),
                child_turn_id,
                None,
                None,
            )
            .await
            .expect("child terminal with finish-drain"),
        crate::storage::session_repo::AdmittedTerminalCommit::Applied
    );
    assert_eq!(
        original_runtime
            .store
            .protocol_event_store()
            .history_items_by_id(
                child_session.session.id,
                &[grandchild_handoff.history_item_id],
            )
            .expect("finish-drained grandchild result")
            .len(),
        1
    );

    let store = original_runtime.store.clone();
    drop(original_runtime);
    let resumed_runtime = Arc::new(AgentRuntime::new(
        store.clone(),
        crate::session::SessionService::new(store.clone()),
    ));
    let execution = resumed_runtime
        .begin_root(
            &root_session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("rehydrated root");
    bind_test_root_turn(&resumed_runtime, &execution).await;
    let tree = execution.context.tree.clone();

    let restored = execution
        .context
        .list_agents(None)
        .expect("list restored agents");
    assert_eq!(restored.len(), 3);
    assert_eq!(restored[1].path.as_str(), "/root/research");
    assert_eq!(restored[1].session_id, child_session.session.id);
    assert!(matches!(restored[1].status, AgentStatus::Completed(None)));
    assert!(!restored[1].is_active);
    assert_eq!(restored[2].path.as_str(), "/root/research/reviewer");
    assert_eq!(restored[2].session_id, grandchild_session.session.id);
    assert_eq!(
        restored[2].parent,
        Some(
            AgentPath::try_from("/root/research").expect("canonical rehydrated grandchild parent")
        )
    );
    assert!(matches!(restored[2].status, AgentStatus::Completed(None)));

    let duplicate = execution
        .context
        .spawn_agent(
            "research",
            "duplicate must not create another durable edge".to_string(),
            AgentForkTurns::All,
            "duplicate".to_string(),
        )
        .await
        .expect_err("restored path collision");
    assert!(duplicate.contains("use followup_task"));
    let child_path = AgentPath::try_from("/root/research").expect("restored child path");
    let child_lease = tree
        .control
        .try_acquire_execution(&child_path)
        .expect("reserve restored child execution before durable follow-up");
    execution
        .context
        .send_message(
            "/root/research",
            "review the new request".to_string(),
            true,
            "followup".to_string(),
        )
        .await
        .expect("follow-up resolves restored child");
    let trigger_history_item_id = store
        .session_repo()
        .pending_agent_trigger_history_item_id(child_session.session.id)
        .expect("restored child pending trigger")
        .expect("follow-up trigger");
    let child_lease = child_lease
        .try_bind_trigger_history_item_id(trigger_history_item_id)
        .unwrap_or_else(|_| panic!("restored child lease must bind the durable follow-up"));
    let child_turn_id = TurnId::new();
    let child_admission = store
        .session_repo()
        .admit_agent_triggered_turn(
            child_session.session.id,
            child_turn_id,
            trigger_history_item_id,
        )
        .await
        .expect("restored child follow-up admission")
        .expect("restored child admitted");
    let child_context = AgentRunContext {
        runtime: resumed_runtime.clone(),
        tree: tree.clone(),
        path: child_path.clone(),
        session_id: child_session.session.id,
        wake_cause: child_lease.wake_cause(),
        execution: child_lease.scope(),
        turn_owner: Arc::new(OnceLock::new()),
        config: execution.context.config.clone(),
        workspace: child_session.workspace.clone(),
        confirmation: execution.context.confirmation.clone(),
        run_service: execution.context.run_service.clone(),
    };
    child_context
        .bind_durable_turn_owner(child_admission.admission_id, child_turn_id)
        .expect("bind restored child follow-up");
    assert_eq!(
        child_context
            .commit_pending_mailbox_delivery(AgentMailboxDeliverySelector::AllPending, 128)
            .expect("safe restored follow-up delivery")
            .history_item_ids,
        vec![trigger_history_item_id]
    );
    child_context
        .mark_durable_turn_admitted()
        .expect("publish restored child admission");
    let child_history = store
        .protocol_event_store()
        .list_history_items_for_session(child_session.session.id)
        .expect("follow-up history");
    assert!(child_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::InterAgentCommunication { communication }
            if communication.author == "/root"
                && communication.recipient == "/root/research"
                && communication.trigger_turn
                && communication.content
                    == "Message Type: NEW_TASK\nTask name: /root/research\nSender: /root\nPayload:\nreview the new request"
    )));
    child_context
        .cancel_for_durable_terminal()
        .expect("close restored child turn");
    terminalize_admitted_test_session(
        &resumed_runtime,
        child_session.session.id,
        child_admission.admission_id,
        child_turn_id,
        &terminal_event(
            child_session.session.id,
            TurnTerminalOutcome::Interrupted {
                cause: TurnInterruptionCause::AgentInterrupted,
            },
            None,
        ),
    )
    .await;
    tree.control
        .complete_execution(child_lease, InactiveAgentStatus::Interrupted, None)
        .expect("complete restored child");
    assert_eq!(
        store
            .session_repo()
            .list_session_spawn_edges(root_session.session.id)
            .await
            .expect("spawn edges")
            .len(),
        2
    );
    resumed_runtime.complete_root(
        execution,
        &Ok(terminal_summary(
            root_session.session.id,
            TurnTerminalOutcome::Completed,
        )),
        None,
    );
}

#[test]
fn pre_admission_terminal_fence_purges_the_claimed_trigger_before_completion() {
    let (control, _root_execution) = AgentControl::new(SessionId::new(), 2).expect("agent tree");
    let root = AgentPath::root();
    let (child, child_execution) = control
        .register_child(&root, "research", SessionId::new(), None)
        .expect("child registration");
    assert!(
        control
            .complete_execution(child_execution, InactiveAgentStatus::Completed(None), None,)
            .expect("complete initial child turn")
            .is_empty()
    );
    let delivery = control
        .commit_and_enqueue_mail(&root, &child.path, true, || {
            Ok(AgentMailCommit {
                history_item_id: HistoryItemId::new(),
                schedule_turn: true,
                owner_resume_request_id: None,
            })
        })
        .expect("enqueue follow-up trigger");
    let AgentMailDeliveryOutcome::Enqueued { mut scheduled, .. } = delivery;
    assert_eq!(scheduled.len(), 1);

    let lease = scheduled.pop().expect("scheduled child lease");
    assert_eq!(
        control
            .commit_pending_trigger_terminal(&lease, None, || {
                Ok(PendingTriggerTerminalCommit::Applied(()))
            })
            .expect("durable terminal fence"),
        PendingTriggerTerminalCommit::Applied(())
    );
    let additional = control
        .complete_execution(
            lease,
            InactiveAgentStatus::Errored("child context could not be constructed".to_string()),
            None,
        )
        .expect("complete failed child");

    assert!(additional.is_empty());
    assert!(
        !control
            .mailbox_has_trigger_turn(&child.path)
            .expect("trigger state")
    );
    assert_eq!(
        control.status(&child.path).expect("failed child status"),
        AgentStatus::Errored("child context could not be constructed".to_string())
    );
}

#[tokio::test]
async fn durable_activity_projection_restores_three_completed_paths_tasks_and_results() {
    let (original_runtime, root_session, config) =
        direct_runtime_fixture("durable-desktop-projection", 4).await;
    let protocol_store = original_runtime.store.protocol_event_store();
    let mut child_sessions = Vec::new();

    for task_name in ["research", "review", "tests"] {
        let child = original_runtime
            .session_service
            .start_or_resume(
                SessionStartRequest {
                    selector: SessionSelector::New,
                    title: Some(task_name.to_string()),
                    cwd: root_session.workspace.cwd.clone(),
                    model: config.model.model.clone(),
                    base_url: config.model.base_url.clone(),
                    access_mode: config.permissions.access_mode,
                },
                root_session.workspace.clone(),
            )
            .await
            .expect("child session");
        let agent_path = format!("/root/{task_name}");
        original_runtime
            .store
            .session_repo()
            .insert_session_spawn_edge(
                root_session.session.id,
                root_session.session.id,
                child.session.id,
                &agent_path,
                task_name,
            )
            .await
            .expect("spawn edge");

        let turn_id = TurnId::new();
        let admission_id = original_runtime
            .store
            .session_repo()
            .admit_session_turn(child.session.id, turn_id)
            .await
            .expect("admit durable child fixture")
            .expect("durable child fixture admitted")
            .admission_id;
        let task = format!("durable task {task_name}");
        let result = format!("durable result {task_name}");
        let response_id = ModelResponseId::new();
        protocol_store
            .seed_history_item_for_test(&HistoryItem {
                id: HistoryItemId::new(),
                session_id: child.session.id,
                scope: HistoryScope::Turn { turn_id },
                sequence_no: 0,
                created_at_ms: SystemClock::now_ms(),
                payload: HistoryItemPayload::UserTurn {
                    content: vec![ContentPart::Text { text: task.clone() }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
            })
            .expect("durable child task");
        protocol_store
            .seed_history_item_for_test(&HistoryItem {
                id: HistoryItemId::new(),
                session_id: child.session.id,
                scope: HistoryScope::Turn { turn_id },
                sequence_no: 1,
                created_at_ms: SystemClock::now_ms(),
                payload: HistoryItemPayload::AssistantMessage {
                    response_id,
                    content: vec![ContentPart::Text {
                        text: result.clone(),
                    }],
                },
            })
            .expect("durable child result");
        terminalize_admitted_test_session(
            &original_runtime,
            child.session.id,
            admission_id,
            turn_id,
            &terminal_event(
                child.session.id,
                TurnTerminalOutcome::Completed,
                Some(response_id),
            ),
        )
        .await;
        child_sessions.push((
            child.session,
            agent_path,
            task_name.to_string(),
            task,
            result,
        ));
    }

    let store = original_runtime.store.clone();
    drop(original_runtime);
    let restarted_runtime =
        AgentRuntime::new(store.clone(), crate::session::SessionService::new(store));
    assert!(
        restarted_runtime
            .activity_records(root_session.session.id)
            .is_empty(),
        "process-local activity is intentionally empty before a resumed run"
    );

    let records = restarted_runtime
        .durable_activity_records(root_session.session.id)
        .await
        .expect("durable activity projection");
    assert_eq!(records.len(), 3);
    assert_eq!(
        records
            .iter()
            .map(|record| record.started_order)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    for (session, agent_path, task_name, task, result) in child_sessions {
        let record = records
            .iter()
            .find(|record| record.session_id == session.id)
            .expect("projected child row");
        assert_eq!(record.agent_path, agent_path);
        assert_eq!(record.task_name, task_name);
        assert_eq!(record.task_preview, task);
        assert!(matches!(record.status, AgentStatus::Completed(Some(_))));
        assert!(!record.is_current_turn);
        assert_eq!(record.result_preview, result);
        assert!(record.current_activity.is_empty());

        let mut running = session;
        running.status = SessionStatus::Running;
        assert!(matches!(
            durable_projection_status(
                running.id,
                running.status,
                Some("still running".to_string()),
                None,
            ),
            AgentStatus::Running
        ));
    }
}

#[tokio::test]
async fn durable_cancelled_projection_uses_the_canonical_typed_cause() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("durable-cancelled-cause", 3).await;

    for (task_name, cause) in [("typed_cancel", TurnInterruptionCause::UserStop)] {
        let child = runtime
            .session_service
            .start_or_resume(
                SessionStartRequest {
                    selector: SessionSelector::New,
                    title: Some(task_name.to_string()),
                    cwd: root_session.workspace.cwd.clone(),
                    model: config.model.model.clone(),
                    base_url: config.model.base_url.clone(),
                    access_mode: config.permissions.access_mode,
                },
                root_session.workspace.clone(),
            )
            .await
            .expect("child session");
        runtime
            .store
            .session_repo()
            .insert_session_spawn_edge(
                root_session.session.id,
                root_session.session.id,
                child.session.id,
                &format!("/root/{task_name}"),
                task_name,
            )
            .await
            .expect("spawn edge");
        let turn_id = TurnId::new();
        terminalize_test_session(
            &runtime,
            child.session.id,
            turn_id,
            &terminal_event(
                child.session.id,
                TurnTerminalOutcome::Interrupted { cause },
                None,
            ),
        )
        .await;
    }

    let records = runtime
        .durable_activity_records(root_session.session.id)
        .await
        .expect("durable cancelled projection");
    let typed = records
        .iter()
        .find(|record| record.task_name == "typed_cancel")
        .expect("typed cancelled child");
    assert_eq!(typed.status, AgentStatus::Interrupted);
}

#[tokio::test]
async fn durable_running_projection_carries_the_exact_active_turn_from_its_status_snapshot() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("durable-running-exact-turn", 2).await;
    let child = runtime
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("running-child".to_string()),
                cwd: root_session.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            root_session.workspace.clone(),
        )
        .await
        .expect("child session");
    runtime
        .store
        .session_repo()
        .insert_session_spawn_edge(
            root_session.session.id,
            root_session.session.id,
            child.session.id,
            "/root/running_child",
            "running_child",
        )
        .await
        .expect("spawn edge");
    let turn_id = TurnId::new();
    runtime
        .store
        .session_repo()
        .admit_session_turn(child.session.id, turn_id)
        .await
        .expect("child admission")
        .expect("running child");

    let record = runtime
        .durable_activity_records(root_session.session.id)
        .await
        .expect("durable activity")
        .into_iter()
        .find(|record| record.session_id == child.session.id)
        .expect("running child projection");
    assert_eq!(record.status, AgentStatus::Running);
    assert_eq!(record.active_turn_id, Some(turn_id));
    assert!(record.can_interrupt);
}

#[tokio::test]
async fn nested_unbound_launch_retains_one_atomically_settled_failed_grandchild() {
    let (runtime, session, config) = direct_runtime_fixture("spawn-depth", 3).await;
    let execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config.clone()),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    let root_owner = bind_test_root_turn(&runtime, &execution).await;
    let tree = execution.context.tree.clone();
    let child_path = crate::runtime::AgentPath::root()
        .join("child")
        .expect("child path");
    let child_session = runtime
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("spawn-depth-existing-child".to_string()),
                cwd: session.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            session.workspace.clone(),
        )
        .await
        .expect("existing child session");
    let child_session_id = child_session.session.id;
    runtime
        .store
        .session_repo()
        .insert_session_spawn_edge(
            session.session.id,
            session.session.id,
            child_session_id,
            "/root/child",
            "child",
        )
        .await
        .expect("durable child edge");
    let (_, child_lease) = tree
        .control
        .register_child(
            &crate::runtime::AgentPath::root(),
            "child",
            child_session_id,
            None,
        )
        .expect("child registration");
    let child_context = AgentRunContext {
        runtime: runtime.clone(),
        tree: tree.clone(),
        path: child_path,
        session_id: child_session_id,
        wake_cause: None,
        execution: child_lease.scope(),
        turn_owner: Arc::new(OnceLock::new()),
        config: captured_turn_config(config),
        workspace: session.workspace.clone(),
        confirmation: execution.context.confirmation.clone(),
        run_service: execution.context.run_service.clone(),
    };
    let child_turn_id = TurnId::new();
    let child_admission = runtime
        .store
        .session_repo()
        .admit_session_turn(child_session_id, child_turn_id)
        .await
        .expect("child admission")
        .expect("child admitted");
    child_context
        .bind_durable_turn_owner(child_admission.admission_id, child_turn_id)
        .expect("bind child durable turn owner");
    let agents_before = tree.control.snapshot().expect("tree before").agents.len();

    let launch_error = child_context
        .spawn_agent(
            "grandchild",
            "reach the launch boundary".to_string(),
            AgentForkTurns::All,
            "depth_check".to_string(),
        )
        .await
        .expect_err("missing captured run service must reject the nested worker launch");

    assert_eq!(
        tree.control.snapshot().expect("tree after").agents.len(),
        agents_before + 1
    );
    let edges = runtime
        .store
        .session_repo()
        .list_session_spawn_edges(session.session.id)
        .await
        .expect("spawn edges");
    assert_eq!(edges.len(), 2);
    assert_eq!(edges[0].agent_path, "/root/child");
    assert_eq!(edges[1].agent_path, "/root/child/grandchild");
    let grandchild_session_id = edges[1].child_session_id;
    assert_eq!(
        runtime
            .store
            .session_repo()
            .get_session(grandchild_session_id)
            .await
            .expect("failed grandchild session")
            .status,
        SessionStatus::Failed
    );
    assert!(
        runtime
            .store
            .session_repo()
            .pending_agent_trigger_history_item_id(grandchild_session_id)
            .expect("settled grandchild trigger")
            .is_none()
    );
    let grandchild_terminals = runtime
        .store
        .protocol_event_store()
        .list_runtime_events_for_session(grandchild_session_id)
        .expect("failed grandchild runtime events")
        .into_iter()
        .filter(|event| event.is_terminal())
        .collect::<Vec<_>>();
    assert_eq!(grandchild_terminals.len(), 1);
    assert!(matches!(
        grandchild_terminals[0].terminal_outcome(),
        Some(TurnTerminalOutcome::Failed { error })
            if error == &launch_error
    ));
    let receipt = runtime
        .store
        .session_repo()
        .agent_completion_handoff(grandchild_session_id, grandchild_terminals[0].turn_id)
        .expect("grandchild handoff query")
        .expect("grandchild handoff");
    assert_eq!(receipt.parent_session_id, child_session_id);
    assert_eq!(receipt.parent_agent_path, child_context.path);
    assert_eq!(
        runtime
            .store
            .session_repo()
            .schedulable_owner_resume_request_id(child_session_id)
            .expect("current child continuation"),
        None
    );
    assert_eq!(
        tree.control
            .drain_mailbox(&child_context.path)
            .expect("child completion mailbox")
            .into_iter()
            .map(|notice| notice.history_item_id)
            .collect::<Vec<_>>(),
        vec![receipt.history_item_id]
    );

    terminalize_admitted_test_session(
        &runtime,
        child_session_id,
        child_admission.admission_id,
        child_turn_id,
        &terminal_event(
            child_session_id,
            TurnTerminalOutcome::Interrupted {
                cause: TurnInterruptionCause::AgentInterrupted,
            },
            None,
        ),
    )
    .await;
    child_context
        .cancel_for_durable_terminal()
        .expect("durable child terminal");
    assert!(child_lease.cancel_token().is_cancelled());
    assert!(!tree.control.tree_is_cancelled());
    tree.control
        .complete_execution(child_lease, InactiveAgentStatus::Interrupted, None)
        .expect("complete child");
    terminalize_admitted_test_session(
        &runtime,
        root_owner.session_id,
        root_owner.admission_id,
        root_owner.turn_id,
        &terminal_event(root_owner.session_id, TurnTerminalOutcome::Completed, None),
    )
    .await;
    assert!(execution.run_control().seal_success());
    runtime.complete_root(
        execution,
        &Ok(terminal_summary(
            session.session.id,
            TurnTerminalOutcome::Completed,
        )),
        None,
    );
}

#[tokio::test]
async fn unbound_launch_settles_atomic_spawn_once_and_restart_does_not_replay_trigger() {
    let (runtime, session, config) = direct_runtime_fixture("spawn-launch-settlement", 2).await;
    let storage_paths = runtime.store.paths().clone();
    let execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config.clone()),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    let root_owner = bind_test_root_turn(&runtime, &execution).await;

    let launch_error = execution
        .context
        .spawn_agent(
            "worker",
            "bounded task".to_string(),
            AgentForkTurns::None,
            "spawn_launch_failure".to_string(),
        )
        .await
        .expect_err("missing captured run service must reject the worker launch");

    let edges = runtime
        .store
        .session_repo()
        .list_session_spawn_edges(session.session.id)
        .await
        .expect("retained spawn edge");
    assert_eq!(edges.len(), 1);
    let edge = &edges[0];
    assert_eq!(edge.agent_path, "/root/worker");
    let child_session_id = edge.child_session_id;
    assert_eq!(
        runtime
            .store
            .session_repo()
            .get_session(child_session_id)
            .await
            .expect("failed child session")
            .status,
        SessionStatus::Failed
    );
    assert!(
        runtime
            .store
            .session_repo()
            .pending_agent_trigger_history_item_id(child_session_id)
            .expect("settled child trigger")
            .is_none()
    );
    let terminal_events = runtime
        .store
        .protocol_event_store()
        .list_runtime_events_for_session(child_session_id)
        .expect("failed child runtime events")
        .into_iter()
        .filter(|event| event.is_terminal())
        .collect::<Vec<_>>();
    assert_eq!(terminal_events.len(), 1);
    assert!(matches!(
        terminal_events[0].terminal_outcome(),
        Some(TurnTerminalOutcome::Failed { error })
            if error == &launch_error
    ));
    let child_turn_id = terminal_events[0].turn_id;
    let receipt = runtime
        .store
        .session_repo()
        .agent_completion_handoff(child_session_id, child_turn_id)
        .expect("failed child handoff")
        .expect("failed child receipt");
    assert_eq!(receipt.parent_session_id, session.session.id);
    assert_eq!(receipt.parent_agent_path, AgentPath::root());
    assert_eq!(
        runtime
            .store
            .session_repo()
            .schedulable_owner_resume_request_id(session.session.id)
            .expect("root continuation query"),
        None
    );
    let root_history_before_delivery = runtime
        .store
        .protocol_event_store()
        .list_history_items_for_session(session.session.id)
        .expect("root completion history before safe delivery");
    assert!(!root_history_before_delivery.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::SubAgentActivity {
            activity_id,
            activity_kind: SubAgentActivityKind::Started,
            ..
        } if activity_id == "spawn_launch_failure"
    )));
    assert!(
        root_history_before_delivery
            .iter()
            .all(|item| item.id != receipt.history_item_id)
    );
    let delivered = execution
        .context
        .commit_pending_mailbox_delivery(AgentMailboxDeliverySelector::AllPending, 128)
        .expect("safe failed-child FINAL delivery");
    assert_eq!(delivered.history_item_ids, vec![receipt.history_item_id]);
    let root_history = runtime
        .store
        .protocol_event_store()
        .list_history_items_for_session(session.session.id)
        .expect("root completion history after safe delivery");
    let parent_finals = root_history
        .iter()
        .filter(|item| {
            matches!(
                &item.payload,
                HistoryItemPayload::InterAgentCommunication { communication }
                    if communication.author == "/root/worker"
                        && communication.recipient == "/root"
                        && communication.content.contains("Message Type: FINAL_ANSWER")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(parent_finals.len(), 1);
    assert_eq!(parent_finals[0].id, receipt.history_item_id);
    assert_eq!(
        execution
            .context
            .tree
            .control
            .drain_mailbox(&AgentPath::root())
            .expect("failed child mailbox")
            .into_iter()
            .map(|notice| notice.history_item_id)
            .collect::<Vec<_>>(),
        Vec::<HistoryItemId>::new()
    );

    let root_terminal = DurableTurnTerminal {
        outcome: TurnTerminalOutcome::Completed,
        final_response_id: None,
        tool_call_count: 0,
        failed_tool_count: 0,
        change_count: 0,
        metrics: Default::default(),
    };
    terminalize_admitted_test_session(
        &runtime,
        root_owner.session_id,
        root_owner.admission_id,
        root_owner.turn_id,
        &RunEvent::TurnTerminal {
            session_id: root_owner.session_id,
            terminal: Box::new(root_terminal.clone()),
        },
    )
    .await;
    assert!(execution.run_control().seal_success());
    runtime.complete_root(
        execution,
        &Ok(RunSummary::from_terminal(
            root_owner.session_id,
            root_owner.turn_id,
            root_terminal,
        )),
        None,
    );
    drop(runtime);

    let sqlite = SqliteStore::open(&storage_paths).expect("reopened store");
    sqlite.migrate().expect("reopened migrations");
    let reopened_store = StoreBundle::new(sqlite);
    assert!(
        reopened_store
            .session_repo()
            .pending_agent_trigger_history_item_id(child_session_id)
            .expect("restart trigger projection")
            .is_none()
    );
    let reopened_terminals = reopened_store
        .protocol_event_store()
        .list_runtime_events_for_session(child_session_id)
        .expect("restart child runtime events")
        .into_iter()
        .filter(|event| event.is_terminal())
        .collect::<Vec<_>>();
    assert_eq!(reopened_terminals.len(), 1);
    assert_eq!(reopened_terminals[0].turn_id, child_turn_id);
    assert_eq!(
        reopened_store
            .session_repo()
            .agent_completion_handoff(child_session_id, child_turn_id)
            .expect("restart failed-child receipt")
            .expect("retained failed-child receipt")
            .history_item_id,
        receipt.history_item_id
    );
    let reopened_parent_finals = reopened_store
        .protocol_event_store()
        .list_history_items_for_session(session.session.id)
        .expect("restart parent history")
        .into_iter()
        .filter(|item| {
            matches!(
                &item.payload,
                HistoryItemPayload::InterAgentCommunication { communication }
                    if communication.author == "/root/worker"
                        && communication.recipient == "/root"
                        && communication.content.contains("Message Type: FINAL_ANSWER")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(reopened_parent_finals.len(), 1);
    assert_eq!(reopened_parent_finals[0].id, receipt.history_item_id);
    let reopened_runtime = Arc::new(AgentRuntime::new(
        reopened_store.clone(),
        crate::session::SessionService::new(reopened_store),
    ));
    let reopened_root = reopened_runtime
        .begin_root(
            &session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("rehydrated root");
    let restored_worker = reopened_root
        .context
        .list_agents(None)
        .expect("rehydrated failed child")
        .into_iter()
        .find(|agent| agent.path.as_str() == "/root/worker")
        .expect("retained failed child");
    assert!(!restored_worker.is_active);
    assert!(matches!(restored_worker.status, AgentStatus::Errored(_)));
}

async fn commit_atomic_child_trigger_without_launch(
    runtime: &Arc<AgentRuntime>,
    caller: &AgentRunContext,
    task_name: &str,
) -> (AgentPath, SessionId, HistoryItemId, AgentExecutionLease) {
    let activity_owner = caller.durable_turn_owner().expect("root turn owner");
    let child_path = caller.path.join(task_name).expect("child path");
    let child_config = caller.effective_config();
    let child_session_id = SessionId::new();
    let child_draft = NewSession {
        project_id: caller.workspace.project_id,
        title: task_name.to_string(),
        cwd: caller.workspace.cwd.clone(),
        model: child_config.model.model,
        base_url: child_config.model.base_url,
        access_mode: child_config.permissions.access_mode,
    };
    let initial_task = InterAgentCommunication {
        author: caller.path.to_string(),
        recipient: child_path.to_string(),
        content: render_inter_agent_message(
            InterAgentMessageType::NewTask,
            child_path.as_str(),
            caller.path.as_str(),
            "cancel before admission",
        ),
        trigger_turn: true,
    };
    let (stored_spawn, _snapshot, lease) = caller
        .tree
        .control
        .commit_spawn(
            &caller.execution,
            &caller.path,
            task_name,
            child_session_id,
            Some("Starting assigned task".to_string()),
            || {
                runtime
                    .store
                    .session_repo()
                    .create_agent_spawn_with_initial_task_for_caller_turn(
                        caller.tree.root_session_id,
                        activity_owner.session_id,
                        child_session_id,
                        child_draft,
                        child_path.as_str(),
                        task_name,
                        activity_owner.admission_id,
                        activity_owner.turn_id,
                        SpawnContextFork::None,
                        initial_task,
                    )
                    .map(|stored| {
                        let spawn_order = stored.edge.spawn_order;
                        (stored, spawn_order)
                    })
                    .map_err(|error| error.to_string())
            },
        )
        .expect("atomic child spawn");
    let trigger_history_item_id = stored_spawn.initial_task_history_item_id;
    let lease = match lease.try_bind_trigger_history_item_id(trigger_history_item_id) {
        Ok(lease) => lease,
        Err(_) => panic!("atomic spawn lease must bind its initial trigger"),
    };
    (child_path, child_session_id, trigger_history_item_id, lease)
}

#[tokio::test]
async fn hard_abort_drops_the_whole_exact_worker_before_durable_interruption() {
    struct DropOrder(Arc<Mutex<Vec<&'static str>>>);
    impl Drop for DropOrder {
        fn drop(&mut self) {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push("worker_dropped");
        }
    }

    let (runtime, session, config) = direct_runtime_fixture("hard-abort-barrier", 2).await;
    let execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    let _root_owner = bind_test_root_turn(&runtime, &execution).await;
    let tree = execution.context.tree.clone();
    let (child_path, child_session_id, history_item_id, child_lease) =
        commit_atomic_child_trigger_without_launch(&runtime, &execution.context, "worker").await;
    let child_turn_id = TurnId::new();
    runtime
        .store
        .session_repo()
        .admit_agent_triggered_turn(child_session_id, child_turn_id, history_item_id)
        .await
        .expect("child admission")
        .expect("explicit wake admitted");

    let wake_cause = child_lease
        .wake_cause()
        .expect("child execution retains exact wake identity");
    let lease = Arc::new(Mutex::new(Some(child_lease)));
    let terminal_owner = AgentWorkerTerminalOwner {
        session_id: child_session_id,
        wake_cause,
        lease: Arc::clone(&lease),
    };
    let order = Arc::new(Mutex::new(Vec::new()));
    let worker_order = Arc::clone(&order);
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let worker = runtime
        .worker_runtime
        .spawn(41, move || async move {
            let _drop_order = DropOrder(worker_order);
            started_tx.send(()).expect("worker started");
            std::future::pending::<()>().await;
        })
        .expect("owned worker");
    runtime
        .install_worker(
            session.session.id,
            child_path.clone(),
            worker,
            terminal_owner,
        )
        .unwrap_or_else(|(error, _worker)| panic!("install exact worker: {error}"));
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("worker start signal");

    tree.control
        .cancel_agent(&child_path)
        .expect("cancel exact child");
    let captured = runtime.capture_cancelled_workers(&tree);
    assert_eq!(captured.len(), 1);
    runtime.abort_cancelled_workers(&tree, captured).await;

    order
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push("durable_terminal_observed");
    assert_eq!(
        *order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        vec!["worker_dropped", "durable_terminal_observed"]
    );
    assert!(matches!(
        runtime
            .store
            .session_repo()
            .durable_terminal_for_turn(child_session_id, child_turn_id)
            .await
            .expect("durable exact terminal")
            .expect("hard-abort terminal")
            .outcome,
        TurnTerminalOutcome::Interrupted {
            cause: TurnInterruptionCause::AgentInterrupted
        }
    ));
    assert!(
        runtime
            .workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tasks
            .get(&(session.session.id, child_path))
            .is_none(),
        "the exact worker generation must leave the registry before settlement returns"
    );
}

#[tokio::test]
async fn cancellation_before_admission_settles_interrupted_without_handoff_or_restart_replay() {
    let (runtime, session, config) =
        direct_runtime_fixture("spawn-pre-admission-cancellation", 2).await;
    let storage_paths = runtime.store.paths().clone();
    let execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    let root_owner = bind_test_root_turn(&runtime, &execution).await;
    let tree = execution.context.tree.clone();
    let (child_path, child_session_id, _trigger_history_item_id, child_lease) =
        commit_atomic_child_trigger_without_launch(&runtime, &execution.context, "cancelled").await;
    tree.control
        .cancel_agent(&child_path)
        .expect("cancel pre-admission child");
    assert_eq!(
        child_lease.run_control().cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::AgentInterrupted
        ))
    );
    assert!(
        runtime
            .settle_pre_admission_execution(&tree, child_lease, None)
            .expect("pre-admission interruption settlement")
            .is_empty()
    );
    assert_eq!(
        runtime
            .store
            .session_repo()
            .get_session(child_session_id)
            .await
            .expect("cancelled child session")
            .status,
        SessionStatus::Cancelled
    );
    assert!(
        runtime
            .store
            .session_repo()
            .pending_agent_trigger_history_item_id(child_session_id)
            .expect("cancelled child trigger")
            .is_none()
    );
    let terminal_events = runtime
        .store
        .protocol_event_store()
        .list_runtime_events_for_session(child_session_id)
        .expect("cancelled child events")
        .into_iter()
        .filter(|event| event.is_terminal())
        .collect::<Vec<_>>();
    assert_eq!(terminal_events.len(), 1);
    assert!(matches!(
        terminal_events[0].terminal_outcome(),
        Some(TurnTerminalOutcome::Interrupted {
            cause: TurnInterruptionCause::AgentInterrupted
        })
    ));
    let child_turn_id = terminal_events[0].turn_id;
    assert!(
        runtime
            .store
            .session_repo()
            .agent_completion_handoff(child_session_id, child_turn_id)
            .expect("cancelled child receipt")
            .is_none()
    );
    assert!(
        runtime
            .store
            .protocol_event_store()
            .list_history_items_for_session(session.session.id)
            .expect("parent history after child cancellation")
            .iter()
            .all(|item| !matches!(
                &item.payload,
                HistoryItemPayload::InterAgentCommunication { communication }
                    if communication.author == child_path.as_str()
                        && communication.content.contains("Message Type: FINAL_ANSWER")
            ))
    );

    let root_terminal = DurableTurnTerminal {
        outcome: TurnTerminalOutcome::Completed,
        final_response_id: None,
        tool_call_count: 0,
        failed_tool_count: 0,
        change_count: 0,
        metrics: Default::default(),
    };
    terminalize_admitted_test_session(
        &runtime,
        root_owner.session_id,
        root_owner.admission_id,
        root_owner.turn_id,
        &RunEvent::TurnTerminal {
            session_id: root_owner.session_id,
            terminal: Box::new(root_terminal.clone()),
        },
    )
    .await;
    assert!(execution.run_control().seal_success());
    runtime.complete_root(
        execution,
        &Ok(RunSummary::from_terminal(
            root_owner.session_id,
            root_owner.turn_id,
            root_terminal,
        )),
        None,
    );
    drop(runtime);
    let sqlite = SqliteStore::open(&storage_paths).expect("reopened cancelled-child store");
    sqlite
        .migrate()
        .expect("reopened cancelled-child migrations");
    let reopened_store = StoreBundle::new(sqlite);
    assert!(
        reopened_store
            .session_repo()
            .pending_agent_trigger_history_item_id(child_session_id)
            .expect("restart cancelled trigger projection")
            .is_none()
    );
    let reopened_terminals = reopened_store
        .protocol_event_store()
        .list_runtime_events_for_session(child_session_id)
        .expect("restart cancelled child events")
        .into_iter()
        .filter(|event| event.is_terminal())
        .collect::<Vec<_>>();
    assert_eq!(reopened_terminals.len(), 1);
    assert_eq!(reopened_terminals[0].turn_id, child_turn_id);
    assert!(
        reopened_store
            .session_repo()
            .agent_completion_handoff(child_session_id, child_turn_id)
            .expect("restart cancelled-child receipt")
            .is_none()
    );
}

#[tokio::test]
async fn pending_init_child_accepts_mail_before_durable_admission() {
    let (runtime, root_session, config) = direct_runtime_fixture("pending-init-mail", 2).await;
    let root_execution = runtime
        .begin_root(
            &root_session,
            captured_turn_config(config.clone()),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    bind_test_root_turn(&runtime, &root_execution).await;
    let tree = root_execution.context.tree.clone();
    let child = runtime
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("pending-init-child".to_string()),
                cwd: root_session.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            root_session.workspace.clone(),
        )
        .await
        .expect("child session");
    let child_path = AgentPath::root().join("pending").expect("child path");
    runtime
        .store
        .session_repo()
        .insert_session_spawn_edge(
            root_session.session.id,
            root_session.session.id,
            child.session.id,
            child_path.as_str(),
            "pending",
        )
        .await
        .expect("pending child spawn edge");
    let (_, child_lease) = tree
        .control
        .register_child(
            &AgentPath::root(),
            "pending",
            child.session.id,
            Some("Starting assigned task".to_string()),
        )
        .expect("child registration");
    assert!(matches!(
        tree.control
            .list_agents(Some(&child_path))
            .expect("pending child")[0]
            .status,
        AgentStatus::PendingInit
    ));
    assert!(
        runtime
            .store
            .session_repo()
            .fresh_running_turn_for_session(child.session.id)
            .await
            .expect("pre-admission child state")
            .is_none()
    );

    let resolved_by_id = root_execution
        .context
        .send_message(
            &child.session.id.to_string(),
            "ordinary evidence".to_string(),
            false,
            "pending_message".to_string(),
        )
        .await
        .expect("pending child message");
    assert_eq!(resolved_by_id, child_path);
    root_execution
        .context
        .send_message(
            "pending",
            "next bounded task".to_string(),
            true,
            "pending_followup".to_string(),
        )
        .await
        .expect("pending child follow-up");

    let pending = tree
        .control
        .list_agents(Some(&child_path))
        .expect("pending child after mail")
        .into_iter()
        .next()
        .expect("pending child snapshot");
    assert_eq!(pending.status, AgentStatus::PendingInit);
    assert_eq!(pending.pending_mail_count, 2);
    let child_history = runtime
        .store
        .protocol_event_store()
        .list_history_items_for_session(child.session.id)
        .expect("pending child history");
    assert_eq!(
        child_history
            .iter()
            .filter(|item| matches!(
                &item.payload,
                HistoryItemPayload::InterAgentCommunication { .. }
            ))
            .count(),
        0
    );
    let trigger_history_item_id = runtime
        .store
        .session_repo()
        .pending_agent_trigger_history_item_id(child.session.id)
        .expect("pending child trigger query")
        .expect("pending child trigger");
    let child_lease = child_lease
        .try_bind_trigger_history_item_id(trigger_history_item_id)
        .unwrap_or_else(|_| panic!("pending child lease must accept its durable wake owner"));
    let child_context = AgentRunContext {
        runtime: runtime.clone(),
        tree: tree.clone(),
        path: child_path.clone(),
        session_id: child.session.id,
        wake_cause: child_lease.wake_cause(),
        execution: child_lease.scope(),
        turn_owner: Arc::new(OnceLock::new()),
        config: captured_turn_config(config),
        workspace: child.workspace.clone(),
        confirmation: root_execution.context.confirmation.clone(),
        run_service: root_execution.context.run_service.clone(),
    };

    let child_turn_id = TurnId::new();
    let admission = runtime
        .store
        .session_repo()
        .admit_agent_triggered_turn(child.session.id, child_turn_id, trigger_history_item_id)
        .await
        .expect("child admission")
        .expect("child admission owner");
    child_context
        .bind_durable_turn_owner(admission.admission_id, child_turn_id)
        .expect("bind admitted child owner");
    let delivered = child_context
        .commit_pending_mailbox_delivery(AgentMailboxDeliverySelector::AllPending, 128)
        .expect("safe pending-child mailbox delivery");
    assert_eq!(delivered.history_item_ids.len(), 2);
    let child_history = runtime
        .store
        .protocol_event_store()
        .list_history_items_for_session(child.session.id)
        .expect("delivered child history");
    assert_eq!(
        child_history
            .iter()
            .filter(|item| matches!(
                &item.payload,
                HistoryItemPayload::InterAgentCommunication { .. }
            ))
            .count(),
        2
    );
    assert!(child_history.iter().all(|item| {
        !matches!(
            item.payload,
            HistoryItemPayload::InterAgentCommunication { .. }
        ) || item.scope.turn_id() == Some(child_turn_id)
    }));
    child_context
        .mark_durable_turn_admitted()
        .expect("publish admitted child");
    let running = tree
        .control
        .list_agents(Some(&child_path))
        .expect("running child")
        .into_iter()
        .next()
        .expect("running child snapshot");
    assert_eq!(running.status, AgentStatus::Running);
    assert_eq!(
        running.last_activity.as_deref(),
        Some("Running assigned task")
    );

    child_context
        .cancel_for_durable_terminal()
        .expect("close pending child mailbox");
    terminalize_admitted_test_session(
        &runtime,
        child.session.id,
        admission.admission_id,
        child_turn_id,
        &terminal_event(
            child.session.id,
            TurnTerminalOutcome::Interrupted {
                cause: TurnInterruptionCause::AgentInterrupted,
            },
            None,
        ),
    )
    .await;
    tree.control
        .complete_execution(child_lease, InactiveAgentStatus::Interrupted, None)
        .expect("complete pending child");
    runtime.complete_root(
        root_execution,
        &Ok(terminal_summary(
            root_session.session.id,
            TurnTerminalOutcome::Completed,
        )),
        None,
    );
}

#[tokio::test]
async fn completed_root_exact_stop_is_rejected_before_explicit_tree_stop() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");
    let storage_paths = StoragePaths {
        data_dir: root.join(".moyai-data"),
        database_path: root.join(".moyai-data/moyai.sqlite3"),
        truncation_dir: root.join(".moyai-data/truncation"),
    };
    let sqlite = SqliteStore::open(&storage_paths).expect("store");
    sqlite.migrate().expect("migrate");
    let store = StoreBundle::new(sqlite);
    let mut config = ResolvedConfig::default();
    config.multi_agent.enabled = true;
    config.multi_agent.max_concurrent_agents = 2;
    let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
    store
        .project_repo()
        .upsert_project(
            workspace.project_id,
            &workspace.root,
            "agent-tree-quiescence-test",
            "none",
        )
        .await
        .expect("project");
    let session_service = crate::session::SessionService::new(store.clone());
    let session = session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("tree quiescence".to_string()),
                cwd: root,
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            workspace,
        )
        .await
        .expect("session");
    let runtime = Arc::new(AgentRuntime::new(store, session_service));
    let root_control = RunControl::new();
    let execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            root_control.clone(),
        )
        .await
        .expect("root execution");
    let root_turn_control = execution.run_control();
    let tree = execution.context.tree.clone();
    let (_, child_lease) = tree
        .control
        .register_child(
            &crate::runtime::AgentPath::root(),
            "detached",
            SessionId::new(),
            Some("detached work".to_string()),
        )
        .expect("detached child");
    let child_cancel = child_lease.cancel_token();
    let summary = Ok(terminal_summary(
        session.session.id,
        TurnTerminalOutcome::Completed,
    ));
    assert!(root_turn_control.seal_success());
    runtime.complete_root(execution, &summary, None);
    assert!(
        !child_cancel.is_cancelled(),
        "successful root completion must preserve detached child work"
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            runtime.wait_for_tree_quiescence(session.session.id),
        )
        .await
        .is_err(),
        "root completion must not make a tree with a detached child quiescent"
    );
    assert!(!root_control.interrupt(TurnInterruptionCause::UserStop));
    assert!(!root_control.interrupt(TurnInterruptionCause::ApprovalAborted));
    assert_eq!(root_control.cause(), None);
    assert!(!child_cancel.is_cancelled());
    assert!(!tree.control.tree_is_cancelled());
    assert!(runtime.cancel_tree_for_session(session.session.id, TurnInterruptionCause::UserStop,));
    tokio::time::timeout(Duration::from_secs(1), child_cancel.cancelled())
        .await
        .expect("explicit tree Stop reached the active child while preserving sealed turn success");
    assert!(tree.control.tree_is_cancelled());
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            runtime.wait_for_tree_quiescence(session.session.id),
        )
        .await
        .is_err(),
        "the stopped child must retain its execution until terminal settlement"
    );
    assert!(!runtime.cancel_tree_for_session(session.session.id, TurnInterruptionCause::UserStop,));
    tree.control
        .complete_execution(child_lease, InactiveAgentStatus::Interrupted, None)
        .expect("complete stopped detached child");
    tokio::time::timeout(
        Duration::from_secs(1),
        runtime.wait_for_tree_quiescence(session.session.id),
    )
    .await
    .expect("bounded quiescence wait")
    .expect("tree quiescence");
}

#[tokio::test]
async fn failed_root_terminal_keeps_detached_child_live() {
    let (runtime, session, config) = direct_runtime_fixture("root-failure-independent", 2).await;
    let execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    let tree = execution.context.tree.clone();
    let (_, child_lease) = tree
        .control
        .register_child(
            &AgentPath::root(),
            "detached",
            SessionId::new(),
            Some("detached work".to_string()),
        )
        .expect("detached child");
    let child_cancel = child_lease.cancel_token();

    runtime.complete_root(
        execution,
        &Err(AppRunError::Message("root admission failed".to_string())),
        None,
    );

    assert!(!tree.control.tree_is_cancelled());
    assert!(!child_cancel.is_cancelled());
    assert!(matches!(
        tree.control
            .status(&AgentPath::root())
            .expect("root status"),
        AgentStatus::Errored(_)
    ));
    tree.control
        .complete_execution(child_lease, InactiveAgentStatus::Completed(None), None)
        .expect("complete independent child");
    runtime
        .wait_for_tree_quiescence(session.session.id)
        .await
        .expect("failed tree quiescence");
}

#[tokio::test]
async fn durable_failed_root_keeps_active_and_queued_children_independent() {
    let (runtime, session, config) =
        direct_runtime_fixture("durable-root-failure-independent", 2).await;
    let root_control = RunControl::new();
    let execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            root_control.clone(),
        )
        .await
        .expect("root execution");
    let tree = execution.context.tree.clone();
    let (_, active_child) = tree
        .control
        .register_child(
            &AgentPath::root(),
            "active",
            SessionId::new(),
            Some("active child".to_string()),
        )
        .expect("active child");
    let active_child_control = active_child.run_control();
    let queued_path = AgentPath::root().join("queued").expect("queued path");
    tree.control
        .restore_inactive_child(
            &AgentPath::root(),
            "queued",
            SessionId::new(),
            InactiveAgentStatus::Completed(None),
            Some("queued follow-up".to_string()),
        )
        .expect("queued child row");
    tree.control
        .restore_pending_mail(&queued_path, HistoryItemId::new(), false)
        .expect("restore dormant queued follow-up");

    runtime.complete_root(
        execution,
        &Ok(terminal_summary(
            session.session.id,
            TurnTerminalOutcome::Failed {
                error: "root failed".to_string(),
            },
        )),
        None,
    );

    assert!(!tree.control.tree_is_cancelled());
    assert_eq!(root_control.cause(), None);
    assert_eq!(active_child_control.cause(), None);
    assert_eq!(
        tree.control
            .status(&AgentPath::root())
            .expect("root status"),
        AgentStatus::Errored("root failed".to_string()),
    );
    let queued = tree
        .control
        .list_agents(Some(&queued_path))
        .expect("queued projection")
        .into_iter()
        .find(|agent| agent.path == queued_path)
        .expect("queued child");
    assert!(
        !queued.is_active,
        "root failure must not reschedule queued work"
    );
    assert_eq!(queued.pending_mail_count, 1);

    tree.control
        .complete_execution(active_child, InactiveAgentStatus::Completed(None), None)
        .expect("settle independent active child");
}

#[tokio::test]
async fn durable_root_interruption_status_preserves_exact_local_owners() {
    let (runtime, session, config) = direct_runtime_fixture("durable-root-stop-authority", 2).await;
    let root_control = RunControl::new();
    let execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            root_control.clone(),
        )
        .await
        .expect("root execution");
    let root_turn_control = execution.run_control();
    let tree = execution.context.tree.clone();
    let (_, child) = tree
        .control
        .register_child(
            &AgentPath::root(),
            "child",
            SessionId::new(),
            Some("running child".to_string()),
        )
        .expect("child");
    let child_control = child.run_control();
    assert!(root_control.interrupt(TurnInterruptionCause::ApprovalAborted));

    runtime.complete_root(
        execution,
        &Ok(terminal_summary(
            session.session.id,
            TurnTerminalOutcome::Interrupted {
                cause: TurnInterruptionCause::UserStop,
            },
        )),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted,
        )),
    );

    assert_eq!(
        root_turn_control.cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted
        )),
        "the local first-writer record is immutable"
    );
    assert_eq!(root_control.cause(), None);
    assert!(!tree.control.tree_is_cancelled());
    assert_eq!(child_control.cause(), None);
    assert_eq!(
        tree.control
            .status(&AgentPath::root())
            .expect("durable root status"),
        AgentStatus::Interrupted
    );
    tree.control
        .complete_execution(child, InactiveAgentStatus::Completed(None), None)
        .expect("settle child");
}

#[tokio::test]
async fn durable_root_success_rejects_exact_deferred_stop_before_explicit_tree_stop() {
    let (runtime, session, config) = direct_runtime_fixture("late-root-cancel", 2).await;
    let root_control = RunControl::new();
    let execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            root_control.clone(),
        )
        .await
        .expect("root execution");
    let root_turn_control = execution.run_control();
    let tree = execution.context.tree.clone();
    let (_, child_lease) = tree
        .control
        .register_child(
            &AgentPath::root(),
            "detached",
            SessionId::new(),
            Some("detached work".to_string()),
        )
        .expect("detached child");
    let child_cancel = child_lease.cancel_token();

    let success_commit = root_turn_control
        .begin_success_commit()
        .expect("reserve durable success commit");
    assert!(matches!(
        root_control.request_cancel(RunCancellationCause::Interruption(
            TurnInterruptionCause::UserStop,
        )),
        RunCancelOutcome::Deferred(_)
    ));
    assert_eq!(
        root_control.request_cancel(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted,
        )),
        RunCancelOutcome::Rejected
    );
    assert!(!child_cancel.is_cancelled());
    assert!(!tree.control.tree_is_cancelled());
    assert!(success_commit.seal());
    assert!(root_turn_control.success_is_sealed());

    runtime.complete_root(
        execution,
        &Ok(terminal_summary(
            session.session.id,
            TurnTerminalOutcome::Completed,
        )),
        None,
    );

    assert!(!tree.control.tree_is_cancelled());
    assert!(!child_cancel.is_cancelled());
    assert!(matches!(
        tree.control
            .status(&AgentPath::root())
            .expect("root status"),
        AgentStatus::Completed(_)
    ));
    assert!(runtime.cancel_tree_for_session(session.session.id, TurnInterruptionCause::UserStop,));
    assert!(tree.control.tree_is_cancelled());
    assert!(child_cancel.is_cancelled());
    tree.control
        .complete_execution(child_lease, InactiveAgentStatus::Interrupted, None)
        .expect("complete detached child");
}

#[tokio::test]
async fn zero_child_exact_stop_loses_to_success_and_allows_idle_root_continuation() {
    let (runtime, session, config) =
        direct_runtime_fixture("zero-child-stop-continuation", 1).await;
    let root_control = RunControl::new();
    let execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            root_control.clone(),
        )
        .await
        .expect("root execution");
    let root_turn_control = execution.run_control();
    let tree = execution.context.tree.clone();
    let success = root_turn_control
        .begin_success_commit()
        .expect("reserve durable success commit");

    assert!(matches!(
        root_control.request_cancel(RunCancellationCause::Interruption(
            TurnInterruptionCause::UserStop,
        )),
        RunCancelOutcome::Deferred(_)
    ));
    assert_eq!(root_control.cause(), None);
    assert!(!tree.control.tree_is_cancelled());
    assert!(success.seal());
    runtime.complete_root(
        execution,
        &Ok(terminal_summary(
            session.session.id,
            TurnTerminalOutcome::Completed,
        )),
        None,
    );

    assert!(root_turn_control.success_is_sealed());
    let continuation = match runtime
        .begin_root_continuation(
            session.session.id,
            root_control.clone(),
            Some(SharedConfirmationPrompt::new(AllowPrompt)),
        )
        .expect("continuation outcome")
    {
        AgentRuntimeContinuationOutcome::Admitted(execution) => execution,
        AgentRuntimeContinuationOutcome::Blocked
        | AgentRuntimeContinuationOutcome::NotReady
        | AgentRuntimeContinuationOutcome::Invalid => panic!("continuation was not admitted"),
    };
    assert_eq!(continuation.run_control().cause(), None);
    assert_eq!(root_control.cause(), None);
    assert!(!tree.control.tree_is_cancelled());
    runtime
        .release_unadmitted_root_continuation(continuation)
        .expect("release admitted continuation");
    assert!(tree.control.is_quiescent().expect("tree quiescence"));
}

#[tokio::test]
async fn inactive_goal_releases_preclaimed_continuation_without_failing_root_scope() {
    let (runtime, session, config) =
        direct_runtime_fixture("inactive-goal-continuation-release", 1).await;
    let root_scope = RunControl::new();
    let first_execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            root_scope.clone(),
        )
        .await
        .expect("first root execution");
    let tree = first_execution.context.tree.clone();
    assert!(first_execution.run_control().seal_success());
    runtime.complete_root(
        first_execution,
        &Ok(terminal_summary(
            session.session.id,
            TurnTerminalOutcome::Completed,
        )),
        None,
    );

    let continuation = match runtime
        .begin_root_continuation(
            session.session.id,
            root_scope.clone(),
            Some(SharedConfirmationPrompt::new(AllowPrompt)),
        )
        .expect("continuation claim")
    {
        AgentRuntimeContinuationOutcome::Admitted(execution) => execution,
        AgentRuntimeContinuationOutcome::Blocked
        | AgentRuntimeContinuationOutcome::NotReady
        | AgentRuntimeContinuationOutcome::Invalid => panic!("continuation was not admitted"),
    };
    let unadmitted_turn = continuation.run_control();
    runtime
        .release_unadmitted_root_continuation(continuation)
        .expect("release inactive-goal continuation");

    assert!(!unadmitted_turn.success_is_sealed());
    assert_eq!(unadmitted_turn.cause(), None);
    assert_eq!(root_scope.cause(), None);
    assert!(!tree.control.tree_is_cancelled());
    assert!(tree.control.is_quiescent().expect("tree quiescence"));
    assert!(matches!(
        tree.control
            .status(&AgentPath::root())
            .expect("root status"),
        AgentStatus::Completed(_)
    ));
}

#[tokio::test]
async fn root_continuation_claim_before_stop_cancels_only_the_claimed_turn() {
    let (runtime, session, config) =
        direct_runtime_fixture("claimed-root-continuation-stop", 1).await;
    let root_control = RunControl::new();
    let execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            root_control.clone(),
        )
        .await
        .expect("root execution");
    let first_turn_control = execution.run_control();
    let tree = execution.context.tree.clone();
    assert!(first_turn_control.seal_success());
    runtime.complete_root(
        execution,
        &Ok(terminal_summary(
            session.session.id,
            TurnTerminalOutcome::Completed,
        )),
        None,
    );

    let continuation = match runtime
        .begin_root_continuation(
            session.session.id,
            root_control.clone(),
            Some(SharedConfirmationPrompt::new(AllowPrompt)),
        )
        .expect("continuation outcome")
    {
        AgentRuntimeContinuationOutcome::Admitted(execution) => execution,
        AgentRuntimeContinuationOutcome::Blocked
        | AgentRuntimeContinuationOutcome::NotReady
        | AgentRuntimeContinuationOutcome::Invalid => panic!("continuation was not admitted"),
    };
    let continuation_control = continuation.run_control();
    assert!(Arc::ptr_eq(&tree, &continuation.context.tree));
    assert!(!continuation_control.same_owner(&first_turn_control));
    assert!(!continuation_control.same_owner(&root_control));

    assert_eq!(
        root_control.request_cancel(RunCancellationCause::Interruption(
            TurnInterruptionCause::UserStop,
        )),
        RunCancelOutcome::Applied
    );
    assert_eq!(root_control.cause(), None);
    assert!(!tree.control.tree_is_cancelled());
    assert_eq!(
        continuation_control.cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::UserStop
        ))
    );
    runtime.complete_root(
        continuation,
        &Ok(terminal_summary(
            session.session.id,
            TurnTerminalOutcome::Interrupted {
                cause: TurnInterruptionCause::UserStop,
            },
        )),
        continuation_control.cause(),
    );
    assert!(tree.control.is_quiescent().expect("tree quiescence"));
}

#[tokio::test]
async fn preclaimed_root_early_error_is_owned_by_the_continuation_turn() {
    let (runtime, session, config) = direct_runtime_fixture("preclaimed-root-early-error", 1).await;
    let root_control = RunControl::new();
    let execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            root_control.clone(),
        )
        .await
        .expect("root execution");
    let first_turn_control = execution.run_control();
    let tree = execution.context.tree.clone();
    assert!(first_turn_control.seal_success());
    runtime.complete_root(
        execution,
        &Ok(terminal_summary(
            session.session.id,
            TurnTerminalOutcome::Completed,
        )),
        None,
    );
    let continuation = match runtime
        .begin_root_continuation(
            session.session.id,
            root_control.clone(),
            Some(SharedConfirmationPrompt::new(AllowPrompt)),
        )
        .expect("continuation outcome")
    {
        AgentRuntimeContinuationOutcome::Admitted(execution) => execution,
        AgentRuntimeContinuationOutcome::Blocked
        | AgentRuntimeContinuationOutcome::NotReady
        | AgentRuntimeContinuationOutcome::Invalid => panic!("continuation was not admitted"),
    };
    let continuation_control = continuation.run_control();
    let result = Err(crate::error::AppRunError::Message(
        "continuation setup failed".to_string(),
    ));
    crate::app::run_service::classify_run_error(
        &continuation_control,
        result.as_ref().expect_err("early error"),
    );
    runtime.complete_root(continuation, &result, continuation_control.cause());

    assert_eq!(
        continuation_control.cause(),
        Some(RunCancellationCause::Failure(
            "continuation setup failed".to_string()
        ))
    );
    assert_eq!(root_control.cause(), None);
    assert!(!tree.control.tree_is_cancelled());
    assert!(tree.control.is_quiescent().expect("tree quiescence"));
    assert!(matches!(
        tree.control
            .status(&AgentPath::root())
            .expect("root status"),
        AgentStatus::Errored(_)
    ));
}

#[tokio::test]
async fn active_root_cancel_reaches_only_the_exact_root_turn_before_settlement() {
    let (runtime, session, config) = direct_runtime_fixture("active-root-cancel", 2).await;
    let root_control = RunControl::new();
    let execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            root_control.clone(),
        )
        .await
        .expect("root execution");
    let root_turn_control = execution.run_control();
    let tree = execution.context.tree.clone();
    let root_turn_cancel = root_turn_control.token();
    let root_scope_cancel = root_control.token();
    let (_, child_lease) = tree
        .control
        .register_child(
            &AgentPath::root(),
            "detached",
            SessionId::new(),
            Some("detached work".to_string()),
        )
        .expect("detached child");
    let child_cancel = child_lease.cancel_token();

    assert!(root_control.interrupt(TurnInterruptionCause::UserStop));
    tokio::time::timeout(Duration::from_secs(1), root_turn_cancel.cancelled())
        .await
        .expect("external cancellation reached the exact active root turn");
    assert_eq!(
        root_turn_control.cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::UserStop
        ))
    );
    assert_eq!(root_control.cause(), None);
    assert!(!root_scope_cancel.is_cancelled());
    assert!(!child_cancel.is_cancelled());
    assert!(!tree.control.tree_is_cancelled());

    runtime.complete_root(
        execution,
        &Err(AppRunError::Message("root cancelled".to_string())),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::UserStop,
        )),
    );
    assert!(!tree.control.tree_is_cancelled());
    assert!(!child_cancel.is_cancelled());
    tree.control
        .complete_execution(child_lease, InactiveAgentStatus::Completed(None), None)
        .expect("complete detached child");
}

#[tokio::test]
async fn root_context_durable_terminal_accessor_cancels_only_the_root() {
    let (runtime, session, config) = direct_runtime_fixture("root-durable-terminal", 2).await;
    let root_control = RunControl::new();
    let execution = runtime
        .begin_root(
            &session,
            captured_turn_config(config),
            SharedConfirmationPrompt::new(AllowPrompt),
            root_control.clone(),
        )
        .await
        .expect("root execution");
    let root_turn_control = execution.run_control();
    let tree = execution.context.tree.clone();
    let (_, child) = tree
        .control
        .register_child(
            &AgentPath::root(),
            "child",
            SessionId::new(),
            Some("independent child".to_string()),
        )
        .expect("child");
    let child_control = child.run_control();
    assert!(!tree.control.tree_is_cancelled());

    execution
        .context
        .cancel_for_durable_terminal()
        .expect("durable root terminal");
    assert!(!tree.control.tree_is_cancelled());
    assert_eq!(
        root_turn_control.cause(),
        Some(RunCancellationCause::Superseded)
    );
    assert_eq!(root_control.cause(), None);
    assert_eq!(child_control.cause(), None);

    runtime.complete_root(
        execution,
        &Err(AppRunError::Message("root turn was superseded".to_string())),
        Some(RunCancellationCause::Superseded),
    );
    tree.control
        .complete_execution(child, InactiveAgentStatus::Completed(None), None)
        .expect("settle independent child");
}

#[derive(Default)]
struct AgentScriptState {
    root_calls: AtomicUsize,
    child_calls: AtomicUsize,
    grandchild_calls: AtomicUsize,
    child_spawns_grandchild: AtomicBool,
    child_saw_grandchild_result: AtomicBool,
    grandchild_request_started: AtomicBool,
    root_plans_before_spawn_with_sibling: AtomicBool,
    requests: Mutex<Vec<ChatRequest>>,
}

struct AgentScriptClient {
    state: Arc<AgentScriptState>,
}

#[derive(Default)]
struct DetachedGoalScriptState {
    root_calls: AtomicUsize,
    child_calls: AtomicUsize,
    first_root_turn_finished: AtomicBool,
    child_finished: AtomicBool,
    continuation_saw_child_result: AtomicBool,
    child_waits_for_interrupt: AtomicBool,
    goal_update_emitted: AtomicBool,
}

struct DetachedGoalScriptClient {
    state: Arc<DetachedGoalScriptState>,
    complete_goal_after_child: bool,
}

#[async_trait(?Send)]
impl LlmClient for DetachedGoalScriptClient {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
        sink: &mut dyn LlmEventSink,
    ) -> Result<LlmResponseSummary, LlmError> {
        let is_child = request.messages.iter().any(|message| {
            matches!(message, ModelMessage::Agent { content }
                if content.contains("Message Type: NEW_TASK")
                    && content.contains(DETACHED_CHILD_ASSIGNMENT))
        });
        if is_child {
            let call = self.state.child_calls.fetch_add(1, Ordering::SeqCst);
            if call != 0 {
                return Err(LlmError::Message(format!(
                    "unexpected detached child request {}",
                    call + 1
                )));
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while !self.state.first_root_turn_finished.load(Ordering::SeqCst) {
                if Instant::now() >= deadline {
                    return Err(LlmError::Message(
                        "detached child timed out waiting for the first root turn".to_string(),
                    ));
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if self.state.child_waits_for_interrupt.load(Ordering::SeqCst) {
                cancel.cancelled().await;
                return Err(LlmError::Message(
                    "detached child provider request interrupted".to_string(),
                ));
            }
            if self.complete_goal_after_child {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            sink.push(LlmEvent::TextDelta(DETACHED_CHILD_RESULT.to_string()))?;
            self.state.child_finished.store(true, Ordering::SeqCst);
            return Ok(response_summary(FinishReason::Stop));
        }

        match self.state.root_calls.fetch_add(1, Ordering::SeqCst) {
            0 => {
                emit_tool_call(
                    sink,
                    "spawn_detached",
                    "spawn_agent",
                    json!({
                        "task_name": "detached",
                        "message": DETACHED_CHILD_ASSIGNMENT,
                        "fork_turns": "none"
                    }),
                )?;
                Ok(response_summary(FinishReason::ToolCall))
            }
            1 => {
                if self.state.child_finished.load(Ordering::SeqCst) {
                    return Err(LlmError::Message(
                        "detached child unexpectedly completed before the first root turn"
                            .to_string(),
                    ));
                }
                sink.push(LlmEvent::TextDelta(
                    "root turn completed while detached child is active".to_string(),
                ))?;
                self.state
                    .first_root_turn_finished
                    .store(true, Ordering::SeqCst);
                Ok(response_summary(FinishReason::Stop))
            }
            2 => {
                let saw_child_result = request.messages.iter().any(|message| {
                    matches!(message, ModelMessage::Agent { content }
                        if content.contains("Message Type: FINAL_ANSWER")
                            && content.contains(DETACHED_CHILD_RESULT))
                });
                self.state
                    .continuation_saw_child_result
                    .store(saw_child_result, Ordering::SeqCst);
                if !saw_child_result {
                    if self.complete_goal_after_child {
                        emit_tool_call(
                            sink,
                            "wait_detached",
                            "wait_agent",
                            json!({"timeout_ms": 10_000}),
                        )?;
                        return Ok(response_summary(FinishReason::ToolCall));
                    }
                    return Err(LlmError::Message(
                        "goal-less detached root unexpectedly resumed".to_string(),
                    ));
                }
                if self.complete_goal_after_child {
                    self.state.goal_update_emitted.store(true, Ordering::SeqCst);
                    emit_tool_call(
                        sink,
                        "complete_goal",
                        "update_goal",
                        json!({"status": "complete"}),
                    )?;
                    Ok(response_summary(FinishReason::ToolCall))
                } else {
                    sink.push(LlmEvent::TextDelta(
                        "root continuation integrated detached child".to_string(),
                    ))?;
                    Ok(response_summary(FinishReason::Stop))
                }
            }
            3 => {
                if !self.complete_goal_after_child {
                    return Err(LlmError::Message(
                        "a goal-less root made a model request after integrating its child result"
                            .to_string(),
                    ));
                }
                let saw_child_result = request.messages.iter().any(|message| {
                    matches!(message, ModelMessage::Agent { content }
                        if content.contains("Message Type: FINAL_ANSWER")
                            && content.contains(DETACHED_CHILD_RESULT))
                });
                self.state
                    .continuation_saw_child_result
                    .store(saw_child_result, Ordering::SeqCst);
                if !saw_child_result {
                    return Err(LlmError::Message(
                        "wait_agent returned without the detached child result".to_string(),
                    ));
                }
                if !self.state.goal_update_emitted.swap(true, Ordering::SeqCst) {
                    emit_tool_call(
                        sink,
                        "complete_goal",
                        "update_goal",
                        json!({"status": "complete"}),
                    )?;
                    return Ok(response_summary(FinishReason::ToolCall));
                }
                sink.push(LlmEvent::TextDelta(
                    "goal continuation integrated detached child".to_string(),
                ))?;
                Ok(response_summary(FinishReason::Stop))
            }
            4 if self.complete_goal_after_child => {
                sink.push(LlmEvent::TextDelta(
                    "goal continuation integrated detached child".to_string(),
                ))?;
                Ok(response_summary(FinishReason::Stop))
            }
            call => Err(LlmError::Message(format!(
                "unexpected detached root request {}",
                call + 1
            ))),
        }
    }
}

#[async_trait(?Send)]
impl LlmClient for AgentScriptClient {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        _cancel: CancellationToken,
        sink: &mut dyn LlmEventSink,
    ) -> Result<LlmResponseSummary, LlmError> {
        let is_grandchild = request.messages.iter().any(|message| {
            matches!(message, ModelMessage::Agent { content }
                if content.contains("Message Type: NEW_TASK")
                    && content.contains(GRANDCHILD_ASSIGNMENT))
        });
        let is_child = request.messages.iter().any(|message| {
            matches!(message, ModelMessage::Agent { content }
                if content.contains("Message Type: NEW_TASK")
                    && content.contains(CHILD_ASSIGNMENT))
        });
        self.state
            .requests
            .lock()
            .expect("request capture mutex")
            .push(request.clone());

        if is_grandchild {
            let call = self.state.grandchild_calls.fetch_add(1, Ordering::SeqCst);
            return match call {
                0 => {
                    self.state
                        .grandchild_request_started
                        .store(true, Ordering::SeqCst);
                    emit_tool_call(
                        sink,
                        "grandchild_apply_patch",
                        "apply_patch",
                        json!({
                            "patch_text": format!(
                                "*** Begin Patch\n*** Add File: {GRANDCHILD_ARTIFACT}\n+grandchild durable artifact\n*** End Patch"
                            )
                        }),
                    )?;
                    Ok(response_summary(FinishReason::ToolCall))
                }
                1 => {
                    sink.push(LlmEvent::TextDelta(GRANDCHILD_RESULT.to_string()))?;
                    Ok(response_summary(FinishReason::Stop))
                }
                _ => Err(LlmError::Message(format!(
                    "unexpected grandchild model request {}",
                    call + 1
                ))),
            };
        }

        if is_child {
            let call = self.state.child_calls.fetch_add(1, Ordering::SeqCst);
            if self.state.child_spawns_grandchild.load(Ordering::SeqCst) {
                return match call {
                    0 => {
                        emit_tool_call(
                            sink,
                            "spawn_grandchild",
                            "spawn_agent",
                            json!({
                                "task_name": "reviewer",
                                "message": GRANDCHILD_ASSIGNMENT,
                                "fork_turns": "none"
                            }),
                        )?;
                        Ok(response_summary(FinishReason::ToolCall))
                    }
                    1 => {
                        emit_tool_call(
                            sink,
                            "wait_grandchild",
                            "wait_agent",
                            json!({"timeout_ms": 10_000}),
                        )?;
                        Ok(response_summary(FinishReason::ToolCall))
                    }
                    2 => {
                        let saw_result = request.messages.iter().any(|message| {
                            matches!(message, ModelMessage::Agent { content }
                                if content.contains("Message Type: FINAL_ANSWER")
                                    && content.contains(GRANDCHILD_RESULT))
                        });
                        self.state
                            .child_saw_grandchild_result
                            .store(saw_result, Ordering::SeqCst);
                        if !saw_result {
                            return Err(LlmError::Message(
                                "child continuation did not receive the grandchild final answer"
                                    .to_string(),
                            ));
                        }
                        emit_tool_call(
                            sink,
                            "child_apply_patch",
                            "apply_patch",
                            json!({
                                "patch_text": format!(
                                    "*** Begin Patch\n*** Add File: {CHILD_ARTIFACT}\n+child durable artifact\n*** End Patch"
                                )
                            }),
                        )?;
                        Ok(response_summary(FinishReason::ToolCall))
                    }
                    3 => {
                        emit_tool_call(sink, "child_list", "list", json!({"path": "."}))?;
                        Ok(response_summary(FinishReason::ToolCall))
                    }
                    4 => {
                        sink.push(LlmEvent::TextDelta(CHILD_RESULT.to_string()))?;
                        Ok(response_summary(FinishReason::Stop))
                    }
                    _ => Err(LlmError::Message(format!(
                        "unexpected child model request {}",
                        call + 1
                    ))),
                };
            }
            if call != 0 {
                return Err(LlmError::Message(format!(
                    "unexpected child model request {}",
                    call + 1
                )));
            }
            tokio::time::sleep(Duration::from_millis(75)).await;
            sink.push(LlmEvent::TextDelta(CHILD_RESULT.to_string()))?;
            return Ok(response_summary(FinishReason::Stop));
        }

        if self
            .state
            .root_plans_before_spawn_with_sibling
            .load(Ordering::SeqCst)
        {
            return match self.state.root_calls.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    emit_tool_call(
                        sink,
                        "root_local_plan",
                        "update_plan",
                        json!({
                            "explanation": "Root owns a distinct initial coordination blocker.",
                            "plan": [{
                                "step": "Establish the initial coordination boundary",
                                "status": "in_progress"
                            }]
                        }),
                    )?;
                    Ok(response_summary(FinishReason::ToolCall))
                }
                1 => {
                    sink.push(LlmEvent::TextDelta(ROOT_PLAN.to_string()))?;
                    emit_tool_call(
                        sink,
                        "spawn_1",
                        "spawn_agent",
                        json!({
                            "task_name": "worker",
                            "message": CHILD_ASSIGNMENT,
                            "fork_turns": "all"
                        }),
                    )?;
                    emit_tool_call(sink, "duplicate_root_list", "list", json!({"path": "."}))?;
                    Ok(response_summary(FinishReason::ToolCall))
                }
                2 => {
                    emit_tool_call(sink, "wait_1", "wait_agent", json!({"timeout_ms": 10_000}))?;
                    Ok(response_summary(FinishReason::ToolCall))
                }
                3 => {
                    let received_child_result = request.messages.iter().any(|message| {
                        matches!(message, ModelMessage::Agent { content }
                            if content.contains("Message Type: FINAL_ANSWER")
                                && content.contains(CHILD_RESULT))
                    });
                    if !received_child_result {
                        return Err(LlmError::Message(
                            "root resumed without the child's durable communication".to_string(),
                        ));
                    }
                    sink.push(LlmEvent::TextDelta(ROOT_RESULT.to_string()))?;
                    Ok(response_summary(FinishReason::Stop))
                }
                call => Err(LlmError::Message(format!(
                    "unexpected root model request {}",
                    call + 1
                ))),
            };
        }

        match self.state.root_calls.fetch_add(1, Ordering::SeqCst) {
            0 => {
                sink.push(LlmEvent::TextDelta(ROOT_PLAN.to_string()))?;
                emit_tool_call(
                    sink,
                    "spawn_1",
                    "spawn_agent",
                    json!({
                        "task_name": "worker",
                        "message": CHILD_ASSIGNMENT,
                        "fork_turns": "all"
                    }),
                )?;
                Ok(response_summary(FinishReason::ToolCall))
            }
            1 => {
                emit_tool_call(sink, "wait_1", "wait_agent", json!({"timeout_ms": 10_000}))?;
                Ok(response_summary(FinishReason::ToolCall))
            }
            2 => {
                let received_child_result = request.messages.iter().any(|message| {
                    matches!(message, ModelMessage::Agent { content }
                        if content.contains("Message Type: FINAL_ANSWER")
                            && content.contains(CHILD_RESULT))
                });
                if !received_child_result {
                    return Err(LlmError::Message(
                        "root resumed without the child's durable communication".to_string(),
                    ));
                }
                sink.push(LlmEvent::TextDelta(ROOT_RESULT.to_string()))?;
                Ok(response_summary(FinishReason::Stop))
            }
            call => Err(LlmError::Message(format!(
                "unexpected root model request {}",
                call + 1
            ))),
        }
    }
}

fn emit_tool_call(
    sink: &mut dyn LlmEventSink,
    call_id: &str,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<(), LlmError> {
    sink.push(LlmEvent::ToolCallStart {
        call_id: call_id.to_string(),
        tool_name: tool_name.to_string(),
    })?;
    sink.push(LlmEvent::ToolCallArgsDelta {
        call_id: call_id.to_string(),
        delta: arguments.to_string(),
    })
}

fn response_summary(finish_reason: FinishReason) -> LlmResponseSummary {
    LlmResponseSummary {
        finish_reason,
        usage: Some(TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            reasoning_tokens: None,
        }),
        response_id: None,
    }
}

fn bind_agent_script_run_service(
    runtime: &Arc<AgentRuntime>,
    session: &SessionContext,
    config: &ResolvedConfig,
    script: Arc<AgentScriptState>,
) -> Arc<RunService> {
    let store = runtime.store.clone();
    let session_service = runtime.session_service.clone();
    let tool_services = ToolServices {
        edit_safety: crate::edit::EditSafety::default(),
        formatter: crate::edit::Formatter::new(config.format.clone()),
        change_tracker: crate::edit::ChangeTracker::default(),
        store: store.clone(),
        storage_paths: store.paths().clone(),
        truncator: ToolTruncator,
        mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
        skills: crate::skill::SkillsService::new(),
    };
    let registry = ToolRegistry::core_agent_for_config(config);
    let llm = Arc::new(AgentScriptClient { state: script });
    let agent_loop = AgentLoop::new(llm, registry, store.clone(), PromptBuilder, tool_services)
        .with_model_request_concurrency(1);
    let run_service = Arc::new(RunService::new(
        store,
        config.clone(),
        session.workspace.clone(),
        session_service,
        agent_loop,
        SessionRuntimeEventHub::new(32),
        runtime.clone(),
    ));
    runtime
        .bind_run_service(Arc::downgrade(&run_service))
        .expect("bind agent run service");
    run_service
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_child_is_hard_stopped_at_worker_activation_before_run_admission() {
    let (runtime, root_session, config) =
        direct_runtime_fixture("cancelled-child-worker-activation", 2).await;
    let script = Arc::new(AgentScriptState::default());
    let _run_service =
        bind_agent_script_run_service(&runtime, &root_session, &config, script.clone());
    let root_execution = runtime
        .begin_root(
            &root_session,
            captured_turn_config(config.clone()),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
    let _root_owner = bind_test_root_turn(&runtime, &root_execution).await;
    let tree = root_execution.context.tree.clone();
    let (child_path, child_session_id, _trigger_history_item_id, child_lease) =
        commit_atomic_child_trigger_without_launch(
            &runtime,
            &root_execution.context,
            "cancelled_child",
        )
        .await;
    let child_context = AgentRunContext {
        runtime: runtime.clone(),
        tree: tree.clone(),
        path: child_path.clone(),
        session_id: child_session_id,
        wake_cause: child_lease.wake_cause(),
        execution: child_lease.scope(),
        turn_owner: Arc::new(OnceLock::new()),
        config: captured_turn_config(config),
        workspace: root_session.workspace.clone(),
        confirmation: root_execution.context.confirmation.clone(),
        run_service: root_execution.context.run_service.clone(),
    };

    assert!(tree.control.interrupt_tree(TurnInterruptionCause::UserStop));
    if let Err(failure) =
        runtime.launch_agent_turn(child_context, child_lease, CHILD_ASSIGNMENT.to_string())
    {
        panic!("worker thread launch failed: {}", failure.message);
    }

    let child_snapshot = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = tree
                .control
                .list_agents(Some(&child_path))
                .expect("child snapshot")
                .into_iter()
                .find(|agent| agent.path == child_path)
                .expect("retained child");
            if !snapshot.is_active {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled child worker did not settle");

    assert_eq!(child_snapshot.status, AgentStatus::Interrupted);
    assert_ne!(
        child_snapshot.last_activity.as_deref(),
        Some("Running assigned task")
    );
    assert_eq!(script.child_calls.load(Ordering::SeqCst), 0);
    assert_eq!(script.root_calls.load(Ordering::SeqCst), 0);
    assert!(
        script
            .requests
            .lock()
            .expect("request capture mutex")
            .is_empty()
    );
    assert!(
        runtime
            .store
            .session_repo()
            .fresh_running_turn_for_session(child_session_id)
            .await
            .expect("child active turn")
            .is_none()
    );
    assert!(
        runtime
            .store
            .protocol_event_store()
            .list_history_items_for_session(child_session_id)
            .expect("child history")
            .is_empty()
    );
    root_execution
        .complete(AgentStatus::Interrupted)
        .expect("complete stopped root");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_tree_mutation_follows_admission_and_setup_failure_releases_owner() {
    let (base_url, provider_server) = start_probe_provider().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");
    let storage_paths = StoragePaths {
        data_dir: root.join(".moyai-data"),
        database_path: root.join(".moyai-data/moyai.sqlite3"),
        truncation_dir: root.join(".moyai-data/truncation"),
    };
    let sqlite = SqliteStore::open(&storage_paths).expect("store");
    sqlite.migrate().expect("migrate");
    let store = StoreBundle::new(sqlite);
    let mut config = ResolvedConfig::default();
    config.model.model = "scripted".to_string();
    config.model.base_url = base_url.clone();
    config.model.provider_api_mode = ProviderApiMode::ChatCompletions;
    config.model.provider_metadata_mode = ProviderMetadataMode::OpenAiCompatibleOnly;
    config.model.supports_tools = true;
    config.model.connect_timeout_ms = 2_000;
    config.model.request_timeout_ms = 5_000;
    config.model.stream_idle_timeout_ms = 5_000;
    config.model.max_retries = 0;
    config.permissions.access_mode = AccessMode::FullAccess;
    config.multi_agent.enabled = true;
    config.multi_agent.max_concurrent_agents = 0;
    config.multi_agent.max_concurrent_model_requests = 1;
    let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
    store
        .project_repo()
        .upsert_project(
            workspace.project_id,
            &workspace.root,
            "agent-admission-order-test",
            "none",
        )
        .await
        .expect("project");
    let session_service = crate::session::SessionService::new(store.clone());
    let agent_runtime = Arc::new(AgentRuntime::new(store.clone(), session_service.clone()));
    let tool_services = ToolServices {
        edit_safety: crate::edit::EditSafety::default(),
        formatter: crate::edit::Formatter::new(config.format.clone()),
        change_tracker: crate::edit::ChangeTracker::default(),
        store: store.clone(),
        storage_paths: storage_paths.clone(),
        truncator: ToolTruncator,
        mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
        skills: crate::skill::SkillsService::new(),
    };
    let registry = ToolRegistry::core_agent_for_config(&config);
    let script = Arc::new(AgentScriptState::default());
    let llm = Arc::new(AgentScriptClient { state: script });
    let agent_loop = AgentLoop::new(llm, registry, store.clone(), PromptBuilder, tool_services)
        .with_model_request_concurrency(1);
    let run_service = Arc::new(RunService::new(
        store.clone(),
        config.clone(),
        workspace.clone(),
        session_service.clone(),
        agent_loop,
        SessionRuntimeEventHub::new(32),
        agent_runtime.clone(),
    ));
    agent_runtime
        .bind_run_service(Arc::downgrade(&run_service))
        .expect("bind runtime");

    let blocked = session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("blocked admission".to_string()),
                cwd: root.clone(),
                model: "scripted".to_string(),
                base_url: base_url.clone(),
                access_mode: AccessMode::FullAccess,
            },
            workspace.clone(),
        )
        .await
        .expect("blocked session");
    let _blocking_process_lease = store
        .try_acquire_run_process_lease(blocked.session.id)
        .expect("blocking process lease");
    let shared_confirmation = SharedConfirmationPrompt::new(AllowPrompt);
    let mut prompt = shared_confirmation.clone();
    let mut renderer = AgentEventRenderer;
    let blocked_error = run_service
        .execute(
            AppCommand::Run(RunRequest {
                prompt: "process lease must precede root setup".to_string(),
                session_id: Some(blocked.session.id),
                continue_last: false,
                title: None,
                cwd: root.clone(),
                config: crate::app::RunConfigInput::Layered {
                    model: "scripted".to_string(),
                    base_url: base_url.clone(),
                    config_override: None,
                },
                output_mode: OutputMode::Human,
                show_reasoning_summary: false,
                prompt_dispatch: None,
                editor_context: None,
                review_request: None,
                image_paths: Vec::new(),
                run_control: RunControl::new(),
                session_access_mode_adoption: None,
                agent_confirmation: Some(shared_confirmation.clone()),
                agent_context: None,
            }),
            &mut renderer,
            &mut prompt,
        )
        .await
        .expect_err("process lease must win before root setup");
    assert!(
        blocked_error
            .to_string()
            .contains("owned by another live process"),
        "unexpected pre-admission error: {blocked_error}"
    );
    assert!(!agent_runtime.has_tree_for_session(blocked.session.id));

    let setup_failure = session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("setup failure".to_string()),
                cwd: root.clone(),
                model: "scripted".to_string(),
                base_url: base_url.clone(),
                access_mode: AccessMode::FullAccess,
            },
            workspace,
        )
        .await
        .expect("setup failure session");
    let setup_outcome = run_service
        .execute(
            AppCommand::Run(RunRequest {
                prompt: "fail root setup after admission".to_string(),
                session_id: Some(setup_failure.session.id),
                continue_last: false,
                title: None,
                cwd: root,
                config: crate::app::RunConfigInput::Layered {
                    model: "scripted".to_string(),
                    base_url,
                    config_override: None,
                },
                output_mode: OutputMode::Human,
                show_reasoning_summary: false,
                prompt_dispatch: None,
                editor_context: None,
                review_request: None,
                image_paths: Vec::new(),
                run_control: RunControl::new(),
                session_access_mode_adoption: None,
                agent_confirmation: Some(shared_confirmation),
                agent_context: None,
            }),
            &mut renderer,
            &mut prompt,
        )
        .await
        .expect("invalid root setup must return its durable failed terminal");
    let crate::app::AppCommandOutcome::Turn(setup_summary) = setup_outcome else {
        panic!("invalid root setup must return a turn summary");
    };
    assert!(matches!(
        &setup_summary.terminal().outcome,
        TurnTerminalOutcome::Failed { error }
            if error.contains("max_concurrent_agents")
    ));
    assert_eq!(
        session_service
            .get_session(setup_failure.session.id)
            .await
            .expect("settled setup failure")
            .status,
        SessionStatus::Failed
    );
    assert!(
        !store
            .session_repo()
            .has_fresh_run_admission(setup_failure.session.id)
            .await
            .expect("released setup admission")
    );
    assert!(
        store
            .session_repo()
            .admit_session_turn(setup_failure.session.id, TurnId::new())
            .await
            .expect("readmission after setup failure")
            .is_some()
    );
    assert!(!agent_runtime.has_tree_for_session(setup_failure.session.id));
    provider_server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn goal_less_root_terminal_does_not_implicitly_resume_for_detached_child() {
    let (base_url, provider_server) = start_probe_provider().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");
    let storage_paths = StoragePaths {
        data_dir: root.join(".moyai-data"),
        database_path: root.join(".moyai-data/moyai.sqlite3"),
        truncation_dir: root.join(".moyai-data/truncation"),
    };
    let sqlite = SqliteStore::open(&storage_paths).expect("store");
    sqlite.migrate().expect("migrate");
    let store = StoreBundle::new(sqlite);
    let mut config = ResolvedConfig::default();
    config.model.model = "scripted".to_string();
    config.model.base_url = base_url.clone();
    config.model.provider_api_mode = ProviderApiMode::ChatCompletions;
    config.model.provider_metadata_mode = ProviderMetadataMode::OpenAiCompatibleOnly;
    config.model.supports_tools = true;
    config.model.connect_timeout_ms = 2_000;
    config.model.request_timeout_ms = 5_000;
    config.model.stream_idle_timeout_ms = 5_000;
    config.model.max_retries = 0;
    config.permissions.access_mode = AccessMode::FullAccess;
    config.multi_agent.enabled = true;
    config.multi_agent.mode = MultiAgentMode::ExplicitRequestOnly;
    config.multi_agent.max_concurrent_agents = 2;
    config.multi_agent.max_concurrent_model_requests = 1;
    let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
    store
        .project_repo()
        .upsert_project(
            workspace.project_id,
            &workspace.root,
            "detached-no-goal-agent-test",
            "none",
        )
        .await
        .expect("project");
    let session_service = crate::session::SessionService::new(store.clone());
    let root_session = session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("detached integration without goal".to_string()),
                cwd: root.clone(),
                model: config.model.model.clone(),
                base_url: base_url.clone(),
                access_mode: AccessMode::FullAccess,
            },
            workspace.clone(),
        )
        .await
        .expect("root session");
    let agent_runtime = Arc::new(AgentRuntime::new(store.clone(), session_service.clone()));
    let tool_services = ToolServices {
        edit_safety: crate::edit::EditSafety::default(),
        formatter: crate::edit::Formatter::new(config.format.clone()),
        change_tracker: crate::edit::ChangeTracker::default(),
        store: store.clone(),
        storage_paths: storage_paths.clone(),
        truncator: ToolTruncator,
        mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
        skills: crate::skill::SkillsService::new(),
    };
    let registry = ToolRegistry::core_agent_for_config(&config);
    let script = Arc::new(DetachedGoalScriptState::default());
    let agent_loop = AgentLoop::new(
        Arc::new(DetachedGoalScriptClient {
            state: Arc::clone(&script),
            complete_goal_after_child: false,
        }),
        registry,
        store.clone(),
        PromptBuilder,
        tool_services,
    );
    let run_service = Arc::new(RunService::new(
        store.clone(),
        config,
        workspace,
        session_service,
        agent_loop,
        SessionRuntimeEventHub::new(64),
        agent_runtime.clone(),
    ));
    agent_runtime
        .bind_run_service(Arc::downgrade(&run_service))
        .expect("bind runtime");

    let shared_confirmation = SharedConfirmationPrompt::new(AllowPrompt);
    let mut prompt = shared_confirmation.clone();
    let mut renderer = AgentEventRenderer;
    let summary = admitted_turn(
        tokio::time::timeout(
            Duration::from_secs(5),
            run_service.execute(
                AppCommand::Run(RunRequest {
                    prompt:
                        "Delegate the detached work and integrate the child result before replying."
                            .to_string(),
                    session_id: Some(root_session.session.id),
                    continue_last: false,
                    title: None,
                    cwd: root,
                    config: crate::app::RunConfigInput::Layered {
                        model: "scripted".to_string(),
                        base_url,
                        config_override: None,
                    },
                    output_mode: OutputMode::Human,
                    show_reasoning_summary: false,
                    prompt_dispatch: None,
                    editor_context: None,
                    review_request: None,
                    image_paths: Vec::new(),
                    run_control: RunControl::new(),
                    session_access_mode_adoption: None,
                    agent_confirmation: Some(shared_confirmation),
                    agent_context: None,
                }),
                &mut renderer,
                &mut prompt,
            ),
        )
        .await
        .expect("bounded goal-less continuation")
        .expect("goal-less detached run"),
    );

    assert_eq!(
        summary.status(),
        SessionStatus::Completed,
        "summary={summary:#?}; root_calls={}; child_calls={}; child_finished={}; saw_child_result={}",
        script.root_calls.load(Ordering::SeqCst),
        script.child_calls.load(Ordering::SeqCst),
        script.child_finished.load(Ordering::SeqCst),
        script.continuation_saw_child_result.load(Ordering::SeqCst),
    );
    assert_eq!(
        script.root_calls.load(Ordering::SeqCst),
        2,
        "a final response settles the root turn without an implicit OwnerResume"
    );
    assert!(!script.continuation_saw_child_result.load(Ordering::SeqCst));
    tokio::time::timeout(
        Duration::from_secs(5),
        agent_runtime.wait_for_tree_quiescence(summary.session_id()),
    )
    .await
    .expect("bounded detached child completion")
    .expect("detached tree quiescence");
    assert_eq!(script.child_calls.load(Ordering::SeqCst), 1);
    assert!(script.child_finished.load(Ordering::SeqCst));
    assert_eq!(script.root_calls.load(Ordering::SeqCst), 2);
    assert!(
        store
            .session_repo()
            .get_thread_goal(summary.session_id())
            .await
            .expect("goal read")
            .is_none(),
        "ordinary delegated work must not synthesize a ThreadGoal"
    );
    let root_terminals = store
        .protocol_event_store()
        .list_runtime_events_for_session(summary.session_id())
        .expect("root runtime events")
        .into_iter()
        .filter(|event| event.is_terminal())
        .collect::<Vec<_>>();
    assert_eq!(
        root_terminals.len(),
        1,
        "late child completion must not rewrite or extend the settled root turn"
    );
    assert!(root_terminals.iter().all(|event| matches!(
        event.terminal_outcome(),
        Some(TurnTerminalOutcome::Completed { .. })
    )));
    let canonical_history = store
        .protocol_event_store()
        .list_history_items_for_session(summary.session_id())
        .expect("canonical root history");
    assert!(!canonical_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::InterAgentCommunication { communication }
            if communication.content.contains("Message Type: FINAL_ANSWER")
                && communication.content.contains(DETACHED_CHILD_RESULT)
    )));
    assert!(
        store
            .session_repo()
            .has_pending_agent_mailbox_messages(summary.session_id())
            .expect("pending direct-child result"),
        "the late direct-child FINAL remains queued for the next explicit root turn"
    );
    assert!(
        !store
            .session_repo()
            .has_fresh_run_admission(summary.session_id())
            .await
            .expect("released continuation admission")
    );
    provider_server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_terminal_is_not_recalled_or_rewritten_by_late_child_interrupt() {
    let (base_url, provider_server) = start_probe_provider().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");
    let storage_paths = StoragePaths {
        data_dir: root.join(".moyai-data"),
        database_path: root.join(".moyai-data/moyai.sqlite3"),
        truncation_dir: root.join(".moyai-data/truncation"),
    };
    let sqlite = SqliteStore::open(&storage_paths).expect("store");
    sqlite.migrate().expect("migrate");
    let store = StoreBundle::new(sqlite);
    let mut config = ResolvedConfig::default();
    config.model.model = "scripted".to_string();
    config.model.base_url = base_url.clone();
    config.model.provider_api_mode = ProviderApiMode::ChatCompletions;
    config.model.provider_metadata_mode = ProviderMetadataMode::OpenAiCompatibleOnly;
    config.model.supports_tools = true;
    config.model.connect_timeout_ms = 2_000;
    config.model.request_timeout_ms = 5_000;
    config.model.stream_idle_timeout_ms = 5_000;
    config.model.max_retries = 0;
    config.permissions.access_mode = AccessMode::FullAccess;
    config.multi_agent.enabled = true;
    config.multi_agent.mode = MultiAgentMode::ExplicitRequestOnly;
    config.multi_agent.max_concurrent_agents = 2;
    config.multi_agent.max_concurrent_model_requests = 1;
    let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
    store
        .project_repo()
        .upsert_project(
            workspace.project_id,
            &workspace.root,
            "interrupted-detached-agent-test",
            "none",
        )
        .await
        .expect("project");
    let session_service = crate::session::SessionService::new(store.clone());
    let root_session = session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("interrupted detached integration".to_string()),
                cwd: root.clone(),
                model: config.model.model.clone(),
                base_url: base_url.clone(),
                access_mode: AccessMode::FullAccess,
            },
            workspace.clone(),
        )
        .await
        .expect("root session");
    let agent_runtime = Arc::new(AgentRuntime::new(store.clone(), session_service.clone()));
    let tool_services = ToolServices {
        edit_safety: crate::edit::EditSafety::default(),
        formatter: crate::edit::Formatter::new(config.format.clone()),
        change_tracker: crate::edit::ChangeTracker::default(),
        store: store.clone(),
        storage_paths: storage_paths.clone(),
        truncator: ToolTruncator,
        mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
        skills: crate::skill::SkillsService::new(),
    };
    let registry = ToolRegistry::core_agent_for_config(&config);
    let script = Arc::new(DetachedGoalScriptState::default());
    script
        .child_waits_for_interrupt
        .store(true, Ordering::SeqCst);
    let agent_loop = AgentLoop::new(
        Arc::new(DetachedGoalScriptClient {
            state: Arc::clone(&script),
            complete_goal_after_child: false,
        }),
        registry,
        store.clone(),
        PromptBuilder,
        tool_services,
    );
    let run_service = Arc::new(RunService::new(
        store.clone(),
        config,
        workspace,
        session_service,
        agent_loop,
        SessionRuntimeEventHub::new(64),
        agent_runtime.clone(),
    ));
    agent_runtime
        .bind_run_service(Arc::downgrade(&run_service))
        .expect("bind runtime");

    let shared_confirmation = SharedConfirmationPrompt::new(AllowPrompt);
    let mut prompt = shared_confirmation.clone();
    let mut renderer = AgentEventRenderer;
    let run = run_service.execute(
        AppCommand::Run(RunRequest {
            prompt: "Delegate the detached work, but preserve the same final if it is interrupted."
                .to_string(),
            session_id: Some(root_session.session.id),
            continue_last: false,
            title: None,
            cwd: root,
            config: crate::app::RunConfigInput::Layered {
                model: "scripted".to_string(),
                base_url,
                config_override: None,
            },
            output_mode: OutputMode::Human,
            show_reasoning_summary: false,
            prompt_dispatch: None,
            editor_context: None,
            review_request: None,
            image_paths: Vec::new(),
            run_control: RunControl::new(),
            session_access_mode_adoption: None,
            agent_confirmation: Some(shared_confirmation),
            agent_context: None,
        }),
        &mut renderer,
        &mut prompt,
    );
    let interrupt = async {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if script.first_root_turn_finished.load(Ordering::SeqCst)
                && script.child_calls.load(Ordering::SeqCst) > 0
            {
                let tree = agent_runtime
                    .trees
                    .lock()
                    .expect("tree registry")
                    .get(&root_session.session.id)
                    .cloned()
                    .expect("live root tree");
                let child = AgentPath::try_from("/root/detached").expect("child path");
                if tree
                    .control
                    .list_agents(Some(&child))
                    .expect("child snapshot")
                    .into_iter()
                    .any(|agent| agent.path == child && agent.is_active)
                {
                    tree.control
                        .cancel_agent(&child)
                        .expect("target-only child interruption");
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "detached child did not become interruptible"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let (result, ()) = tokio::join!(tokio::time::timeout(Duration::from_secs(8), run), interrupt);
    let summary = admitted_turn(
        result
            .expect("bounded interrupted child run")
            .expect("interrupted child root run"),
    );

    assert_eq!(summary.status(), SessionStatus::Completed);
    assert_eq!(script.root_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        summary.metrics().model_request_count,
        2,
        "the root spawn response and early final are the only provider requests"
    );
    assert_eq!(script.child_calls.load(Ordering::SeqCst), 1);
    provider_server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_goal_continuation_uses_explicit_wait_agent_for_detached_child() {
    let (base_url, provider_server) = start_probe_provider().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");
    let storage_paths = StoragePaths {
        data_dir: root.join(".moyai-data"),
        database_path: root.join(".moyai-data/moyai.sqlite3"),
        truncation_dir: root.join(".moyai-data/truncation"),
    };
    let sqlite = SqliteStore::open(&storage_paths).expect("store");
    sqlite.migrate().expect("migrate");
    let store = StoreBundle::new(sqlite);
    let mut config = ResolvedConfig::default();
    config.model.model = "scripted".to_string();
    config.model.base_url = base_url.clone();
    config.model.provider_api_mode = ProviderApiMode::ChatCompletions;
    config.model.provider_metadata_mode = ProviderMetadataMode::OpenAiCompatibleOnly;
    config.model.supports_tools = true;
    config.model.connect_timeout_ms = 2_000;
    config.model.request_timeout_ms = 5_000;
    config.model.stream_idle_timeout_ms = 5_000;
    config.model.max_retries = 0;
    config.permissions.access_mode = AccessMode::FullAccess;
    config.multi_agent.enabled = true;
    config.multi_agent.mode = MultiAgentMode::ExplicitRequestOnly;
    config.multi_agent.max_concurrent_agents = 2;
    config.multi_agent.max_concurrent_model_requests = 2;
    let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
    store
        .project_repo()
        .upsert_project(
            workspace.project_id,
            &workspace.root,
            "detached-goal-agent-test",
            "none",
        )
        .await
        .expect("project");
    let session_service = crate::session::SessionService::new(store.clone());
    let root_session = session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("detached goal integration".to_string()),
                cwd: root.clone(),
                model: config.model.model.clone(),
                base_url: base_url.clone(),
                access_mode: AccessMode::FullAccess,
            },
            workspace.clone(),
        )
        .await
        .expect("root session");
    let agent_runtime = Arc::new(AgentRuntime::new(store.clone(), session_service.clone()));
    let tool_services = ToolServices {
        edit_safety: crate::edit::EditSafety::default(),
        formatter: crate::edit::Formatter::new(config.format.clone()),
        change_tracker: crate::edit::ChangeTracker::default(),
        store: store.clone(),
        storage_paths: storage_paths.clone(),
        truncator: ToolTruncator,
        mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
        skills: crate::skill::SkillsService::new(),
    };
    let registry = ToolRegistry::core_agent_for_config(&config);
    let script = Arc::new(DetachedGoalScriptState::default());
    let agent_loop = AgentLoop::new(
        Arc::new(DetachedGoalScriptClient {
            state: Arc::clone(&script),
            complete_goal_after_child: true,
        }),
        registry,
        store.clone(),
        PromptBuilder,
        tool_services,
    );
    let run_service = Arc::new(RunService::new(
        store.clone(),
        config,
        workspace,
        session_service,
        agent_loop,
        SessionRuntimeEventHub::new(64),
        agent_runtime.clone(),
    ));
    agent_runtime
        .bind_run_service(Arc::downgrade(&run_service))
        .expect("bind runtime");

    let shared_confirmation = SharedConfirmationPrompt::new(AllowPrompt);
    let mut prompt = shared_confirmation.clone();
    let mut renderer = AgentEventRenderer;
    let summary = admitted_turn(
        tokio::time::timeout(
            Duration::from_secs(5),
            run_service.execute(
                AppCommand::Run(RunRequest {
                    prompt: "/goal Integrate the detached child result before completion"
                        .to_string(),
                    session_id: Some(root_session.session.id),
                    continue_last: false,
                    title: None,
                    cwd: root,
                    config: crate::app::RunConfigInput::Layered {
                        model: "scripted".to_string(),
                        base_url,
                        config_override: None,
                    },
                    output_mode: OutputMode::Human,
                    show_reasoning_summary: false,
                    prompt_dispatch: None,
                    editor_context: None,
                    review_request: None,
                    image_paths: Vec::new(),
                    run_control: RunControl::new(),
                    session_access_mode_adoption: None,
                    agent_confirmation: Some(shared_confirmation),
                    agent_context: None,
                }),
                &mut renderer,
                &mut prompt,
            ),
        )
        .await
        .expect("bounded goal continuation")
        .expect("goal run"),
    );

    let canonical_history = store
        .protocol_event_store()
        .list_history_items_for_session(summary.session_id())
        .expect("canonical root history");
    let durable_communications = canonical_history
        .iter()
        .filter_map(|item| match &item.payload {
            HistoryItemPayload::InterAgentCommunication { communication } => {
                Some(communication.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary.status(),
        SessionStatus::Completed,
        "summary={summary:#?}; root_calls={}; child_calls={}; child_finished={}; saw_child_result={}; durable_communications={durable_communications:#?}",
        script.root_calls.load(Ordering::SeqCst),
        script.child_calls.load(Ordering::SeqCst),
        script.child_finished.load(Ordering::SeqCst),
        script.continuation_saw_child_result.load(Ordering::SeqCst),
    );
    assert_eq!(script.root_calls.load(Ordering::SeqCst), 5);
    assert_eq!(script.child_calls.load(Ordering::SeqCst), 1);
    assert!(script.child_finished.load(Ordering::SeqCst));
    assert!(script.continuation_saw_child_result.load(Ordering::SeqCst));
    assert!(canonical_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::ToolCall { tool_name, .. } if tool_name == "wait_agent"
    )));
    assert_eq!(
        store
            .session_repo()
            .get_thread_goal(summary.session_id())
            .await
            .expect("goal read")
            .expect("goal")
            .status,
        ThreadGoalStatus::Complete
    );
    let edges = store
        .session_repo()
        .list_session_spawn_edges(summary.session_id())
        .await
        .expect("detached child edge");
    assert_eq!(edges.len(), 1);
    let child_history = store
        .protocol_event_store()
        .list_history_items_for_session(edges[0].child_session_id)
        .expect("fork-none child history");
    assert!(
        !child_history
            .iter()
            .any(|item| matches!(&item.payload, HistoryItemPayload::UserTurn { .. }))
    );
    assert!(child_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::InterAgentCommunication { communication }
            if communication.trigger_turn
                && communication.content.contains("Message Type: NEW_TASK")
                && communication.content.contains(DETACHED_CHILD_ASSIGNMENT)
    )));
    provider_server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_nested_owner_explicitly_waits_and_keeps_tool_parity() {
    let (base_url, provider_server) = start_probe_provider().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");
    let storage_paths = StoragePaths {
        data_dir: root.join(".moyai-data"),
        database_path: root.join(".moyai-data/moyai.sqlite3"),
        truncation_dir: root.join(".moyai-data/truncation"),
    };
    let sqlite = SqliteStore::open(&storage_paths).expect("store");
    sqlite.migrate().expect("migrate");
    let store = StoreBundle::new(sqlite);

    let mut config = ResolvedConfig::default();
    config.model.model = "scripted".to_string();
    config.model.base_url = base_url.clone();
    config.model.provider_api_mode = ProviderApiMode::ChatCompletions;
    config.model.provider_metadata_mode = ProviderMetadataMode::OpenAiCompatibleOnly;
    config.model.supports_tools = true;
    config.model.supports_reasoning = true;
    config.model.supports_images = false;
    config.model.parallel_tool_calls = false;
    config.model.connect_timeout_ms = 2_000;
    config.model.request_timeout_ms = 5_000;
    config.model.stream_idle_timeout_ms = 5_000;
    config.model.max_retries = 0;
    config.permissions.access_mode = AccessMode::FullAccess;
    config.multi_agent.enabled = true;
    config.multi_agent.mode = MultiAgentMode::Proactive;
    config.multi_agent.max_concurrent_agents = 3;
    config.multi_agent.max_concurrent_model_requests = 2;

    let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");
    store
        .project_repo()
        .upsert_project(
            workspace.project_id,
            &workspace.root,
            "agent-runtime-test",
            "none",
        )
        .await
        .expect("project");
    let session_service = crate::session::SessionService::new(store.clone());
    let root_session = session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("multi-agent integration".to_string()),
                cwd: root.clone(),
                model: "scripted".to_string(),
                base_url: base_url.clone(),
                access_mode: AccessMode::FullAccess,
            },
            workspace.clone(),
        )
        .await
        .expect("precreate root session");
    let source_activity = project_sub_agent_activity(
        root_session.session.id,
        TurnId::new(),
        0,
        "preexisting_activity".to_string(),
        root_session.session.id,
        "/root/previous".to_string(),
        SubAgentActivityKind::Interacted,
    );
    store
        .protocol_event_store()
        .seed_event_bundle_for_test(
            &source_activity.runtime_event,
            source_activity.history_item.as_ref(),
            source_activity.turn_item.as_ref(),
        )
        .expect("seed source activity");
    let agent_runtime = Arc::new(AgentRuntime::new(store.clone(), session_service.clone()));
    let tool_services = ToolServices {
        edit_safety: crate::edit::EditSafety::default(),
        formatter: crate::edit::Formatter::new(config.format.clone()),
        change_tracker: crate::edit::ChangeTracker::default(),
        store: store.clone(),
        storage_paths: storage_paths.clone(),
        truncator: ToolTruncator,
        mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
        skills: crate::skill::SkillsService::new(),
    };
    let registry = ToolRegistry::core_agent_for_config(&config);
    let script = Arc::new(AgentScriptState::default());
    script.child_spawns_grandchild.store(true, Ordering::SeqCst);
    script
        .root_plans_before_spawn_with_sibling
        .store(true, Ordering::SeqCst);
    let llm = Arc::new(AgentScriptClient {
        state: Arc::clone(&script),
    });
    let agent_loop = AgentLoop::new(llm, registry, store.clone(), PromptBuilder, tool_services)
        .with_model_request_concurrency(config.multi_agent.max_concurrent_model_requests);
    let run_service = Arc::new(RunService::new(
        store.clone(),
        config.clone(),
        workspace.clone(),
        session_service.clone(),
        agent_loop,
        SessionRuntimeEventHub::new(128),
        agent_runtime.clone(),
    ));
    agent_runtime
        .bind_run_service(Arc::downgrade(&run_service))
        .expect("bind runtime");

    let shared_confirmation = SharedConfirmationPrompt::new(AllowPrompt);
    let mut execute_prompt = shared_confirmation.clone();
    let mut renderer = AgentEventRenderer;
    let summary = admitted_turn(
        run_service
            .execute(
                AppCommand::Run(RunRequest {
                    prompt: ROOT_TASK.to_string(),
                    session_id: Some(root_session.session.id),
                    continue_last: false,
                    title: None,
                    cwd: root.clone(),
                    config: crate::app::RunConfigInput::Layered {
                        model: "scripted".to_string(),
                        base_url: base_url.clone(),
                        config_override: None,
                    },
                    output_mode: OutputMode::Human,
                    show_reasoning_summary: false,
                    prompt_dispatch: None,
                    editor_context: None,
                    review_request: None,
                    image_paths: Vec::new(),
                    run_control: RunControl::new(),
                    session_access_mode_adoption: None,
                    agent_confirmation: Some(shared_confirmation),
                    agent_context: None,
                }),
                &mut renderer,
                &mut execute_prompt,
            )
            .await
            .expect("root run"),
    );

    assert_eq!(summary.status(), SessionStatus::Completed);
    assert_eq!(summary.session_id(), root_session.session.id);
    assert_eq!(script.root_calls.load(Ordering::SeqCst), 4);
    assert_eq!(script.child_calls.load(Ordering::SeqCst), 5);
    assert_eq!(script.grandchild_calls.load(Ordering::SeqCst), 2);
    assert!(script.child_saw_grandchild_result.load(Ordering::SeqCst));
    assert!(script.grandchild_request_started.load(Ordering::SeqCst));
    assert_eq!(
        session_service
            .get_session(summary.session_id())
            .await
            .expect("root session")
            .status,
        SessionStatus::Completed
    );

    let edges = store
        .session_repo()
        .list_session_spawn_edges(summary.session_id())
        .await
        .expect("spawn edges");
    assert_eq!(edges.len(), 2);
    let edge = edges
        .iter()
        .find(|edge| edge.agent_path == "/root/worker")
        .expect("worker edge");
    assert_eq!(edge.root_session_id, summary.session_id());
    assert_eq!(edge.parent_session_id, summary.session_id());
    assert_eq!(edge.agent_path, "/root/worker");
    assert_eq!(edge.task_name, "worker");
    let child_session_id = edge.child_session_id;
    let grandchild_edge = edges
        .iter()
        .find(|edge| edge.agent_path == "/root/worker/reviewer")
        .expect("grandchild edge");
    assert_eq!(grandchild_edge.root_session_id, summary.session_id());
    assert_eq!(grandchild_edge.parent_session_id, child_session_id);
    assert_eq!(grandchild_edge.task_name, "reviewer");
    let grandchild_session_id = grandchild_edge.child_session_id;

    let visible_sessions = session_service
        .list_sessions(workspace.project_id, 20)
        .await
        .expect("normal session list");
    assert_eq!(visible_sessions.len(), 1);
    assert_eq!(visible_sessions[0].id, summary.session_id());
    assert_eq!(
        session_service
            .get_session(child_session_id)
            .await
            .expect("explicit child session")
            .status,
        SessionStatus::Completed
    );
    assert_eq!(
        session_service
            .get_session(grandchild_session_id)
            .await
            .expect("explicit grandchild session")
            .status,
        SessionStatus::Completed
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let child_activity = loop {
        if let Some(activity) = agent_runtime
            .activity_records(summary.session_id())
            .into_iter()
            .find(|activity| activity.agent_path == "/root/worker")
            .filter(|activity| matches!(activity.status, AgentStatus::Completed(_)))
        {
            break activity;
        }
        assert!(
            Instant::now() < deadline,
            "child activity did not become completed before the bounded deadline"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(child_activity.result_preview.contains(CHILD_RESULT));
    assert_eq!(
        std::fs::read_to_string(root.join(CHILD_ARTIFACT).as_std_path())
            .expect("child artifact")
            .replace("\r\n", "\n"),
        "child durable artifact\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join(GRANDCHILD_ARTIFACT).as_std_path())
            .expect("grandchild artifact")
            .replace("\r\n", "\n"),
        "grandchild durable artifact\n"
    );

    let root_history = store
        .protocol_event_store()
        .list_history_items_for_session(summary.session_id())
        .expect("root history");
    assert!(root_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::SubAgentActivity { activity_id, .. }
            if activity_id == "preexisting_activity"
    )));
    assert!(root_history.iter().all(|item| !matches!(
        &item.payload,
        HistoryItemPayload::SubAgentActivity { agent_path, .. }
            if agent_path == "/root/worker/reviewer"
    )));
    let child_history = store
        .protocol_event_store()
        .list_history_items_for_session(child_session_id)
        .expect("child history");
    assert!(child_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::SubAgentActivity { agent_path, .. }
            if agent_path == "/root/worker/reviewer"
    )));
    assert!(root_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::ToolCall {
            tool_name,
            ..
        } if tool_name == "spawn_agent"
    )));
    let root_list_call_id = root_history
        .iter()
        .find_map(|item| match &item.payload {
            HistoryItemPayload::ToolCall {
                call_id, tool_name, ..
            } if tool_name == "list" => Some(call_id),
            _ => None,
        })
        .expect("root list sibling call");
    assert!(root_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::ToolOutput {
            call_id,
            status: crate::protocol::ToolLifecycleStatus::Completed,
            ..
        } if call_id == root_list_call_id
    )));
    let child_finals_to_root = root_history
        .iter()
        .filter(|item| {
            matches!(
                &item.payload,
                HistoryItemPayload::InterAgentCommunication { communication }
                    if communication.author == "/root/worker"
                        && communication.recipient == "/root"
                        && communication.content.contains("Message Type: FINAL_ANSWER")
                        && !communication.trigger_turn
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        child_finals_to_root.len(),
        1,
        "the immediate owner must emit exactly one durable FINAL to root"
    );
    assert!(matches!(
        &child_finals_to_root[0].payload,
        HistoryItemPayload::InterAgentCommunication { communication }
            if communication.content.contains(CHILD_RESULT)
    ));
    assert!(!root_history.iter().any(|item| {
        matches!(
            &item.payload,
            HistoryItemPayload::InterAgentCommunication { communication }
                if communication.author == "/root/worker/reviewer"
                    && communication.content.contains(GRANDCHILD_RESULT)
        )
    }));

    let child_history = store
        .protocol_event_store()
        .list_history_items_for_session(child_session_id)
        .expect("child history");
    let child_response = child_history
        .iter()
        .find(|item| {
            matches!(
                &item.payload,
                HistoryItemPayload::AssistantMessage { content, .. }
                    if content_contains(content, CHILD_RESULT)
            )
        })
        .expect("child model response after explicit wait");
    let grandchild_finals_for_child = child_history
        .iter()
        .filter(|item| {
            matches!(
                &item.payload,
                HistoryItemPayload::InterAgentCommunication { communication }
                    if communication.author == "/root/worker/reviewer"
                        && communication.recipient == "/root/worker"
                        && communication.content.contains("Message Type: FINAL_ANSWER")
                        && communication.content.contains(GRANDCHILD_RESULT)
                        && !communication.trigger_turn
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        grandchild_finals_for_child.len(),
        1,
        "the grandchild must hand its result to the immediate owner exactly once"
    );
    let grandchild_final_for_child = grandchild_finals_for_child[0];
    let child_turn_id = match (&grandchild_final_for_child.scope, &child_response.scope) {
        (
            HistoryScope::Turn {
                turn_id: delivered_result_turn,
            },
            HistoryScope::Turn {
                turn_id: child_response_turn,
            },
        ) if delivered_result_turn == child_response_turn => *child_response_turn,
        scopes => panic!(
            "wait_agent must deliver the grandchild result into the same child turn, got {scopes:?}"
        ),
    };
    let child_terminals = store
        .protocol_event_store()
        .list_runtime_events_for_session(child_session_id)
        .expect("child runtime events")
        .into_iter()
        .filter(|event| event.is_terminal())
        .collect::<Vec<_>>();
    assert_eq!(
        child_terminals.len(),
        1,
        "explicit wait keeps the child work in one durable turn"
    );
    assert_eq!(child_terminals[0].turn_id, child_turn_id);
    assert!(
        store
            .session_repo()
            .agent_terminal_effects(child_session_id, child_turn_id)
            .expect("child terminal effects")
            .deferred
            .is_none()
    );
    assert!(child_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::ToolCall { tool_name, .. } if tool_name == "wait_agent"
    )));
    assert!(child_history.iter().any(|item| {
        matches!(
            &item.payload,
            HistoryItemPayload::UserTurn { content, .. }
                if content_contains(content, ROOT_TASK)
        )
    }));
    assert!(!child_history.iter().any(|item| {
        matches!(
            &item.payload,
            HistoryItemPayload::AssistantMessage { content, .. }
                if content_contains(content, ROOT_PLAN)
        )
    }));
    assert!(child_history.iter().any(|item| {
        matches!(
            &item.payload,
            HistoryItemPayload::InterAgentCommunication { communication }
                if communication.author == "/root"
                    && communication.recipient == "/root/worker"
                    && communication.trigger_turn
                    && communication.content.contains("Message Type: NEW_TASK")
                    && communication.content.contains(CHILD_ASSIGNMENT)
        )
    }));
    assert!(child_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::ToolCall { tool_name, .. } if tool_name == "list"
    )));
    let child_patch_call_id = child_history
        .iter()
        .find_map(|item| match &item.payload {
            HistoryItemPayload::ToolCall {
                call_id, tool_name, ..
            } if tool_name == "apply_patch" => Some(*call_id),
            _ => None,
        })
        .expect("child apply_patch call");
    assert!(child_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::ToolOutput {
            call_id,
            status: crate::protocol::ToolLifecycleStatus::Completed,
            ..
        } if *call_id == child_patch_call_id
    )));
    assert!(child_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::ToolCall { tool_name, .. } if tool_name == "spawn_agent"
    )));
    assert!(child_history.iter().any(|item| {
        matches!(
            &item.payload,
            HistoryItemPayload::InterAgentCommunication { communication }
                if communication.author == "/root/worker/reviewer"
                    && communication.recipient == "/root/worker"
                    && communication.content.contains("Message Type: FINAL_ANSWER")
                    && communication.content.contains(GRANDCHILD_RESULT)
                    && !communication.trigger_turn
        )
    }));
    assert!(child_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::ToolOutput {
            status: crate::protocol::ToolLifecycleStatus::Completed,
            ..
        }
    )));
    assert!(child_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::SubAgentActivity { agent_path, .. }
            if agent_path == "/root/worker/reviewer"
    )));
    let grandchild_history = store
        .protocol_event_store()
        .list_history_items_for_session(grandchild_session_id)
        .expect("grandchild history");
    assert!(grandchild_history.iter().any(|item| {
        matches!(
            &item.payload,
            HistoryItemPayload::InterAgentCommunication { communication }
                if communication.author == "/root/worker"
                    && communication.recipient == "/root/worker/reviewer"
                    && communication.trigger_turn
                    && communication.content.contains(GRANDCHILD_ASSIGNMENT)
        )
    }));
    let grandchild_patch_call_id = grandchild_history
        .iter()
        .find_map(|item| match &item.payload {
            HistoryItemPayload::ToolCall {
                call_id, tool_name, ..
            } if tool_name == "apply_patch" => Some(*call_id),
            _ => None,
        })
        .expect("grandchild apply_patch call");
    assert!(grandchild_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::ToolOutput {
            call_id,
            status: crate::protocol::ToolLifecycleStatus::Completed,
            ..
        } if *call_id == grandchild_patch_call_id
    )));

    let requests = script.requests.lock().expect("request capture mutex");
    let root_requests = requests
        .iter()
        .filter(|request| {
            matches!(
                request.messages.first(),
                Some(ModelMessage::Developer { content })
                    if content.starts_with("You are `/root`, the primary agent")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(root_requests.len(), 4);
    for request in &root_requests {
        let names = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        for required in [
            "list",
            "apply_patch",
            "spawn_agent",
            "wait_agent",
            "update_plan",
        ] {
            assert!(
                names.contains(&required),
                "root request missing ordinary/collaboration tool {required}: {names:?}"
            );
        }
    }
    let child_requests = requests
        .iter()
        .filter(|request| {
            request.messages.iter().any(|message| {
                matches!(message, ModelMessage::Agent { content }
                    if content.contains("Message Type: NEW_TASK")
                        && content.contains(CHILD_ASSIGNMENT))
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(child_requests.len(), 5);
    let child_request = child_requests[0];
    assert!(!child_request.system_prompt.contains("## Sub-agent"));
    assert!(matches!(
        child_request.messages.first(),
        Some(ModelMessage::Developer { content })
            if content.starts_with("You are an agent in a team of agents")
                && content.contains("response in the final channel")
                && content.contains("immediately delivered back to your parent agent")
                && content.contains("Message Type: NEW_TASK | MESSAGE | FINAL_ANSWER")
                && content.contains("spawn their own sub-agents")
                && content.contains("same set of tools")
    ));
    for child_request in child_requests {
        let tool_names = child_request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        for required in [
            "list",
            "apply_patch",
            "spawn_agent",
            "send_message",
            "followup_task",
            "wait_agent",
            "interrupt_agent",
            "list_agents",
        ] {
            assert!(
                tool_names.contains(&required),
                "child request missing multi-agent tool {required}"
            );
        }
    }
    let grandchild_requests = requests
        .iter()
        .filter(|request| {
            request.messages.iter().any(|message| {
                matches!(message, ModelMessage::Agent { content }
                    if content.contains("Message Type: NEW_TASK")
                        && content.contains(GRANDCHILD_ASSIGNMENT))
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(grandchild_requests.len(), 2);
    for grandchild_request in grandchild_requests {
        let grandchild_tools = grandchild_request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        for required in ["list", "apply_patch", "spawn_agent", "wait_agent"] {
            assert!(
                grandchild_tools.contains(&required),
                "grandchild request missing tool {required}"
            );
        }
    }

    provider_server.abort();
}

fn content_contains(content: &[ContentPart], expected: &str) -> bool {
    content
        .iter()
        .any(|part| matches!(part, ContentPart::Text { text } if text.contains(expected)))
}

async fn start_probe_provider() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route(
            "/v1/models",
            get(|| async {
                Json(json!({
                    "data": [{
                        "id": "scripted",
                        "context_window": 32_768,
                        "max_output_tokens": 4_096,
                        "max_parallel_predictions": 1,
                        "capabilities": {
                            "tools": true,
                            "reasoning": true,
                            "vision": false
                        }
                    }]
                }))
            }),
        )
        .route(
            "/v1/chat/completions",
            post(|| async {
                Json(json!({
                    "id": "probe-response",
                    "choices": [{
                        "index": 0,
                        "finish_reason": "tool_calls",
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "probe-call",
                                "type": "function",
                                "function": {
                                    "name": "echo_word",
                                    "arguments": "{\"word\":\"ping\"}"
                                }
                            }]
                        }
                    }]
                }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind probe provider");
    let address = listener.local_addr().expect("probe provider address");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("probe provider server");
    });
    (format!("http://{address}"), handle)
}
