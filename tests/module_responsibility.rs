use std::collections::BTreeSet;
use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use moyai::agent::prompt::build_provider_replay_messages_from_history_items;
use moyai::config::{AccessMode, ShellFamily};
use moyai::harness::preflight::{
    PreflightGateFamily, PreflightResultStatus, run_artifact_replay_preflight,
    run_default_active_preflight,
};
use moyai::llm::ModelMessage;
use moyai::protocol::{
    ActionAuthority, ActiveWorkContractProjection, ContentPart, DispatchPolicy, HistoryItem,
    HistoryItemId, HistoryItemPayload, ModelCapabilities, ObligationKind, ObligationSet,
    ObligationStatus, ProjectionBundle, ProjectionId, ToolChoice, TurnContext, TurnId,
    TurnObligation,
};
use moyai::session::{
    MessagePart, ProjectId, SessionId, SessionRecord, SessionStateSnapshot, SessionStatus,
    TaskRoute, TodoItem, TodoKind, TodoPriority, TodoStatus, transcript_from_history_items,
};
use moyai::session::{history_items_to_markdown, todo_counts_as_open_work};
use moyai::tool::ToolName;
use moyai::workspace::{AccessKind, IgnorePlan, PathGuard, PathPolicy, VcsKind, Workspace};
use tempfile::TempDir;

#[test]
fn harness_preflight_module_runs_codex_style_active_gates() {
    let report = run_default_active_preflight();

    assert_eq!(report.status, PreflightResultStatus::Pass);
    assert_eq!(report.generated_by, "codex_style_preflight_v2");

    let gate_ids = report
        .results
        .iter()
        .map(|result| result.gate_id.as_str())
        .collect::<BTreeSet<_>>();
    assert!(gate_ids.contains("preflight.protocol.history_item_lifecycle_authority"));
    assert!(gate_ids.contains("preflight.item_lifecycle.provider_replay_call_output_symmetry"));
    assert!(gate_ids.contains("preflight.control_envelope.dispatch_projection_authority"));
    assert!(gate_ids.contains("preflight.state_reducer.runtime_feedback_classification_authority"));
    assert!(
        gate_ids
            .contains("preflight.state_reducer.requested_work_completion_promotes_verification")
    );
    assert!(gate_ids.contains(
        "preflight.state_reducer.verification_failure_preserves_repair_target_authority"
    ));
    assert!(gate_ids.contains("preflight.state_reducer.docs_route_contract_authority"));
    assert!(
        gate_ids.contains("preflight.state_reducer.post_repair_edit_promotes_verification_rerun")
    );
    assert!(
        gate_ids
            .contains("preflight.plan_progress_projection.todo_absence_does_not_gate_authoring")
    );
    assert!(gate_ids.contains("preflight.prompt_replay.stale_write_arguments_summary_projection"));
    assert!(gate_ids.contains("preflight.prompt_replay.active_user_hook_non_droppable"));
    assert!(gate_ids.contains("preflight.prompt_replay.tool_pair_symmetry"));
    assert!(gate_ids.contains("preflight.prompt_replay.compaction_orphan_assistant_repaired"));
    assert!(gate_ids.contains("preflight.prompt_replay.stale_inactive_authoring_pair_omitted"));
    assert!(gate_ids.contains("preflight.prompt_replay.progress_projection_pair_omitted"));
    assert!(gate_ids.contains("preflight.tool_lifecycle.typed_route_metadata_authority"));
    assert!(
        gate_ids.contains("preflight.tool_lifecycle.rejected_singleton_payload_terminal_guard")
    );
    assert!(
        gate_ids.contains("preflight.tool_lifecycle.executed_failure_call_output_terminal_guard")
    );
    assert!(gate_ids.contains("preflight.tool_lifecycle.verification_stable_tool_surface"));
    assert!(gate_ids.contains("preflight.tool_lifecycle.authoring_stable_tool_surface"));
    assert!(gate_ids.contains("preflight.tool_lifecycle.progress_projection_stable_surface_guard"));
    assert!(gate_ids.contains("preflight.tool_lifecycle.edit_surface_registry_symmetry"));
    assert!(gate_ids.contains("preflight.tool_lifecycle.rejected_tool_semantic_terminal_guard"));
    assert!(
        gate_ids.contains("preflight.tool_lifecycle.synthetic_feedback_not_verification_authority")
    );
    assert!(gate_ids.contains("preflight.tool_lifecycle.workspace_relative_file_change_authority"));
    assert!(gate_ids.contains("preflight.vision.input_item_lifecycle_authority"));
    assert!(gate_ids.contains("preflight.workspace.absolute_turn_cwd_root_authority"));
    assert!(
        gate_ids.contains("preflight.turn_decision.active_work_edit_before_verification_rerun")
    );
    assert!(gate_ids.contains("preflight.closeout.final_assistant_message_lifecycle"));
    assert!(
        gate_ids.contains("preflight.closeout.open_obligation_final_assistant_continuation_hook")
    );
    assert!(
        gate_ids.contains("preflight.closeout.verification_failure_preserves_closeout_evidence")
    );
    assert!(gate_ids.contains("preflight.closeout.verification_repair_continuation_hook"));
    assert!(gate_ids.contains("preflight.closeout.verification_labels_not_requested_work"));
    assert!(gate_ids.contains("preflight.verification.typed_evidence_cluster_authority"));
    assert!(gate_ids.contains("preflight.route_evidence.schema"));

    for result in &report.results {
        assert_eq!(result.status, PreflightResultStatus::Pass);
        assert!(result.gate_id.starts_with("preflight."));
        assert!(!result.gate_id.contains("case"));
        assert_ne!(
            result.family,
            Some(PreflightGateFamily::ArtifactReplaySchema)
        );
    }
}

#[test]
fn vision_input_projection_uses_codex_labeled_image_item() {
    assert!(moyai::agent::prompt::vision_input_provider_projection_fixture_passes());
}

#[test]
fn provider_replay_keeps_latest_user_hook_after_trailing_compaction() {
    assert!(
        moyai::agent::prompt::provider_replay_preserves_latest_user_across_trailing_compaction()
    );
}

#[test]
fn provider_replay_drops_orphan_assistant_after_compaction() {
    assert!(
        moyai::agent::prompt::provider_replay_after_compaction_repairs_orphan_assistant_before_user(
        )
    );
}

#[test]
fn provider_replay_keeps_tool_pair_symmetry_with_model_arguments() {
    assert!(
        moyai::agent::prompt::provider_replay_preserves_tool_pair_symmetry_with_model_arguments()
    );
}

#[test]
fn provider_replay_omits_stale_inactive_authoring_fake_arguments() {
    assert!(
        moyai::agent::prompt::stale_inactive_authoring_replay_omits_fake_executable_arguments()
    );
}

#[test]
fn provider_replay_omits_stale_progress_projection_arguments() {
    assert!(moyai::agent::prompt::provider_replay_omits_stale_progress_projection_arguments());
}

#[test]
fn harness_artifact_replay_module_requires_route_evidence_schema() {
    let temp = TempDir::new().expect("tempdir");
    let root = utf8_path(temp.path());

    let missing = run_artifact_replay_preflight(&root, vec!["FR03-example".to_string()])
        .expect("preflight report");
    assert_eq!(missing.status, PreflightResultStatus::Fail);
    assert!(missing.results[0].diagnostics.iter().any(|line| {
        line.contains("route_manifest.json") && line.contains("verification_command_log.json")
    }));

    for artifact in [
        "route_manifest.json",
        "verification_command_log.json",
        "workspace_diff_manifest.json",
        "result.json",
        "preflight_report.json",
        "timeout_classification.json",
    ] {
        fs::write(root.join(artifact), "{}").expect("write required artifact");
    }

    let passing = run_artifact_replay_preflight(&root, Vec::new()).expect("preflight report");
    assert_eq!(passing.status, PreflightResultStatus::Pass);
    assert!(passing.results[0].diagnostics.iter().any(|line| {
        line.contains("artifact root satisfies Codex-style route evidence schema")
    }));
}

#[test]
fn protocol_control_module_validates_single_projection_authority() {
    let envelope = build_control_envelope(vec![ToolName::Read]);
    assert!(envelope.validate().passes());

    let narrowed = build_control_envelope_with_choice(
        vec![ToolName::Read, ToolName::TodoWrite, ToolName::Write],
        ToolChoice::Auto,
    );
    assert!(narrowed.validate().passes());
    assert_eq!(
        narrowed.action_authority.allowed_tools,
        vec![ToolName::Read, ToolName::TodoWrite, ToolName::Write]
    );
    assert_eq!(narrowed.action_authority.tool_choice, ToolChoice::Auto);
    assert!(narrowed.projection_bundle.surfaces().iter().all(|surface| {
        surface.allowed_tools == vec![ToolName::Read, ToolName::TodoWrite, ToolName::Write]
            && surface.required_next_action.is_none()
    }));

    let stable = build_control_envelope(vec![ToolName::Read]);
    let validation = stable.validate();
    assert!(validation.passes());

    let surfaces = stable.projection_bundle.rendered_surfaces();
    assert_eq!(surfaces.len(), 5);
    assert!(surfaces.iter().all(|surface| {
        surface.required_next_action.is_none() && surface.allowed_tools == vec!["read".to_string()]
    }));
}

#[test]
fn session_markdown_module_exports_canonical_history_items() {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "Protocol Export".to_string(),
        status: SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local-model".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 10,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "inspect the workspace".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 11,
            payload: HistoryItemPayload::ToolCall {
                call_id: moyai::session::ToolCallId::new(),
                tool: ToolName::Read,
                arguments: serde_json::json!({"path": "src/lib.rs"}),
                model_arguments: serde_json::json!({"path": "src/lib.rs"}),
                effective_arguments: serde_json::json!({"path": "src/lib.rs"}),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::Read],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
    ];

    let markdown = history_items_to_markdown(&session, &items);

    assert!(markdown.contains("# Protocol Export"));
    assert!(markdown.contains("inspect the workspace"));
    assert!(markdown.contains("### Tool Call: read"));
    assert!(markdown.contains("Tool Lifecycle Decisions"));
    assert!(markdown.contains("allowed_surface"));
}

#[test]
fn session_materialized_view_excludes_internal_control_items_from_provider_visible_text() {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "Prompt Projection".to_string(),
        status: SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local-model".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let mut stale_state = SessionStateSnapshot::default();
    stale_state.active_targets = vec![Utf8PathBuf::from("source_module.py")];
    let control_envelope = build_control_envelope(vec![ToolName::Write]);
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 10,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "create test_calculator.py".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 11,
            payload: HistoryItemPayload::SessionState { state: stale_state },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 12,
            payload: HistoryItemPayload::ControlEnvelope {
                envelope: control_envelope,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 13,
            payload: HistoryItemPayload::Error {
                message_id: None,
                message: "terminal runtime failure must stay out of assistant history".to_string(),
            },
        },
    ];

    let transcript = transcript_from_history_items(&session, &items);
    let provider_visible_text = transcript
        .messages
        .iter()
        .flat_map(|message| message.parts.iter())
        .filter_map(|part| match &part.payload {
            MessagePart::Text(value) => Some(value.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(provider_visible_text.contains("test_calculator.py"));
    assert!(!provider_visible_text.contains("source_module.py"));
    assert!(!provider_visible_text.contains("terminal runtime failure"));
    assert!(
        transcript
            .messages
            .iter()
            .all(|message| !matches!(message.record.sequence_no, 2 | 3 | 4))
    );
}

#[test]
fn provider_replay_uses_canonical_history_items_without_transcript_demotions() {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let call_id = moyai::session::ToolCallId::new();
    let orphan_call_id = moyai::session::ToolCallId::new();
    let interrupted_call_id = moyai::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "Provider Replay".to_string(),
        status: SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local-model".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 10,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "inspect the workspace".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 11,
            payload: HistoryItemPayload::ToolCall {
                call_id,
                tool: ToolName::List,
                arguments: serde_json::json!({"path": "."}),
                model_arguments: serde_json::json!({"path": "."}),
                effective_arguments: serde_json::json!({"path": "."}),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::List],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 12,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: moyai::protocol::ToolLifecycleStatus::Completed,
                title: "Listed .".to_string(),
                output_text: "calculator.py".to_string(),
                metadata: serde_json::json!({"success": true}),
                success: Some(true),
                progress_effect: moyai::protocol::ToolProgressEffect::MadeProgress,
                blocked_action: None,
                required_next_action: None,
                result_hash: Some("result-hash".to_string()),
                verification_run: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 13,
            payload: HistoryItemPayload::ToolOutput {
                call_id: orphan_call_id,
                status: moyai::protocol::ToolLifecycleStatus::Completed,
                title: "orphan output".to_string(),
                output_text: "must not become assistant prose".to_string(),
                metadata: serde_json::json!({}),
                success: Some(true),
                progress_effect: moyai::protocol::ToolProgressEffect::Unknown,
                blocked_action: None,
                required_next_action: None,
                result_hash: None,
                verification_run: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 5,
            created_at_ms: 14,
            payload: HistoryItemPayload::ToolCall {
                call_id: interrupted_call_id,
                tool: ToolName::Read,
                arguments: serde_json::json!({"path": "unfinished.py"}),
                model_arguments: serde_json::json!({"path": "unfinished.py"}),
                effective_arguments: serde_json::json!({"path": "unfinished.py"}),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::Read],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 6,
            created_at_ms: 15,
            payload: HistoryItemPayload::Error {
                message_id: None,
                message: "runtime failure must not replay as assistant text".to_string(),
            },
        },
    ];

    let replay = build_provider_replay_messages_from_history_items(&session, &items, 32);
    let rendered = serde_json::to_string(&replay).expect("serialize replay");

    assert!(
        matches!(replay.first(), Some(ModelMessage::User { content }) if content.contains("inspect the workspace"))
    );
    assert!(replay.iter().any(|message| matches!(
        message,
        ModelMessage::AssistantToolCalls { tool_calls, .. }
            if tool_calls.iter().any(|tool_call| tool_call.call_id == call_id.to_string())
    )));
    assert!(replay.iter().any(|message| matches!(
        message,
        ModelMessage::Tool { call_id: replayed, result, .. }
            if replayed == &call_id.to_string() && result.contains("calculator.py")
    )));
    assert!(replay.iter().any(|message| matches!(
        message,
        ModelMessage::Tool { call_id: replayed, result, .. }
            if replayed == &interrupted_call_id.to_string() && result == "aborted"
    )));
    assert!(!rendered.contains("must not become assistant prose"));
    assert!(!rendered.contains("runtime failure must not replay"));
}

#[test]
fn workspace_path_guard_module_enforces_workspace_and_external_roots() {
    let workspace_root = TempDir::new().expect("workspace tempdir");
    let external_root = TempDir::new().expect("external tempdir");
    let root = utf8_path(workspace_root.path());
    let external = utf8_path(external_root.path());
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(root.join("src/lib.rs"), "").expect("write workspace file");
    fs::write(external.join("external.txt"), "").expect("write external file");

    let mut workspace = workspace(root.clone());
    let inside = PathGuard::require_path(
        &workspace,
        Utf8Path::new("src/lib.rs"),
        AccessKind::Read,
        false,
    )
    .expect("inside workspace path is allowed");
    assert!(inside.inside_workspace);
    assert_eq!(inside.relative_to_root, Utf8PathBuf::from("src/lib.rs"));

    let blocked = PathGuard::require_path(
        &workspace,
        &external.join("external.txt"),
        AccessKind::Read,
        false,
    );
    assert!(blocked.is_err());

    workspace
        .path_policy
        .additional_read_roots
        .push(external.clone());
    let trusted = PathGuard::require_path(
        &workspace,
        &external.join("external.txt"),
        AccessKind::Read,
        false,
    )
    .expect("configured read root is allowed");
    assert!(!trusted.inside_workspace);
    assert!(trusted.trusted_external);
}

#[test]
fn session_todo_module_distinguishes_completion_from_open_work() {
    let mut work = TodoItem::simple(
        "implement protocol store",
        TodoStatus::Pending,
        TodoPriority::High,
    );
    assert!(todo_counts_as_open_work(&work));

    work.status = TodoStatus::Completed;
    assert!(!todo_counts_as_open_work(&work));

    let mut closeout = TodoItem::simple(
        "final verification",
        TodoStatus::Pending,
        TodoPriority::Medium,
    );
    closeout.kind = TodoKind::Completion;
    assert!(!todo_counts_as_open_work(&closeout));
}

#[test]
fn config_module_exposes_fixed_three_preset_access_contract() {
    assert_eq!(AccessMode::Default.as_str(), "default");
    assert_eq!(AccessMode::AutoReview.as_str(), "auto_review");
    assert_eq!(AccessMode::FullAccess.as_str(), "full_access");
    assert_eq!(AccessMode::Default.next(), AccessMode::AutoReview);
    assert_eq!(AccessMode::AutoReview.next(), AccessMode::FullAccess);
    assert_eq!(AccessMode::FullAccess.next(), AccessMode::Default);
}

fn build_control_envelope(allowed_tools: Vec<ToolName>) -> moyai::protocol::TurnControlEnvelope {
    build_control_envelope_with_choice(allowed_tools, ToolChoice::Required)
}

fn build_control_envelope_with_choice(
    allowed_tools: Vec<ToolName>,
    tool_choice: ToolChoice,
) -> moyai::protocol::TurnControlEnvelope {
    let session_id = SessionId::new();
    let projection_id = ProjectionId::new();
    let active_contract = ActiveWorkContractProjection {
        route: TaskRoute::Code,
        process_phase: moyai::session::ProcessPhase::Author,
        active_work_kind: Some("code".to_string()),
        summary: "module responsibility test".to_string(),
        active_targets: vec![Utf8PathBuf::from("src/lib.rs")],
        operation_intents: Vec::new(),
        required_next_action: None,
        required_verification_commands: Vec::new(),
        allowed_tools: allowed_tools.clone(),
        forbidden_tools: Vec::new(),
        projection_id,
    };
    let context = TurnContext {
        session_id,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        workspace_root: Utf8PathBuf::from("C:/workspace/project"),
        provider: "local".to_string(),
        model: "model".to_string(),
        base_url: "http://localhost:1234".to_string(),
        access_mode: AccessMode::AutoReview,
        sandbox: moyai::protocol::SandboxProfile::WorkspaceWrite,
        shell_family: ShellFamily::PowerShell,
        model_capabilities: ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
            parallel_tool_calls: false,
            context_window: 8192,
            max_output_tokens: 1024,
        },
        route: TaskRoute::Code,
        process_phase: moyai::session::ProcessPhase::Author,
        active_contract: active_contract.clone(),
        allowed_tools: allowed_tools.clone(),
        tool_choice,
        images: Vec::new(),
        output_contract: moyai::protocol::OutputContract {
            final_answer_required: true,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: None,
    };
    let obligations = ObligationSet::new(vec![TurnObligation {
        obligation_id: "obligation.module_responsibility".to_string(),
        kind: ObligationKind::UserWork,
        summary: "execute tool lifecycle item".to_string(),
        targets: vec![Utf8PathBuf::from("src/lib.rs")],
        operation_intents: Vec::new(),
        required_actions: Vec::new(),
        verification_commands: Vec::new(),
        contract_refs: Vec::new(),
        evidence_refs: Vec::new(),
        status: ObligationStatus::Open,
    }]);
    let authority =
        ActionAuthority::from_obligations(&context, &obligations, context.tool_choice.clone());
    let bundle = ProjectionBundle::from_authority_and_obligations(&authority, &obligations);

    moyai::protocol::TurnControlEnvelope::new(
        TurnId::new(),
        context,
        obligations,
        authority,
        bundle,
        DispatchPolicy::Dispatch,
        Vec::new(),
    )
}

fn workspace(root: Utf8PathBuf) -> Workspace {
    Workspace {
        project_id: ProjectId::new(),
        root: root.clone(),
        cwd: root.clone(),
        vcs: VcsKind::None,
        ignore: IgnorePlan::default_with(Vec::new()),
        protected_paths: Vec::new(),
        path_policy: PathPolicy {
            workspace_root: root,
            additional_read_roots: Vec::new(),
            additional_write_roots: Vec::new(),
        },
    }
}

fn utf8_path(path: &std::path::Path) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(path.to_path_buf()).expect("test path is valid UTF-8")
}
