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
    DurableTurnTerminal, FinishReason, ProjectRepository, RunEvent, SessionSelector,
    SessionStartRequest, SessionStatus, ThreadGoalStatus, TokenUsage,
};
use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
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

fn captured_turn_config(config: ResolvedConfig) -> Arc<crate::config::ResolvedTurnConfig> {
    Arc::new(crate::config::ResolvedTurnConfig::capture(config).expect("valid test turn config"))
}

fn durable_mailbox_content(
    context: &AgentRunContext,
    notice: &crate::runtime::AgentMailboxNotice,
) -> String {
    let item = context
        .runtime
        .store
        .protocol_event_store()
        .history_items_by_id(context.root_session_id(), &[notice.history_item_id])
        .expect("durable mailbox history")
        .into_iter()
        .next()
        .expect("mailbox history item");
    match item.payload {
        HistoryItemPayload::InterAgentCommunication { communication } => communication.content,
        payload => panic!("mailbox notice referenced non-communication payload: {payload:?}"),
    }
}

#[test]
fn suppressed_mail_delivery_is_not_acknowledged_as_queued() {
    let error = match scheduled_mail_delivery(AgentMailDeliveryOutcome::Suppressed) {
        Err(error) => error,
        Ok(_) => panic!("suppressed delivery must not return a successful queued result"),
    };

    assert_eq!(error, SUPPRESSED_MAIL_DELIVERY_ERROR);
}

#[test]
fn suppressed_result_delivery_preserves_durable_child_terminal_status() {
    for status in [
        AgentStatus::Completed(Some("durable child result".to_string())),
        AgentStatus::Errored("durable child failure".to_string()),
    ] {
        let completion = AgentTurnCompletion::new(status.clone())
            .with_delivery_outcome(Ok(AgentMailDeliveryOutcome::Suppressed));

        assert_eq!(completion.status, status);
        assert_eq!(
            completion.activity.as_deref(),
            Some(SUPPRESSED_MAIL_DELIVERY_ERROR)
        );
    }
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
fn child_approval_abort_interrupts_root_and_sibling_before_prompt_returns() {
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
    let tree = AgentTreeRuntime {
        root_session_id,
        control,
        confirmation: Mutex::new(None),
        model_request_gate: Mutex::new(Arc::new(tokio::sync::Semaphore::new(2))),
        active_root_turn_owner: Mutex::new(None),
        metadata: Mutex::new(HashMap::new()),
    };
    let confirmation =
        SharedConfirmationPrompt::new_with_root_control(AbortPrompt, root_control.clone());
    tree.install_run_resources(confirmation.clone(), 2);
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
    let mut child_prompt = tree.confirmation_prompt();

    let outcome = child_prompt
        .confirm_with_control(&request, &requesting_child.run_control())
        .expect("approval abort outcome");

    assert_eq!(outcome, crate::cli::ConfirmationOutcome::Aborted);
    assert!(matches!(
        root_control.cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted
        ))
    ));
    assert!(matches!(
        requesting_child.run_control().cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted
        ))
    ));
    assert!(matches!(
        sibling.run_control().cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::TreeStopped
        ))
    ));
    let sibling_provider_starts = AtomicUsize::new(0);
    if !sibling.run_control().is_cancelled() {
        sibling_provider_starts.fetch_add(1, Ordering::SeqCst);
    }
    assert_eq!(sibling_provider_starts.load(Ordering::SeqCst), 0);
}

#[test]
fn child_approval_abort_reaches_the_tree_while_root_success_is_committing() {
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
        confirmation: Mutex::new(None),
        model_request_gate: Mutex::new(Arc::new(tokio::sync::Semaphore::new(2))),
        active_root_turn_owner: Mutex::new(None),
        metadata: Mutex::new(HashMap::new()),
    };
    let confirmation =
        SharedConfirmationPrompt::new_with_root_control(AbortPrompt, root_control.clone());
    tree.install_run_resources(confirmation, 2);
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

    let outcome = tree
        .confirmation_prompt()
        .confirm_with_control(&request, &requesting_child.run_control())
        .expect("requesting child receives its abort");

    assert_eq!(outcome, crate::cli::ConfirmationOutcome::Aborted);
    assert_eq!(
        root_control.cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted
        ))
    );
    assert_eq!(
        requesting_child.run_control().cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted
        ))
    );
    assert_eq!(
        sibling.run_control().cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::TreeStopped
        ))
    );
    assert!(tree.control.tree_is_cancelled());
    assert!(success_commit.seal());
    assert!(root_turn_control.success_is_sealed());
}

#[test]
fn detached_child_approval_abort_preserves_sealed_root_success_and_stops_the_tree() {
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
        confirmation: Mutex::new(None),
        model_request_gate: Mutex::new(Arc::new(tokio::sync::Semaphore::new(2))),
        active_root_turn_owner: Mutex::new(None),
        metadata: Mutex::new(HashMap::new()),
    };
    assert!(root_turn_control.seal_success());
    let confirmation =
        SharedConfirmationPrompt::new_with_root_control(AbortPrompt, root_control.clone());
    tree.install_run_resources(confirmation, 2);
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

    let outcome = tree
        .confirmation_prompt()
        .confirm_with_control(&request, &requesting_child.run_control())
        .expect("detached child Abort");

    assert_eq!(outcome, crate::cli::ConfirmationOutcome::Aborted);
    assert_eq!(
        root_control.cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted
        ))
    );
    assert!(root_turn_control.success_is_sealed());
    assert_eq!(
        requesting_child.run_control().cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted
        ))
    );
    assert_eq!(
        sibling.run_control().cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::TreeStopped
        ))
    );
    assert!(tree.control.tree_is_cancelled());
}

#[test]
fn child_approval_abort_does_not_override_a_competing_root_terminal_cause() {
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
            confirmation: Mutex::new(None),
            model_request_gate: Mutex::new(Arc::new(tokio::sync::Semaphore::new(2))),
            active_root_turn_owner: Mutex::new(None),
            metadata: Mutex::new(HashMap::new()),
        };
        assert!(root_turn_control.cancel(existing_cause.clone()));
        let confirmation =
            SharedConfirmationPrompt::new_with_root_control(AbortPrompt, root_control.clone());
        tree.install_run_resources(confirmation, 2);
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

        let outcome = tree
            .confirmation_prompt()
            .confirm_with_control(&request, &requesting_child.run_control())
            .expect("requesting child receives its abort");

        assert_eq!(outcome, crate::cli::ConfirmationOutcome::Interrupted);
        assert_eq!(root_control.cause(), Some(existing_cause.clone()));
        let descendant_cause = match existing_cause {
            RunCancellationCause::Interruption(_) => {
                RunCancellationCause::Interruption(TurnInterruptionCause::TreeStopped)
            }
            RunCancellationCause::Failure(message) => RunCancellationCause::Failure(message),
            RunCancellationCause::Superseded => RunCancellationCause::Superseded,
        };
        assert_eq!(
            requesting_child.run_control().cause(),
            Some(descendant_cause.clone())
        );
        assert_eq!(sibling.run_control().cause(), Some(descendant_cause));
        assert!(tree.control.tree_is_cancelled());
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

async fn child_finish_fixture(
    test_name: &str,
) -> (
    Arc<AgentRuntime>,
    AgentRuntimeExecution,
    AgentRunContext,
    AgentExecutionLease,
    crate::session::SessionContext,
) {
    let (runtime, root_session, config) = direct_runtime_fixture(test_name, 2).await;
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
        execution: child_lease.scope(),
        root_turn_owner: Arc::clone(&root_execution.context.root_turn_owner),
        config: captured_turn_config(config),
        workspace: child.workspace.clone(),
    };
    (runtime, root_execution, child_context, child_lease, child)
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
                None,
                None,
            )
            .await
            .expect("terminalize fixture")
            .was_applied()
    );
}

async fn bind_test_root_turn(runtime: &AgentRuntime, execution: &AgentRuntimeExecution) {
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
        .bind_root_turn_owner(admission.admission_id, turn_id)
        .expect("bind test root durable turn owner");
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
            let result = Ok(terminal_summary(
                child.session.id,
                TurnTerminalOutcome::Interrupted { cause },
            ));

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
            context
                .tree
                .control
                .complete_execution(
                    child_lease,
                    inactive_agent_status(status).expect("inactive child status"),
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
async fn durable_child_agent_interruption_delivers_one_typed_parent_notification() {
    let (runtime, root_execution, context, child_lease, child) =
        child_finish_fixture("durable-child-agent-interrupted").await;
    let result = Ok(terminal_summary(
        child.session.id,
        TurnTerminalOutcome::Interrupted {
            cause: TurnInterruptionCause::AgentInterrupted,
        },
    ));

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
    assert_eq!(mail.len(), 1);
    assert_eq!(
        durable_mailbox_content(&context, &mail[0]),
        "Agent interrupted."
    );
    context
        .tree
        .control
        .complete_execution(
            child_lease,
            inactive_agent_status(status).expect("inactive child status"),
            None,
        )
        .expect("complete child");
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root");
}

#[tokio::test]
async fn durable_child_failure_uses_latest_error_despite_stale_local_stop() {
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
    let result = Ok(terminal_summary(
        child.session.id,
        TurnTerminalOutcome::Failed {
            error: "durable child failed".to_string(),
        },
    ));

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
        AgentStatus::Errored("durable final child failure".to_string())
    );
    let mail = context
        .tree
        .control
        .drain_mailbox(&AgentPath::root())
        .expect("root mailbox");
    assert_eq!(mail.len(), 1);
    assert_eq!(
        durable_mailbox_content(&context, &mail[0]),
        "durable final child failure"
    );
    context
        .tree
        .control
        .complete_execution(
            child_lease,
            inactive_agent_status(status).expect("inactive child status"),
            None,
        )
        .expect("complete child");
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root");
}

#[tokio::test]
async fn durable_failed_child_live_and_restart_projections_are_identical() {
    for (index, history_payloads) in [
        vec![HistoryItemPayload::Error {
            message: "durable failed error".to_string(),
        }],
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
        let (runtime, root_execution, context, child_lease, mut child) =
            child_finish_fixture(&format!("durable-failed-equality-{index}")).await;
        for payload in history_payloads {
            append_child_history(&runtime, child.session.id, payload);
        }
        let result = Ok(terminal_summary(
            child.session.id,
            TurnTerminalOutcome::Failed {
                error: "durable child failed".to_string(),
            },
        ));

        let live_status = runtime
            .finish_agent_turn(&context, &result, None)
            .await
            .status;
        let history = runtime
            .store
            .protocol_event_store()
            .list_history_items_for_session(child.session.id)
            .expect("child history");
        child.session.status = SessionStatus::Failed;
        let restarted_status = rehydrated_agent_state(
            child.session.id,
            child.session.status,
            durable_child_result(SessionStatus::Failed, &history),
            None,
        )
        .expect("rehydrated failed child");

        assert_eq!(live_status, restarted_status);
        let mail = context
            .tree
            .control
            .drain_mailbox(&AgentPath::root())
            .expect("root mailbox");
        assert_eq!(mail.len(), 1);
        assert_eq!(
            durable_mailbox_content(&context, &mail[0]),
            agent_status_result(&restarted_status)
        );
        context
            .tree
            .control
            .complete_execution(
                child_lease,
                inactive_agent_status(live_status).expect("inactive child status"),
                None,
            )
            .expect("complete child");
        root_execution
            .complete(AgentStatus::Completed(None))
            .expect("complete root");
    }
}

#[tokio::test]
async fn durable_child_success_matches_rehydrated_canonical_history() {
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
    let history = runtime
        .store
        .protocol_event_store()
        .list_history_items_for_session(child.session.id)
        .expect("child history");
    assert_eq!(
        durable_child_result(SessionStatus::Completed, &history),
        Some(content.clone())
    );
    let result = Ok(terminal_summary(
        child.session.id,
        TurnTerminalOutcome::Completed,
    ));

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

    assert_eq!(status, AgentStatus::Completed(Some(content.clone())));
    let mail = context
        .tree
        .control
        .drain_mailbox(&AgentPath::root())
        .expect("root mailbox");
    assert_eq!(mail.len(), 1);
    assert_eq!(durable_mailbox_content(&context, &mail[0]), content);
    context
        .tree
        .control
        .complete_execution(
            child_lease,
            inactive_agent_status(status).expect("inactive child status"),
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
    append_child_history(
        &runtime,
        child.session.id,
        HistoryItemPayload::AssistantMessage {
            response_id: final_response_id,
            content: vec![ContentPart::Text {
                text: "terminal response result".to_string(),
            }],
        },
    );
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
    let result = Ok(RunSummary::from_terminal(
        child.session.id,
        TurnId::new(),
        DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Completed,
            final_response_id: Some(final_response_id),
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        },
    ));

    let completion = runtime.finish_agent_turn(&context, &result, None).await;

    assert_eq!(
        completion.status,
        AgentStatus::Completed(Some("terminal response result".to_string()))
    );
    let mail = context
        .tree
        .control
        .drain_mailbox(&AgentPath::root())
        .expect("root mailbox");
    assert_eq!(mail.len(), 1);
    assert_eq!(
        durable_mailbox_content(&context, &mail[0]),
        "terminal response result"
    );
    context
        .tree
        .control
        .complete_execution(
            child_lease,
            inactive_agent_status(completion.status).expect("inactive child status"),
            completion.activity,
        )
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
    let result = Ok(terminal_summary(
        child.session.id,
        TurnTerminalOutcome::Completed,
    ));

    let status = runtime
        .finish_agent_turn(&context, &result, None)
        .await
        .status;

    assert_eq!(status, AgentStatus::Completed(Some(content.clone())));
    let root_history = runtime
        .store
        .protocol_event_store()
        .list_history_items_for_session(root_session_id)
        .expect("root history");
    assert!(root_history.iter().any(|item| {
        matches!(
            &item.payload,
            HistoryItemPayload::InterAgentCommunication { communication }
                if communication.content == content
        )
    }));
    let mail = context
        .tree
        .control
        .drain_mailbox(&AgentPath::root())
        .expect("root mailbox");
    assert_eq!(mail.len(), 1);
    assert_eq!(durable_mailbox_content(&context, &mail[0]), content);
    context
        .tree
        .control
        .complete_execution(
            child_lease,
            inactive_agent_status(status).expect("inactive child status"),
            None,
        )
        .expect("complete child");
    root_execution
        .complete(AgentStatus::Completed(None))
        .expect("complete root");
}

#[tokio::test]
async fn existing_child_followup_uses_the_newly_admitted_root_config() {
    let (runtime, root_session, mut config) =
        direct_runtime_fixture("followup-admitted-access", 3).await;
    let child_path = AgentPath::root().join("research").expect("child path");
    let child = runtime
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("research".to_string()),
                cwd: root_session.workspace.cwd.clone(),
                model: config.model.model.clone(),
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
    config.permissions.access_mode = AccessMode::FullAccess;
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

    let materialized = runtime
        .materialize_context_config_and_sync_session(&child_context)
        .await
        .expect("materialized followup config");

    assert_eq!(materialized.permissions.access_mode, AccessMode::FullAccess);
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
            captured_turn_config(config.clone()),
            original.clone(),
            RunControl::new(),
        )
        .await
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
            captured_turn_config(resumed_config),
            replacement.clone(),
            RunControl::new(),
        )
        .await
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
    let turn_id = TurnId::new();
    terminalize_test_session(
        &original_runtime,
        child_session.session.id,
        turn_id,
        &terminal_event(
            child_session.session.id,
            TurnTerminalOutcome::Completed,
            None,
        ),
    )
    .await;

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
        &Ok(terminal_summary(
            root_session.session.id,
            TurnTerminalOutcome::Completed,
        )),
        None,
    );
}

#[test]
fn unlaunchable_scheduled_turn_consumes_trigger_before_failed_handoff() {
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
        .commit_and_enqueue_mail(&root, &child.path, true, || Ok(HistoryItemId::new()))
        .expect("enqueue follow-up trigger");
    let AgentMailDeliveryOutcome::Enqueued { mut scheduled, .. } = delivery else {
        panic!("follow-up trigger must be scheduled");
    };
    assert_eq!(scheduled.len(), 1);

    let additional = settle_unlaunchable_scheduled_execution(
        &control,
        scheduled.pop().expect("scheduled child lease"),
        "child context could not be constructed".to_string(),
    );

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

#[test]
fn failed_durable_child_prefers_latest_error_over_partial_assistant_text() {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let history = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 0,
            created_at_ms: SystemClock::now_ms(),
            payload: HistoryItemPayload::AssistantMessage {
                response_id: ModelResponseId::new(),
                content: vec![ContentPart::Text {
                    text: "partial assistant output".to_string(),
                }],
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 1,
            created_at_ms: SystemClock::now_ms(),
            payload: HistoryItemPayload::Error {
                message: "recoverable provider error".to_string(),
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            scope: HistoryScope::Turn { turn_id },
            sequence_no: 2,
            created_at_ms: SystemClock::now_ms(),
            payload: HistoryItemPayload::Error {
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
            captured_turn_config(config.clone()),
            SharedConfirmationPrompt::new(AllowPrompt),
            RunControl::new(),
        )
        .await
        .expect("root execution");
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
        execution: child_lease.scope(),
        root_turn_owner: Arc::clone(&execution.context.root_turn_owner),
        config: captured_turn_config(config),
        workspace: session.workspace.clone(),
    };
    let agents_before = tree.control.snapshot().expect("tree before").agents.len();
    let sessions_before = runtime
        .session_service
        .list_sessions(session.session.project_id, 20)
        .await
        .expect("sessions before")
        .len();

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
        sessions_before
    );
    let _ = tree
        .control
        .commit_and_enqueue_mail(&AgentPath::root(), &child_context.path, true, || {
            runtime.append_communication(
                child_context.session_id,
                InterAgentCommunication {
                    author: AgentPath::root().to_string(),
                    recipient: child_context.path.to_string(),
                    content: "must not restart after durable terminalization".to_string(),
                    trigger_turn: true,
                },
                false,
            )
        })
        .expect("trigger mail");
    assert!(!tree.control.tree_is_cancelled());
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
    assert!(!tree.control.tree_is_cancelled());
    tree.control
        .complete_execution(child_lease, InactiveAgentStatus::Interrupted, None)
        .expect("complete child");
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
async fn completed_root_task_scope_stop_preserves_turn_success_and_stops_active_children() {
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
    assert!(root_control.interrupt(TurnInterruptionCause::UserStop));
    assert!(!root_control.interrupt(TurnInterruptionCause::ApprovalAborted));
    tokio::time::timeout(Duration::from_secs(1), child_cancel.cancelled())
        .await
        .expect("task-scope Stop reached the active child while preserving sealed turn success");
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

    assert!(tree.control.tree_is_cancelled());
    assert!(child_cancel.is_cancelled());
    tree.control
        .complete_execution(child_lease, InactiveAgentStatus::Interrupted, None)
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
async fn durable_failed_root_cancels_active_and_queued_children() {
    let (runtime, session, config) =
        direct_runtime_fixture("durable-root-failure-cascade", 2).await;
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
    let delivery = tree
        .control
        .commit_and_enqueue_mail(&AgentPath::root(), &queued_path, true, || {
            Ok(HistoryItemId::new())
        })
        .expect("queue follow-up while capacity is full");
    assert!(matches!(
        delivery,
        AgentMailDeliveryOutcome::Enqueued { ref scheduled, .. } if scheduled.is_empty()
    ));

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

    assert!(tree.control.tree_is_cancelled());
    assert!(matches!(
        root_control.cause(),
        Some(RunCancellationCause::Failure(message))
            if message.contains("durable failed status")
    ));
    assert!(matches!(
        active_child_control.cause(),
        Some(RunCancellationCause::Failure(message))
            if message.contains("durable failed status")
    ));
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
        .complete_execution(
            active_child,
            InactiveAgentStatus::Errored("root failed".to_string()),
            None,
        )
        .expect("settle active child");
}

#[tokio::test]
async fn durable_root_interruption_cause_wins_over_a_conflicting_local_cause() {
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
        root_control.cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted
        )),
        "the local first-writer record is immutable"
    );
    assert!(tree.control.tree_is_cancelled());
    assert_eq!(
        child_control.cause(),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::TreeStopped
        )),
        "the authoritative durable root stop must still close descendant work"
    );
    tree.control
        .complete_execution(child, InactiveAgentStatus::Interrupted, None)
        .expect("settle child");
}

#[tokio::test]
async fn durable_root_success_preserves_root_while_deferred_stop_closes_children() {
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
    assert!(child_cancel.is_cancelled());
    assert!(tree.control.tree_is_cancelled());
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

    assert!(tree.control.tree_is_cancelled());
    assert!(child_cancel.is_cancelled());
    assert!(matches!(
        tree.control
            .status(&AgentPath::root())
            .expect("root status"),
        AgentStatus::Completed(_)
    ));
    assert!(!runtime.cancel_tree_for_session(session.session.id, TurnInterruptionCause::UserStop,));
    assert!(tree.control.tree_is_cancelled());
    assert!(child_cancel.is_cancelled());
    tree.control
        .complete_execution(child_lease, InactiveAgentStatus::Interrupted, None)
        .expect("complete detached child");
}

#[tokio::test]
async fn zero_child_stop_during_success_blocks_idle_root_continuation() {
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
    assert!(tree.control.tree_is_cancelled());
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
    assert!(matches!(
        runtime
            .begin_root_continuation(
                session.session.id,
                root_control,
                Some(SharedConfirmationPrompt::new(AllowPrompt)),
            )
            .expect("continuation outcome"),
        AgentRuntimeContinuationOutcome::Blocked
    ));
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
async fn root_continuation_claim_before_stop_reuses_and_cancels_the_retained_tree() {
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
    assert!(tree.control.tree_is_cancelled());
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
async fn preclaimed_root_early_error_classifies_tree_and_releases_lease() {
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
        root_control.cause(),
        Some(RunCancellationCause::Failure(
            "continuation setup failed".to_string()
        ))
    );
    assert!(tree.control.tree_is_cancelled());
    assert!(tree.control.is_quiescent().expect("tree quiescence"));
    assert!(matches!(
        tree.control
            .status(&AgentPath::root())
            .expect("root status"),
        AgentStatus::Errored(_)
    ));
}

#[tokio::test]
async fn active_root_cancel_cascades_before_the_root_run_settles() {
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
    let tree = execution.context.tree.clone();
    let root_cancel = root_control.token();
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
    tokio::time::timeout(Duration::from_secs(1), root_cancel.cancelled())
        .await
        .expect("external cancellation reached active root");
    assert!(
        child_cancel.is_cancelled(),
        "raw current-root cancellation must close descendant work synchronously"
    );

    runtime.complete_root(
        execution,
        &Err(AppRunError::Message("root cancelled".to_string())),
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::UserStop,
        )),
    );
    assert!(tree.control.tree_is_cancelled());
    assert!(child_cancel.is_cancelled());
    tree.control
        .complete_execution(child_lease, InactiveAgentStatus::Interrupted, None)
        .expect("complete detached child");
}

#[tokio::test]
async fn root_context_durable_terminal_accessor_cancels_the_whole_tree() {
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
    let tree = execution.context.tree.clone();
    assert!(!tree.control.tree_is_cancelled());

    execution
        .context
        .cancel_for_durable_terminal()
        .expect("durable root terminal");
    assert!(tree.control.tree_is_cancelled());
    assert_eq!(root_control.cause(), Some(RunCancellationCause::Superseded));

    runtime.complete_root(
        execution,
        &Err(AppRunError::Message("root turn was superseded".to_string())),
        Some(RunCancellationCause::Superseded),
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
                    matches!(message, ModelMessage::User { content }
                        if content.contains("<inter_agent_message")
                            && content.contains(DETACHED_CHILD_RESULT))
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
                    matches!(message, ModelMessage::User { content }
                        if content.contains("<inter_agent_message")
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
    let tree = root_execution.context.tree.clone();
    let child = runtime
        .session_service
        .start_or_resume(
            SessionStartRequest {
                selector: SessionSelector::New,
                title: Some("cancelled child".to_string()),
                cwd: root_session.workspace.cwd.clone(),
                model: config.model.model.clone(),
                base_url: config.model.base_url.clone(),
                access_mode: config.permissions.access_mode,
            },
            root_session.workspace.clone(),
        )
        .await
        .expect("child session");
    let child_path = AgentPath::root()
        .join("cancelled_child")
        .expect("child path");
    let (_, child_lease) = tree
        .control
        .register_child(
            &AgentPath::root(),
            "cancelled_child",
            child.session.id,
            Some("Waiting for worker activation".to_string()),
        )
        .expect("child registration");
    let child_context = AgentRunContext {
        runtime: runtime.clone(),
        tree: tree.clone(),
        path: child_path.clone(),
        session_id: child.session.id,
        execution: child_lease.scope(),
        root_turn_owner: Arc::clone(&root_execution.context.root_turn_owner),
        config: captured_turn_config(config),
        workspace: child.workspace.clone(),
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
            .fresh_running_turn_for_session(child.session.id)
            .await
            .expect("child active turn")
            .is_none()
    );
    assert!(
        runtime
            .store
            .protocol_event_store()
            .list_history_items_for_session(child.session.id)
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
            .admit_session_turn(setup_failure.session.id, TurnId::new())
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
    assert_eq!(script.root_calls.load(Ordering::SeqCst), 4);
    assert_eq!(script.child_calls.load(Ordering::SeqCst), 1);
    assert!(script.child_finished.load(Ordering::SeqCst));
    assert!(script.continuation_saw_child_result.load(Ordering::SeqCst));
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
    assert_eq!(script.root_calls.load(Ordering::SeqCst), 3);
    assert_eq!(script.child_calls.load(Ordering::SeqCst), 1);
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
    assert_eq!(edges.len(), 1);
    let edge = &edges[0];
    assert_eq!(edge.root_session_id, summary.session_id());
    assert_eq!(edge.parent_session_id, summary.session_id());
    assert_eq!(edge.agent_path, "/root/worker");
    assert_eq!(edge.task_name, "worker");
    let child_session_id = edge.child_session_id;

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

    let root_history = store
        .protocol_event_store()
        .list_history_items_for_session(summary.session_id())
        .expect("root history");
    assert!(root_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::SubAgentActivity { activity_id, .. }
            if activity_id == "preexisting_activity"
    )));
    assert!(root_history.iter().any(|item| matches!(
        &item.payload,
        HistoryItemPayload::ToolCall {
            tool_name,
            ..
        } if tool_name == "spawn_agent"
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
            HistoryItemPayload::AssistantMessage { content, .. }
                if content_contains(content, ROOT_PLAN)
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
    assert!(
        !tool_names.contains(&"spawn_agent"),
        "a one-level child must not receive the root-only spawn_agent tool"
    );
    for required in [
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
