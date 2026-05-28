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
    MessagePart, MessageRole, ProjectId, SessionId, SessionRecord, SessionStateSnapshot,
    SessionStatus, TaskRoute, TodoItem, TodoKind, TodoPriority, TodoStatus,
    transcript_from_history_items,
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
    assert!(gate_ids.contains("preflight.protocol.persistence_unit_of_work_authority"));
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
    assert!(gate_ids.contains("preflight.docs_spec.semantic_reconciliation_before_handoff"));
    assert!(gate_ids.contains("preflight.verification.public_command_contract_coverage"));
    assert!(gate_ids.contains("preflight.verification.command_correction_satisfies_obligation"));
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
    assert!(gate_ids.contains(
        "preflight.prompt_replay.openai_compatible_tool_policy_preserves_tool_lifecycle"
    ));
    assert!(gate_ids.contains("preflight.lifecycle_kernel.turn_lifecycle_plan_authority"));
    assert!(gate_ids.contains("preflight.tool_lifecycle.typed_route_metadata_authority"));
    assert!(
        gate_ids.contains("preflight.tool_lifecycle.rejected_singleton_payload_terminal_guard")
    );
    assert!(gate_ids.contains("preflight.tool_lifecycle.pre_execution_corrective_order_authority"));
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
    assert!(gate_ids.contains("preflight.tool_lifecycle.command_text_encoding_contract"));
    assert!(gate_ids.contains("preflight.tool_lifecycle.shell_timeout_process_tree_authority"));
    assert!(gate_ids.contains("preflight.tool_lifecycle.closed_network_shell_authority"));
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
    assert!(gate_ids.contains("preflight.desktop_transcript.completed_primary_reading_path"));
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
fn removed_required_action_string_field_does_not_reappear() {
    let legacy_field = ["required", "action", "label"].join("_");
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .expect("moyAI has repository parent")
        .to_path_buf();
    let roots = [
        manifest_dir.join("src"),
        manifest_dir.join("tests"),
        repo_root.join("docs"),
    ];
    for root in roots {
        assert_no_legacy_required_action_field(&root, &legacy_field);
    }
    for file in ["Kanban.md", "README.md", "ProjectBrief.md"] {
        let path = repo_root.join(file);
        if path.exists() {
            let text = fs::read_to_string(path.as_std_path()).expect("read root document");
            assert!(
                !text.contains(&legacy_field),
                "{path} contains removed field"
            );
        }
    }
}

#[test]
fn vision_input_projection_uses_codex_labeled_image_item() {
    assert!(moyai::agent::prompt::vision_input_provider_projection_fixture_passes());
}

fn assert_no_legacy_required_action_field(root: &Utf8Path, legacy_field: &str) {
    if !root.exists() {
        return;
    }
    let entries = fs::read_dir(root.as_std_path()).expect("read audit root");
    for entry in entries {
        let path = entry.expect("read audit entry").path();
        let path = Utf8PathBuf::from_path_buf(path).expect("utf8 audit path");
        if path.is_dir() {
            assert_no_legacy_required_action_field(&path, legacy_field);
            continue;
        }
        let extension = path.extension().unwrap_or_default();
        if !matches!(extension, "rs" | "md" | "json" | "toml") {
            continue;
        }
        let text = fs::read_to_string(path.as_std_path()).expect("read audit file");
        assert!(
            !text.contains(legacy_field),
            "{path} contains removed field"
        );
    }
}

#[test]
fn shell_guard_blocks_external_setup_and_projects_stdout_stderr() {
    assert!(moyai::tool::shell::external_connection_shell_review_fixture_passes());
    assert!(moyai::tool::shell::shell_output_projection_fixture_passes());
    assert!(moyai::tool::shell::command_text_encoding_contract_fixture_passes());
}

#[test]
fn docs_spec_semantic_reconciliation_rejects_contradictory_docs_before_handoff() {
    assert!(
        moyai::agent::docs_semantic_contract::docs_spec_semantic_reconciliation_fixture_passes()
    );
    assert!(
        moyai::agent::docs_semantic_contract::docs_spec_semantic_reconciliation_tool_fixture_passes(
        )
    );
}

#[test]
fn public_command_contract_coverage_is_not_unittest_only() {
    assert!(moyai::agent::public_command_contract::public_command_contract_fixture_passes());
    assert!(moyai::harness::manual_st::public_command_contract_route_evidence_fixture_passes());
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
fn invalid_tool_recovery_shell_success_does_not_synthesize_closeout() {
    assert!(
        moyai::agent::loop_impl::invalid_tool_recovery_shell_success_does_not_synthesize_closeout_fixture_passes()
    );
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
        "case_progress.json",
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
            && surface.required_action.is_none()
    }));

    let stable = build_control_envelope(vec![ToolName::Read]);
    let validation = stable.validate();
    assert!(validation.passes());

    let surfaces = stable.projection_bundle.rendered_surfaces();
    assert_eq!(surfaces.len(), 5);
    assert!(surfaces.iter().all(|surface| {
        surface.required_action.is_none() && surface.allowed_tools == vec!["read".to_string()]
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
                    text: "older request".to_string(),
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
            payload: HistoryItemPayload::Message {
                message_id: None,
                role: MessageRole::Assistant,
                content: vec![ContentPart::Text {
                    text: "previous answer".to_string(),
                }],
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 12,
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
            sequence_no: 4,
            created_at_ms: 13,
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
    assert!(
        markdown.find("> older request").unwrap()
            < markdown.find("> inspect the workspace").unwrap(),
        "Codex-style bulk export should preserve chronological user turn blocks"
    );
    assert!(
        markdown.find("previous answer").unwrap() > markdown.find("> older request").unwrap()
            && markdown.find("previous answer").unwrap()
                < markdown.find("> inspect the workspace").unwrap(),
        "assistant closeout for the first turn must stay with that turn instead of being folded under the latest request"
    );
    assert!(markdown.contains("### Tool Call: read"));
    assert!(markdown.contains("Tool Lifecycle Decisions"));
    assert!(markdown.contains("allowed_surface"));
    assert!(markdown.contains("<details><summary>実行情報</summary>"));
}

#[test]
fn session_markdown_cancelled_history_uses_terminal_outcome_authority() {
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: ProjectId::new(),
        title: "Cancelled Export".to_string(),
        status: SessionStatus::Cancelled,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "local-model".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: Some(3),
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
                    text: "update the code".to_string(),
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
            payload: HistoryItemPayload::Message {
                message_id: None,
                role: MessageRole::Assistant,
                content: vec![ContentPart::Text {
                    text: "I will adjust the test expectation.".to_string(),
                }],
            },
        },
    ];

    let markdown = history_items_to_markdown(&session, &items);

    assert!(markdown.contains("停止しました: run cancelled by user"));
    assert!(
        markdown
            .find("I will adjust the test expectation.")
            .unwrap()
            < markdown
                .find("停止しました: run cancelled by user")
                .unwrap(),
        "cancelled exports should not expose in-progress assistant intent as the final outcome"
    );
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
                    text: "create test_component.py".to_string(),
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

    assert!(provider_visible_text.contains("test_component.py"));
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
                output_text: "component.py".to_string(),
                metadata: serde_json::json!({"success": true}),
                success: Some(true),
                progress_effect: moyai::protocol::ToolProgressEffect::MadeProgress,
                blocked_action: None,
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
            if replayed == &call_id.to_string() && result.contains("component.py")
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
    let inside = PathGuard::require_path(&workspace, Utf8Path::new("src/lib.rs"), AccessKind::Read)
        .expect("inside workspace path is allowed");
    assert!(inside.inside_workspace);
    assert_eq!(inside.relative_to_root, Utf8PathBuf::from("src/lib.rs"));

    let blocked =
        PathGuard::require_path(&workspace, &external.join("external.txt"), AccessKind::Read);
    assert!(blocked.is_err());

    workspace
        .path_policy
        .additional_read_roots
        .push(external.clone());
    let trusted =
        PathGuard::require_path(&workspace, &external.join("external.txt"), AccessKind::Read)
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

#[test]
fn active_runtime_does_not_reintroduce_old_lifecycle_owners() {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lifecycle = fs::read_to_string(manifest_dir.join("src/agent/lifecycle_kernel.rs"))
        .expect("read lifecycle kernel source");
    let grounding_evidence =
        fs::read_to_string(manifest_dir.join("src/agent/grounding_evidence.rs"))
            .expect("read grounding evidence source");
    let edit_recovery = fs::read_to_string(manifest_dir.join("src/agent/edit_recovery.rs"))
        .expect("read edit recovery source");
    let tool_runtime = fs::read_to_string(manifest_dir.join("src/agent/tool_orchestrator.rs"))
        .expect("read tool lifecycle runtime source");
    let prompt =
        fs::read_to_string(manifest_dir.join("src/agent/prompt.rs")).expect("read prompt source");
    let state =
        fs::read_to_string(manifest_dir.join("src/agent/state.rs")).expect("read state source");
    let loop_impl = fs::read_to_string(manifest_dir.join("src/agent/loop_impl.rs"))
        .expect("read loop implementation source");
    let preflight = fs::read_to_string(manifest_dir.join("src/harness/preflight.rs"))
        .expect("read preflight source");
    let desktop_render_ts = fs::read_to_string(manifest_dir.join("ui/desktop-web/src/render.ts"))
        .expect("read desktop web render source");
    let package_release_ps1 = fs::read_to_string(manifest_dir.join("scripts/package-release.ps1"))
        .expect("read release packaging script");
    let repo_root = manifest_dir.parent().expect("repo root");
    let runtime_contracts = fs::read_to_string(repo_root.join("docs/design/runtime-contracts.md"))
        .expect("read runtime contracts");
    let item_lifecycle_design =
        fs::read_to_string(repo_root.join("docs/design/itemlifecycle-detail-design.md"))
            .expect("read item lifecycle design");
    let loop_runtime_body = loop_impl
        .split("pub(crate) fn terminal_token_accounting_sequence_fixture_passes")
        .next()
        .unwrap_or(loop_impl.as_str());

    assert!(
        !lifecycle.contains("struct ToolLifecycleRuntime"),
        "ToolLifecycleRuntime must not be a lifecycle_kernel wrapper"
    );
    assert!(
        !tool_runtime.contains("struct ToolOrchestrator"),
        "old ToolOrchestrator owner must not be reintroduced"
    );
    assert!(
        tool_runtime.contains("struct ToolLifecycleRuntime"),
        "tool lifecycle implementation must live under the active runtime owner"
    );
    assert!(
        !prompt.contains("legacy_import_transcript")
            && !prompt.contains("from_compatibility_transcript"),
        "runtime input must not accept Transcript as a second authority"
    );
    assert!(
        !state.contains("latest_verification_failure_context(transcript"),
        "repair focus must not be reconstructed from compatibility Transcript"
    );
    for source in [&tool_runtime, &loop_impl] {
        assert!(
            !source.contains("\"owner\": \"tool_orchestrator\""),
            "tool lifecycle metadata must name ToolLifecycleRuntime as owner"
        );
    }
    assert!(
        !loop_impl.contains("fn rejected_tool_no_progress_key")
            && !loop_impl.contains("fn executed_tool_failure_no_progress_key")
            && !loop_impl.contains("fn rejected_tool_result(")
            && !loop_impl.contains("fn wrong_verification_shell_command_result")
            && !loop_impl.contains("fn wrong_verification_command_key")
            && !loop_impl.contains("fn wrong_verification_command_terminal_message")
            && !loop_impl.contains("fn wrong_authoring_target_result")
            && !loop_impl.contains("fn repair_target_authority_violation_result")
            && !loop_impl.contains("fn wrong_authoring_target_key")
            && !loop_impl.contains("fn wrong_authoring_target_terminal_message")
            && !loop_impl.contains("fn submitted_authoring_targets")
            && !loop_impl.contains("fn active_requested_work_targets")
            && !loop_impl.contains("docs_spec_semantic_reconciliation_key")
            && !loop_impl.contains("docs_spec_semantic_reconciliation_terminal_message")
            && !loop_impl.contains("public_command_contract_key")
            && !loop_impl.contains("public_command_contract_terminal_message")
            && !loop_impl.contains("fn artifact_content_shape_violation_result")
            && !loop_impl.contains("fn required_write_content_shape_violation_result")
            && !loop_impl.contains("fn required_write_target_mismatch_content_shape_guidance")
            && !loop_impl.contains("fn write_content_matches_required_target")
            && !loop_impl.contains("fn detected_test_target_forbidden_content_markers")
            && !loop_impl.contains("fn authoring_target_grounding_required_result")
            && !loop_impl.contains("fn generated_test_target_grounding_required_result")
            && !loop_impl.contains("fn authoring_target_grounding_required_key")
            && !loop_impl.contains("fn generated_test_target_grounding_required_key")
            && !loop_impl.contains("fn authoring_target_grounding_required_terminal_message")
            && !loop_impl.contains("fn generated_test_target_grounding_required_terminal_message")
            && !loop_impl.contains("struct AuthoringGroundingRecoveryEnvelope")
            && !loop_impl.contains("fn docs_route_supporting_context_budget_key")
            && !loop_impl.contains("fn docs_supporting_context_budget_exhausted_result")
            && !loop_impl.contains("fn docs_supporting_context_budget_exhausted_terminal_message")
            && !loop_impl.contains("fn operation_non_content_no_progress_key")
            && !loop_impl.contains("fn operation_non_content_no_progress_terminal_message")
            && !loop_impl.contains("fn record_corrective_content_shape_no_progress")
            && !loop_impl.contains("fn verification_supporting_context_no_progress_key")
            && !loop_impl
                .contains("fn verification_supporting_context_no_progress_terminal_message")
            && !loop_impl.contains("fn same_verification_failure_no_progress_key")
            && !loop_impl.contains("fn same_verification_failure_terminal_message"),
        "generic tool rejection / accepted-call validation guards must not live in TurnRuntime"
    );
    assert!(
        loop_impl.contains("compile_turn_lifecycle_plan")
            && !loop_impl.contains("fn tool_choice_for_dispatch")
            && !loop_impl.contains("fn provider_noncompliance_edit_recovery_tool_choice")
            && !loop_impl.contains("fn bounded_code_authoring_recovery_tool_choice")
            && !loop_impl.contains("fn docs_route_content_grounding_recovery_tool_choice")
            && !loop_impl.contains("fn open_obligation_final_message_recovery_tool_choice")
            && !loop_impl.contains("fn tool_choice_from_policy"),
        "dispatch tool_choice policy must be owned by lifecycle_kernel TurnLifecyclePlan, not TurnRuntime helpers"
    );
    assert!(
        loop_runtime_body.contains("classify_pre_execution_corrective_result")
            && loop_runtime_body.contains("record_pre_execution_corrective_no_progress")
            && !loop_runtime_body.contains("PreExecutionCorrectiveKind::")
            && !loop_runtime_body.contains("artifact_content_shape_violation_result(")
            && !loop_runtime_body.contains("repair_target_authority_violation_result(")
            && !loop_runtime_body.contains("repair_active_shell_probe_target_result(")
            && !loop_runtime_body.contains("wrong_authoring_target_result(")
            && !loop_runtime_body.contains("docs_spec_semantic_reconciliation_result(")
            && !loop_runtime_body.contains("public_command_contract_result(")
            && !loop_runtime_body.contains("wrong_verification_shell_command_result("),
        "pre-execution corrective result ordering must be owned by ToolLifecycleRuntime, not TurnRuntime branch cascade"
    );
    assert!(
        lifecycle.contains("replay_policy")
            && lifecycle.contains("proposal_policy")
            && lifecycle.contains("corrective_policy")
            && lifecycle.contains("terminal_policy")
            && lifecycle.contains("continuation_expectation")
            && lifecycle.contains("diagnostics_projection"),
        "TurnLifecyclePlan must own replay, proposal, corrective, terminal, continuation, and diagnostics policy projections, not only tool_choice"
    );
    assert!(
        lifecycle.contains("apply_codex_style_provider_edit_surface")
            && lifecycle.contains("apply_pre_normalization_recovery_surface")
            && lifecycle.contains("apply_post_normalization_recovery_surface")
            && lifecycle.contains("open_executable_work_requires_tool_call")
            && lifecycle.contains("closeout_ready_final_message_authority")
            && lifecycle.contains("docs_route_supporting_context_budget_recovery_surface_active")
            && lifecycle.contains("authoring_supporting_context_budget_recovery_surface_active")
            && lifecycle.contains("repair_supporting_context_budget_recovery_surface_active")
            && lifecycle.contains("verification_repair_target_grounding_surface_active")
            && lifecycle.contains("provider_noncompliance_edit_recovery_applies")
            && lifecycle.contains("docs_route_supporting_context_budget_recovery_tool_visible")
            && lifecycle.contains("authoring_supporting_context_budget_recovery_tool_visible")
            && lifecycle.contains("repair_supporting_context_budget_recovery_tool_visible")
            && lifecycle.contains("verification_repair_target_grounding_surface_tool_visible")
            && lifecycle.contains("provider_noncompliance_edit_recovery_tool_visible")
            && lifecycle.contains("provider_noncompliance_edit_recovery_policy")
            && lifecycle.contains("malformed_write_patch_capable_recovery_policy")
            && lifecycle.contains("malformed_apply_patch_write_recovery_policy")
            && lifecycle.contains("invalid_edit_arguments_control_recovery_policy")
            && lifecycle.contains("provider_required_tool_choice_final_message_noncompliance")
            && lifecycle
                .contains("provider_required_tool_choice_final_message_recovery_has_write_surface")
            && lifecycle.contains("provider_required_tool_choice_final_message_recovery_policy")
            && lifecycle.contains("docs_route_requires_content_grounding_before_write")
            && lifecycle.contains("authoring_target_grounding_final_message_recovery_active")
            && lifecycle.contains("generated_test_source_reference_grounding_active")
            && lifecycle.contains("generated_test_reference_consumed_target_grounding_active")
            && lifecycle.contains("singleton_missing_authoring_target_create_action_active")
            && grounding_evidence.contains("docs_route_has_required_content_grounding_evidence")
            && grounding_evidence.contains("authoring_missing_grounding_targets")
            && grounding_evidence.contains("history_has_unread_source_change_for_generated_test")
            && grounding_evidence
                .contains("history_has_current_source_reference_read_for_generated_test")
            && grounding_evidence.contains("singleton_active_target_exists")
            && grounding_evidence.contains("metadata_path_matches_active_target")
            && edit_recovery.contains("struct InvalidEditRecoveryEnvelope")
            && edit_recovery.contains("invalid_edit_arguments_control_recovery_envelope")
            && edit_recovery.contains("invalid_tool_arguments_result")
            && edit_recovery.contains("invalid_edit_arguments_no_progress_key")
            && edit_recovery.contains("record_patch_context_mismatch_grounding_targets")
            && edit_recovery.contains("patch_context_mismatch_target_grounding_surface_active")
            && edit_recovery.contains("patch_context_mismatch_target_grounding_read_satisfied")
            && edit_recovery.contains("repair_write_arguments_from_active_target")
            && edit_recovery.contains("struct EscapedSourceWriteCandidate")
            && edit_recovery.contains("normalized_escaped_source_write_candidate")
            && runtime_contracts.contains("`edit_recovery` module")
            && item_lifecycle_design.contains("`edit_recovery` owns")
            && lifecycle.contains("docs_route_content_grounding_recovery_tool_visible")
            && lifecycle.contains("generated_test_source_reference_grounding_tool_visible")
            && lifecycle.contains("authoring_target_grounding_recovery_tool_visible")
            && lifecycle.contains("open_obligation_final_message_recovery_tool_visible")
            && lifecycle.contains(
                "code_authoring_open_obligation_final_message_recovery_uses_stable_surface"
            )
            && lifecycle
                .contains("code_repair_open_obligation_final_message_recovery_uses_stable_surface")
            && lifecycle.contains("singleton_missing_authoring_target_create_action_tool_visible")
            && lifecycle.contains("apply_generated_test_source_reference_grounding_surface")
            && lifecycle
                .contains("apply_generated_test_reference_consumed_target_grounding_surface")
            && !prompt.contains("fn apply_codex_style_provider_edit_surface")
            && !loop_impl.contains("fn open_executable_work_requires_tool_call")
            && !loop_impl.contains("fn closeout_ready_final_message_authority")
            && !loop_impl.contains("fn answer_only_final_message_authority")
            && !loop_impl.contains("fn clean_closeout_final_message_lifecycle(")
            && !loop_impl
                .contains("fn docs_route_supporting_context_budget_recovery_surface_active")
            && !loop_impl
                .contains("fn authoring_supporting_context_budget_recovery_surface_active")
            && !loop_impl.contains("fn repair_supporting_context_budget_recovery_surface_active")
            && !loop_impl.contains("fn verification_repair_target_grounding_surface_active")
            && !loop_impl.contains("fn provider_noncompliance_edit_recovery_applies")
            && !loop_impl.contains("fn provider_noncompliance_edit_recovery_policy")
            && !loop_impl.contains("fn malformed_write_patch_capable_recovery_policy")
            && !loop_impl.contains("fn malformed_apply_patch_write_recovery_policy")
            && !loop_impl.contains("fn invalid_edit_arguments_control_recovery_policy")
            && !loop_impl.contains("fn provider_required_tool_choice_final_message_noncompliance")
            && !loop_impl.contains(
                "fn provider_required_tool_choice_final_message_recovery_has_write_surface"
            )
            && !loop_impl
                .contains("fn provider_required_tool_choice_final_message_recovery_policy")
            && !loop_impl.contains("fn docs_route_requires_content_grounding_before_write")
            && !loop_impl.contains("fn authoring_final_message_target_grounding_recovery_active")
            && !loop_impl.contains("fn generated_test_authoring_source_reference_grounding_active")
            && !loop_impl
                .contains("fn generated_test_source_reference_consumed_target_grounding_active")
            && !loop_impl.contains("fn singleton_missing_authoring_target_create_action_active")
            && !loop_impl.contains("fn docs_route_has_required_content_grounding_evidence")
            && !loop_impl.contains("fn authoring_missing_grounding_targets")
            && !loop_impl.contains("fn history_has_unread_source_change_for_generated_test")
            && !loop_impl
                .contains("fn history_has_current_source_reference_read_for_generated_test")
            && !loop_impl.contains("fn singleton_active_target_exists")
            && !loop_impl.contains("fn metadata_path_matches_active_target")
            && !loop_impl.contains("fn normalize_path_for_target_match")
            && !loop_impl.contains("struct InvalidEditRecoveryEnvelope")
            && !loop_impl.contains("fn invalid_edit_arguments_control_recovery_envelope")
            && !loop_impl.contains("fn invalid_tool_arguments_result")
            && !loop_impl.contains("fn invalid_edit_arguments_no_progress_key")
            && !loop_impl.contains("fn record_patch_context_mismatch_grounding_targets")
            && !loop_impl.contains("fn patch_context_mismatch_target_grounding_surface_active")
            && !loop_impl.contains("fn patch_context_mismatch_target_grounding_read_satisfied")
            && !loop_impl.contains("fn repair_write_arguments_from_active_target")
            && !loop_impl.contains("struct EscapedSourceWriteCandidate")
            && !loop_impl.contains("fn normalized_escaped_source_write_candidate")
            && !loop_impl.contains("fn docs_route_supporting_context_budget_recovery_tool_visible")
            && !loop_impl.contains("fn authoring_supporting_context_budget_recovery_tool_visible")
            && !loop_impl.contains("fn repair_supporting_context_budget_recovery_tool_visible")
            && !loop_impl.contains("fn verification_repair_target_grounding_surface_tool_visible")
            && !loop_impl.contains("fn provider_noncompliance_edit_recovery_tool_visible")
            && !loop_impl.contains("fn docs_route_content_grounding_recovery_tool_visible")
            && !loop_impl.contains("fn generated_test_source_reference_grounding_tool_visible")
            && !loop_impl.contains("fn authoring_target_grounding_recovery_tool_visible")
            && !loop_impl.contains("fn open_obligation_final_message_recovery_tool_visible")
            && !loop_impl.contains(
                "fn code_authoring_open_obligation_final_message_recovery_uses_stable_surface"
            )
            && !loop_impl.contains(
                "fn code_repair_open_obligation_final_message_recovery_uses_stable_surface"
            )
            && !loop_impl
                .contains("fn singleton_missing_authoring_target_create_action_tool_visible")
            && !loop_impl.contains("fn apply_generated_test_source_reference_grounding_surface")
            && !loop_impl
                .contains("fn apply_generated_test_reference_consumed_target_grounding_surface")
            && !loop_impl.contains("fn augment_tools_from_stable_surface")
            && !loop_impl.contains("crate::agent::prompt::apply_codex_style_provider_edit_surface"),
        "provider edit surface, recovery surface reconstruction, executable-work / grounding predicates, and tool-visible surface renderers must be lifecycle-kernel policy, not PromptBuilder or TurnRuntime compatibility code"
    );
    assert!(
        preflight
            .contains("kernel-owned TurnLifecyclePlan decides dispatch tool_choice, replay policy, proposal policy, corrective policy, terminal policy, continuation expectation, and diagnostics projection")
            && preflight
                .contains("turn_lifecycle_plan_owns_dispatch_tool_choice_fixture_passes")
            && preflight.contains(
                "tool_orchestrator::pre_execution_corrective_order_authority_fixture_passes"
            ),
        "preflight must assert owner-module lifecycle invariants, not only loop branch outcomes"
    );
    assert!(
        !desktop_render_ts.contains("shouldHidePseudoToolCallTranscriptRow")
            && !desktop_render_ts.contains("body.includes(\"<tool_call>\")")
            && !desktop_render_ts.contains("body.includes(\"<function=\")"),
        "Desktop Web UI must not hide pseudo tool-call text with client-side substring filters; typed Rust transcript projection owns stale tool-call normalization"
    );
    assert!(
        !desktop_render_ts.contains("parseFileChangesFromTranscriptBody")
            && !desktop_render_ts.contains("normalizedTranscriptFileChanges")
            && !desktop_render_ts.contains(".match(/^[-*]"),
        "Desktop Web UI must not synthesize file-change rows from transcript body text; typed Rust transcript projection owns file-change item authority"
    );
    assert!(
        package_release_ps1.contains("function Assert-ReleaseOutputPath")
            && package_release_ps1.contains("function Remove-ReleaseOutputDirectory")
            && package_release_ps1.contains("function Remove-ReleaseOutputFile")
            && !package_release_ps1.contains("Remove-Item -LiteralPath $releaseRoot -Recurse"),
        "release packaging cleanup must validate computed artifact paths before recursive deletion"
    );
    assert!(
        runtime_contracts.contains("## 1.2 Archived FR Chronology Boundary")
            && runtime_contracts
                .contains("FR-specific chronology sections are archived incident evidence")
            && item_lifecycle_design.contains("Archived FR Chronology Boundary")
            && item_lifecycle_design.contains("proposal policy")
            && item_lifecycle_design.contains("diagnostics projection"),
        "design docs must make current Codex-style authority explicit before historical FR wording"
    );
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
