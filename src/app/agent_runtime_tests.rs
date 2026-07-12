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
use crate::cli::{ConfirmationPrompt, OutputMode};
use crate::config::{AccessMode, MultiAgentMode, ProviderMetadataMode, ResolvedConfig};
use crate::error::{CliPromptError, LlmError};
use crate::llm::{
    ChatRequest, LlmClient, LlmEvent, LlmEventSink, LlmResponseSummary, ModelMessage,
};
use crate::protocol::{
    ContentPart, HistoryItem, HistoryItemId, HistoryItemPayload, ProtocolEventStore,
    SubAgentActivityKind, TurnId, project_protocol_run_event, project_sub_agent_activity,
};
use crate::runtime::{AgentStatus, SessionRuntimeEventHub, SystemClock};
use crate::session::{
    FinishReason, MessageId, MessageRole, ProjectRepository, RunEvent, SessionSelector,
    SessionStartRequest, SessionStatus, ThreadGoalStatus, TokenUsage,
};
use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
use crate::tool::ToolName;
use crate::tool::context::ToolServices;
use crate::tool::registry::ToolRegistry;
use crate::tool::truncate::ToolTruncator;
use crate::workspace::WorkspaceDiscovery;

const ROOT_TASK: &str =
    "Delegate the bounded investigation to a sub-agent, wait, and integrate it.";
const ROOT_PLAN: &str = "I will delegate the bounded investigation now.";
const CHILD_ASSIGNMENT: &str = "Inspect the fixture and return the verified child result.";
const CHILD_RESULT: &str = "child verified result";
const ROOT_RESULT: &str = "integrated root result";
const DETACHED_CHILD_ASSIGNMENT: &str = "Complete the detached goal subtask.";
const DETACHED_CHILD_RESULT: &str = "detached child durable result";

#[derive(Default)]
struct AllowPrompt;

impl ConfirmationPrompt for AllowPrompt {
    fn confirm(
        &mut self,
        _request: &crate::tool::PermissionRequest,
    ) -> Result<bool, CliPromptError> {
        Ok(true)
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
async fn root_resume_refreshes_limits_and_permission_broker_without_dropping_rows() {
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
            config.clone(),
            original.clone(),
            None,
            CancellationToken::new(),
        )
        .expect("first root turn");
    let first_context_broker = first.context.confirmation_prompt();
    let first_gate = first.context.model_request_gate();
    let retained_tree = first.context.tree.clone();
    assert!(first_context_broker.shares_broker_with(&original));
    assert_eq!(first_gate.available_permits(), 2);
    drop(first);
    assert!(
        retained_tree
            .confirmation
            .lock()
            .expect("confirmation state")
            .is_none(),
        "a quiescent retained tree must release its permission broker"
    );

    let mut resumed_config = config;
    resumed_config.multi_agent.max_concurrent_agents = 1;
    resumed_config.multi_agent.max_concurrent_model_requests = 1;
    let resumed = runtime
        .begin_root(
            &session,
            resumed_config,
            replacement.clone(),
            None,
            CancellationToken::new(),
        )
        .expect("resumed root turn");
    let resumed_broker = resumed.context.confirmation_prompt();
    let resumed_gate = resumed.context.model_request_gate();
    assert!(!resumed_broker.shares_broker_with(&original));
    assert!(resumed_broker.shares_broker_with(&replacement));
    assert!(!Arc::ptr_eq(&first_gate, &resumed_gate));
    assert_eq!(resumed_gate.available_permits(), 1);
    assert_eq!(
        resumed
            .context
            .tree
            .control
            .snapshot()
            .expect("resumed tree")
            .max_concurrent_agents,
        1
    );
    drop(resumed);
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
    original_runtime
        .store
        .session_repo()
        .set_status_with_protocol_event(
            child_session.session.id,
            SessionStatus::Completed,
            &RunEvent::SessionCompleted {
                session_id: child_session.session.id,
                finish_reason: Some(FinishReason::Stop),
            },
            TurnId::new(),
            Some(0),
        )
        .await
        .expect("terminal child");

    let store = original_runtime.store.clone();
    drop(original_runtime);
    let resumed_runtime = Arc::new(AgentRuntime::new(
        store.clone(),
        crate::session::SessionService::new(store.clone()),
    ));
    let execution = resumed_runtime
        .begin_root(
            &root_session,
            config,
            SharedConfirmationPrompt::new(AllowPrompt),
            None,
            CancellationToken::new(),
        )
        .expect("rehydrated root");

    let restored = execution
        .context
        .list_agents(None)
        .expect("list restored agents");
    assert_eq!(restored.len(), 2);
    assert_eq!(restored[1].path.as_str(), "/root/research");
    assert_eq!(restored[1].session_id, child_session.session.id);
    assert!(matches!(restored[1].status, AgentStatus::Completed(None)));

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
    assert_eq!(
        store
            .session_repo()
            .list_session_spawn_edges(root_session.session.id)
            .await
            .expect("spawn edges")
            .len(),
        1
    );
    resumed_runtime.complete_root(
        execution,
        &Ok(RunSummary {
            session_id: root_session.session.id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: Some(FinishReason::Stop),
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
        false,
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
        let task = format!("durable task {task_name}");
        let result = format!("durable result {task_name}");
        let result_message_id = MessageId::new();
        protocol_store
            .append_history_item(&HistoryItem {
                id: HistoryItemId::new(),
                session_id: child.session.id,
                turn_id,
                sequence_no: 0,
                created_at_ms: SystemClock::now_ms(),
                payload: HistoryItemPayload::UserTurn {
                    message_id: None,
                    content: vec![ContentPart::Text { text: task.clone() }],
                    prompt_dispatch: None,
                    editor_context: None,
                    turn_context: None,
                },
            })
            .expect("durable child task");
        protocol_store
            .append_history_item(&HistoryItem {
                id: HistoryItemId::new(),
                session_id: child.session.id,
                turn_id,
                sequence_no: 1,
                created_at_ms: SystemClock::now_ms(),
                payload: HistoryItemPayload::Message {
                    message_id: Some(result_message_id),
                    role: MessageRole::Assistant,
                    content: vec![ContentPart::Text {
                        text: "durable result ".to_string(),
                    }],
                },
            })
            .expect("durable child result prefix");
        protocol_store
            .append_history_item(&HistoryItem {
                id: HistoryItemId::new(),
                session_id: child.session.id,
                turn_id,
                sequence_no: 2,
                created_at_ms: SystemClock::now_ms(),
                payload: HistoryItemPayload::Message {
                    message_id: Some(result_message_id),
                    role: MessageRole::Assistant,
                    content: vec![ContentPart::Text {
                        text: task_name.to_string(),
                    }],
                },
            })
            .expect("durable child result suffix");
        original_runtime
            .store
            .session_repo()
            .set_status_with_protocol_event(
                child.session.id,
                SessionStatus::Completed,
                &RunEvent::SessionCompleted {
                    session_id: child.session.id,
                    finish_reason: Some(FinishReason::Stop),
                },
                turn_id,
                Some(3),
            )
            .await
            .expect("terminal child");
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
        assert_eq!(record.result_preview, result);
        assert!(record.current_activity.is_empty());

        let mut running = session;
        running.status = SessionStatus::Running;
        assert!(matches!(
            durable_projection_status(&running, Some("still running".to_string())),
            AgentStatus::Running
        ));
    }
}

#[test]
fn failed_durable_child_prefers_latest_error_over_partial_assistant_text() {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let assistant_message_id = MessageId::new();
    let history = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 0,
            created_at_ms: SystemClock::now_ms(),
            payload: HistoryItemPayload::Message {
                message_id: Some(assistant_message_id),
                role: MessageRole::Assistant,
                content: vec![ContentPart::Text {
                    text: "partial assistant output".to_string(),
                }],
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: SystemClock::now_ms(),
            payload: HistoryItemPayload::Error {
                message_id: Some(assistant_message_id),
                message: "recoverable provider error".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: SystemClock::now_ms(),
            payload: HistoryItemPayload::Error {
                message_id: None,
                message: "final child failure".to_string(),
            },
        },
    ];

    assert_eq!(
        durable_child_result(SessionStatus::Failed, &history),
        Some("final child failure".to_string())
    );
    assert_eq!(
        durable_child_result(SessionStatus::Completed, &history),
        Some("partial assistant output".to_string()),
        "successful children retain the latest assistant result"
    );
}

#[tokio::test]
async fn sub_agent_spawn_is_rejected_before_session_or_tree_side_effects() {
    let (runtime, session, config) = direct_runtime_fixture("spawn-depth", 3).await;
    let execution = runtime
        .begin_root(
            &session,
            config.clone(),
            SharedConfirmationPrompt::new(AllowPrompt),
            None,
            CancellationToken::new(),
        )
        .expect("root execution");
    let tree = execution.context.tree.clone();
    let child_path = crate::runtime::AgentPath::root()
        .join("child")
        .expect("child path");
    let child_session_id = SessionId::new();
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
        config,
        workspace: session.workspace.clone(),
        live_config: None,
    };
    let agents_before = tree.control.snapshot().expect("tree before").agents.len();

    let error = child_context
        .spawn_agent(
            "grandchild",
            "must be rejected".to_string(),
            AgentForkTurns::All,
            "depth_check".to_string(),
        )
        .await
        .expect_err("sub-agent nesting must be rejected");

    assert!(error.contains("root → child"));
    assert_eq!(
        tree.control.snapshot().expect("tree after").agents.len(),
        agents_before
    );
    assert!(
        runtime
            .store
            .session_repo()
            .list_session_spawn_edges(session.session.id)
            .await
            .expect("spawn edges")
            .is_empty()
    );
    assert_eq!(
        runtime
            .session_service
            .list_sessions(session.session.project_id, 20)
            .await
            .expect("sessions")
            .len(),
        1
    );
    tree.control
        .enqueue_mail(AgentMailboxMessage::new(
            AgentPath::root(),
            child_context.path.clone(),
            "must not restart after durable terminalization",
            true,
        ))
        .expect("trigger mail");
    assert!(!child_context.tree_cancel_token().is_cancelled());
    child_context
        .cancel_for_durable_terminal()
        .expect("durable child terminal");
    assert!(child_lease.cancel_token().is_cancelled());
    assert!(
        !tree
            .control
            .mailbox_has_trigger_turn(&child_context.path)
            .expect("trigger state")
    );
    assert!(!child_context.tree_cancel_token().is_cancelled());
    tree.control
        .complete_execution(child_lease, AgentStatus::Interrupted, None)
        .expect("complete child");
    runtime.complete_root(
        execution,
        &Ok(RunSummary {
            session_id: session.session.id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: Some(FinishReason::Stop),
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
        false,
    );
}

#[tokio::test]
async fn quiescence_wait_keeps_detached_child_and_external_cancel_bridge_alive() {
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
    let external_cancel = CancellationToken::new();
    let execution = runtime
        .begin_root(
            &session,
            config,
            SharedConfirmationPrompt::new(AllowPrompt),
            None,
            external_cancel.clone(),
        )
        .expect("root execution");
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
    let summary = Ok(RunSummary {
        session_id: session.session.id,
        assistant_message_id: None,
        status: SessionStatus::Completed,
        finish_reason: Some(FinishReason::Stop),
        tool_call_count: 0,
        failed_tool_count: 0,
        change_count: 0,
        metrics: Default::default(),
    });
    runtime.complete_root(execution, &summary, false);
    assert!(
        !child_cancel.is_cancelled(),
        "successful root completion must preserve detached child work"
    );
    assert!(
        tree.confirmation
            .lock()
            .expect("confirmation state")
            .is_some(),
        "the broker must remain while a detached child is active"
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
    external_cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), child_cancel.cancelled())
        .await
        .expect("external cancellation reached detached child");
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            runtime.wait_for_tree_quiescence(session.session.id),
        )
        .await
        .is_err(),
        "the cancellation bridge must wait for the cancelled child to release its execution"
    );
    tree.control
        .complete_execution(child_lease, AgentStatus::Interrupted, None)
        .expect("complete detached child");
    tokio::time::timeout(
        Duration::from_secs(1),
        runtime.wait_for_tree_quiescence(session.session.id),
    )
    .await
    .expect("bounded quiescence wait")
    .expect("tree quiescence");
    assert!(
        tree.confirmation
            .lock()
            .expect("confirmation state")
            .is_none(),
        "the final detached child completion must release the broker"
    );
}

#[tokio::test]
async fn failed_root_cancels_detached_children_but_successful_root_does_not() {
    let (runtime, session, config) = direct_runtime_fixture("root-failure-cascade", 2).await;
    let execution = runtime
        .begin_root(
            &session,
            config,
            SharedConfirmationPrompt::new(AllowPrompt),
            None,
            CancellationToken::new(),
        )
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
        false,
    );

    assert!(tree.control.tree_cancel_token().is_cancelled());
    assert!(child_cancel.is_cancelled());
    tree.control
        .complete_execution(child_lease, AgentStatus::Interrupted, None)
        .expect("complete cancelled child");
    runtime
        .wait_for_tree_quiescence(session.session.id)
        .await
        .expect("failed tree quiescence");
    assert!(
        tree.confirmation
            .lock()
            .expect("confirmation state")
            .is_none()
    );
}

#[tokio::test]
async fn root_context_durable_terminal_accessor_cancels_the_whole_tree() {
    let (runtime, session, config) = direct_runtime_fixture("root-durable-terminal", 2).await;
    let execution = runtime
        .begin_root(
            &session,
            config,
            SharedConfirmationPrompt::new(AllowPrompt),
            None,
            CancellationToken::new(),
        )
        .expect("root execution");
    let tree_cancel = execution.context.tree_cancel_token();
    assert!(!tree_cancel.is_cancelled());

    execution
        .context
        .cancel_for_durable_terminal()
        .expect("durable root terminal");
    assert!(tree_cancel.is_cancelled());

    runtime.complete_root(
        execution,
        &Ok(RunSummary {
            session_id: session.session.id,
            assistant_message_id: None,
            status: SessionStatus::Cancelled,
            finish_reason: Some(FinishReason::Cancelled),
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }),
        true,
    );
}

#[derive(Default)]
struct AgentScriptState {
    root_calls: AtomicUsize,
    child_calls: AtomicUsize,
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
}

struct DetachedGoalScriptClient {
    state: Arc<DetachedGoalScriptState>,
}

#[async_trait(?Send)]
impl LlmClient for DetachedGoalScriptClient {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        _cancel: CancellationToken,
        sink: &mut dyn LlmEventSink,
    ) -> Result<LlmResponseSummary, LlmError> {
        let is_child = request.messages.iter().any(|message| {
            matches!(message, ModelMessage::User { content } if content.contains(DETACHED_CHILD_ASSIGNMENT))
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
                        "fork_turns": "all"
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
                    matches!(message, ModelMessage::Assistant { content } if content.contains(DETACHED_CHILD_RESULT))
                });
                self.state
                    .continuation_saw_child_result
                    .store(saw_child_result, Ordering::SeqCst);
                if !saw_child_result {
                    return Err(LlmError::Message(
                        "idle goal continuation started before durable child delivery".to_string(),
                    ));
                }
                emit_tool_call(
                    sink,
                    "complete_goal",
                    "update_goal",
                    json!({"status": "complete"}),
                )?;
                Ok(response_summary(FinishReason::ToolCall))
            }
            3 => {
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
        let is_child = request.messages.iter().any(|message| {
            matches!(message, ModelMessage::User { content } if content.contains(CHILD_ASSIGNMENT))
        });
        self.state
            .requests
            .lock()
            .expect("request capture mutex")
            .push(request.clone());

        if is_child {
            let call = self.state.child_calls.fetch_add(1, Ordering::SeqCst);
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

        match self.state.root_calls.fetch_add(1, Ordering::SeqCst) {
            0 => {
                sink.push(LlmEvent::ReasoningDelta(
                    "root private planning must not be forked".to_string(),
                ))?;
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
                    matches!(message, ModelMessage::Assistant { content } if content.contains(CHILD_RESULT))
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
    }
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
    config.model.provider_metadata_mode = ProviderMetadataMode::OpenAiCompatibleOnly;
    config.model.supports_tools = true;
    config.model.connect_timeout_ms = 2_000;
    config.model.request_timeout_ms = 5_000;
    config.model.stream_idle_timeout_ms = 5_000;
    config.model.max_retries = 0;
    config.model.stream_max_retries = 0;
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
                model: "scripted".to_string(),
                base_url: base_url.clone(),
                config_override: None,
                output_mode: OutputMode::Human,
                show_reasoning: false,
                prompt_dispatch: None,
                editor_context: None,
                review_request: None,
                image_paths: Vec::new(),
                cancel: CancellationToken::new(),
                live_config: None,
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
    let setup_error = run_service
        .execute(
            AppCommand::Run(RunRequest {
                prompt: "fail root setup after admission".to_string(),
                session_id: Some(setup_failure.session.id),
                continue_last: false,
                title: None,
                cwd: root,
                model: "scripted".to_string(),
                base_url,
                config_override: None,
                output_mode: OutputMode::Human,
                show_reasoning: false,
                prompt_dispatch: None,
                editor_context: None,
                review_request: None,
                image_paths: Vec::new(),
                cancel: CancellationToken::new(),
                live_config: None,
                agent_confirmation: Some(shared_confirmation),
                agent_context: None,
            }),
            &mut renderer,
            &mut prompt,
        )
        .await
        .expect_err("invalid root setup");
    assert!(setup_error.to_string().contains("max_concurrent_agents"));
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
            .admit_session_run(setup_failure.session.id)
            .await
            .expect("readmission after setup failure")
            .is_some()
    );
    assert!(!agent_runtime.has_tree_for_session(setup_failure.session.id));
    provider_server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_goal_continuation_waits_for_detached_child_and_loads_its_result() {
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
    config.model.provider_metadata_mode = ProviderMetadataMode::OpenAiCompatibleOnly;
    config.model.supports_tools = true;
    config.model.connect_timeout_ms = 2_000;
    config.model.request_timeout_ms = 5_000;
    config.model.stream_idle_timeout_ms = 5_000;
    config.model.max_retries = 0;
    config.model.stream_max_retries = 0;
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
    let summary = tokio::time::timeout(
        Duration::from_secs(5),
        run_service.execute(
            AppCommand::Run(RunRequest {
                prompt: "/goal Integrate the detached child result before completion".to_string(),
                session_id: Some(root_session.session.id),
                continue_last: false,
                title: None,
                cwd: root,
                model: "scripted".to_string(),
                base_url,
                config_override: None,
                output_mode: OutputMode::Human,
                show_reasoning: false,
                prompt_dispatch: None,
                editor_context: None,
                review_request: None,
                image_paths: Vec::new(),
                cancel: CancellationToken::new(),
                live_config: None,
                agent_confirmation: Some(shared_confirmation),
                agent_context: None,
            }),
            &mut renderer,
            &mut prompt,
        ),
    )
    .await
    .expect("bounded goal continuation")
    .expect("goal run");

    assert_eq!(summary.status, SessionStatus::Completed);
    assert_eq!(script.root_calls.load(Ordering::SeqCst), 4);
    assert_eq!(script.child_calls.load(Ordering::SeqCst), 1);
    assert!(script.child_finished.load(Ordering::SeqCst));
    assert!(script.continuation_saw_child_result.load(Ordering::SeqCst));
    assert_eq!(
        store
            .session_repo()
            .get_thread_goal(summary.session_id)
            .await
            .expect("goal read")
            .expect("goal")
            .status,
        ThreadGoalStatus::Complete
    );
    provider_server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scripted_provider_runs_parent_child_parent_with_durable_filtered_context() {
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
    config.model.provider_metadata_mode = ProviderMetadataMode::OpenAiCompatibleOnly;
    config.model.supports_tools = true;
    config.model.supports_reasoning = true;
    config.model.supports_images = false;
    config.model.parallel_tool_calls = false;
    config.model.connect_timeout_ms = 2_000;
    config.model.request_timeout_ms = 5_000;
    config.model.stream_idle_timeout_ms = 5_000;
    config.model.max_retries = 0;
    config.model.stream_max_retries = 0;
    config.permissions.access_mode = AccessMode::FullAccess;
    config.multi_agent.enabled = true;
    config.multi_agent.mode = MultiAgentMode::ExplicitRequestOnly;
    config.multi_agent.max_concurrent_agents = 3;
    config.multi_agent.max_concurrent_model_requests = 1;

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
    let source_turn_id = TurnId::new();
    let source_activity = project_sub_agent_activity(
        root_session.session.id,
        source_turn_id,
        0,
        "preexisting_activity".to_string(),
        root_session.session.id,
        "/root/previous".to_string(),
        SubAgentActivityKind::Interacted,
    );
    store
        .protocol_event_store()
        .append_event_bundle(
            &source_activity.runtime_event,
            source_activity.history_item.as_ref(),
            source_activity.turn_item.as_ref(),
        )
        .expect("seed source activity");
    let source_reasoning = project_protocol_run_event(
        &RunEvent::ReasoningDelta {
            message_id: MessageId::new(),
            delta: "preexisting reasoning must not be forked".to_string(),
        },
        Some(root_session.session.id),
        source_turn_id,
        1,
    )
    .expect("project source reasoning");
    store
        .protocol_event_store()
        .append_event_bundle(
            &source_reasoning.runtime_event,
            source_reasoning.history_item.as_ref(),
            source_reasoning.turn_item.as_ref(),
        )
        .expect("seed source reasoning");
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
    let summary = run_service
        .execute(
            AppCommand::Run(RunRequest {
                prompt: ROOT_TASK.to_string(),
                session_id: Some(root_session.session.id),
                continue_last: false,
                title: None,
                cwd: root.clone(),
                model: "scripted".to_string(),
                base_url: base_url.clone(),
                config_override: None,
                output_mode: OutputMode::Human,
                show_reasoning: false,
                prompt_dispatch: None,
                editor_context: None,
                review_request: None,
                image_paths: Vec::new(),
                cancel: CancellationToken::new(),
                live_config: None,
                agent_confirmation: Some(shared_confirmation),
                agent_context: None,
            }),
            &mut renderer,
            &mut execute_prompt,
        )
        .await
        .expect("root run");

    assert_eq!(summary.status, SessionStatus::Completed);
    assert_eq!(summary.session_id, root_session.session.id);
    assert_eq!(script.root_calls.load(Ordering::SeqCst), 3);
    assert_eq!(script.child_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        session_service
            .get_session(summary.session_id)
            .await
            .expect("root session")
            .status,
        SessionStatus::Completed
    );

    let edges = store
        .session_repo()
        .list_session_spawn_edges(summary.session_id)
        .await
        .expect("spawn edges");
    assert_eq!(edges.len(), 1);
    let edge = &edges[0];
    assert_eq!(edge.root_session_id, summary.session_id);
    assert_eq!(edge.parent_session_id, summary.session_id);
    assert_eq!(edge.agent_path, "/root/worker");
    assert_eq!(edge.task_name, "worker");
    let child_session_id = edge.child_session_id;

    let visible_sessions = session_service
        .list_sessions(workspace.project_id, 20)
        .await
        .expect("normal session list");
    assert_eq!(visible_sessions.len(), 1);
    assert_eq!(visible_sessions[0].id, summary.session_id);
    assert_eq!(
        session_service
            .get_session(child_session_id)
            .await
            .expect("explicit child session")
            .status,
        SessionStatus::Completed
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let child_activity = loop {
        if let Some(activity) = agent_runtime
            .activity_records(summary.session_id)
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

    let root_history = store
        .protocol_event_store()
        .list_history_items_for_session(summary.session_id)
        .expect("root history");
    assert!(root_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::SubAgentActivity { activity_id, .. }
            if activity_id == "preexisting_activity"
    )));
    assert!(root_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::Reasoning { text }
            if text.contains("preexisting reasoning must not be forked")
    )));
    assert!(root_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::ToolCall {
            tool: ToolName::SpawnAgent,
            ..
        }
    )));
    assert!(root_history.iter().any(|item| {
        matches!(
            &item.payload,
            HistoryItemPayload::InterAgentCommunication { communication }
                if communication.author == "/root/worker"
                    && communication.recipient == "/root"
                    && communication.content.contains(CHILD_RESULT)
                    && !communication.trigger_turn
        )
    }));

    let child_history = store
        .protocol_event_store()
        .list_history_items_for_session(child_session_id)
        .expect("child history");
    assert!(child_history.iter().any(|item| {
        matches!(
            &item.payload,
            HistoryItemPayload::UserTurn { content, .. }
                if content_contains(content, ROOT_TASK)
        )
    }));
    assert!(child_history.iter().any(|item| {
        matches!(
            &item.payload,
            HistoryItemPayload::Message {
                role: MessageRole::Assistant,
                content,
                ..
            } if content_contains(content, ROOT_PLAN)
        )
    }));
    assert!(child_history.iter().any(|item| {
        matches!(
            &item.payload,
            HistoryItemPayload::UserTurn { content, .. }
                if content_contains(content, CHILD_ASSIGNMENT)
        )
    }));
    assert!(!child_history.iter().any(|item| matches!(
        item.payload,
        HistoryItemPayload::ToolCall { .. }
            | HistoryItemPayload::ToolOutput { .. }
            | HistoryItemPayload::Reasoning { .. }
            | HistoryItemPayload::SubAgentActivity { .. }
    )));

    let requests = script.requests.lock().expect("request capture mutex");
    let child_request = requests
        .iter()
        .find(|request| {
            request.messages.iter().any(|message| {
                matches!(message, ModelMessage::User { content } if content.contains(CHILD_ASSIGNMENT))
            })
        })
        .expect("captured child request");
    assert!(child_request.system_prompt.contains("## Sub-agent"));
    assert!(
        child_request
            .system_prompt
            .contains("bounded task assigned by your parent")
    );
    let tool_names = child_request
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    for required in [
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
