use std::collections::BTreeSet;
use std::fs;
use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use moyai::agent::prompt::build_provider_replay_messages_from_history_items;
use moyai::config::{AccessMode, ProviderMetadataMode, ResolvedConfig, ShellFamily};
use moyai::harness::{
    HarnessRunId, HarnessRunRecord, HarnessRunStatus, HarnessRunStore, ReplayReport,
    ReplayReportStore, ReplayStatus, SqliteHarnessRunStore, SqliteReplayReportStore,
    preflight::{
        PreflightGateFamily, PreflightResultStatus, run_artifact_replay_preflight,
        run_default_active_preflight,
    },
};
use moyai::llm::ModelMessage;
use moyai::protocol::{
    ActionAuthority, ActiveWorkContractProjection, ContentPart, DispatchPolicy, HistoryItem,
    HistoryItemId, HistoryItemPayload, ModelCapabilities, ObligationKind, ObligationSet,
    ObligationStatus, ProjectionBundle, ProjectionId, ToolChoice, TurnContext, TurnId,
    TurnObligation,
    protocol_tool_call_arguments_do_not_fallback_to_legacy_display_projection_fixture_passes,
};
use moyai::session::{
    MessagePart, MessageRole, ProjectId, SessionId, SessionRecord, SessionStateSnapshot,
    SessionStatus, TaskRoute, TodoItem, TodoKind, TodoPriority, TodoStatus,
    transcript_from_history_items,
};
use moyai::session::{history_items_to_markdown, todo_counts_as_open_work};
use moyai::tool::ToolName;
use moyai::workspace::{AccessKind, IgnorePlan, PathGuard, PathPolicy, VcsKind, Workspace};
use rusqlite::{Connection, params};
use tempfile::TempDir;

const CURRENT_PROVIDER_PROFILE_PROVIDER: &str = "lm_studio_native_required";
const CURRENT_PROVIDER_PROFILE_MODEL: &str = "qwen/qwen3.6-35b-a3b";
const CURRENT_PROVIDER_PROFILE_BASE_URL: &str = "http://127.0.0.1:1234";
const CURRENT_PROVIDER_PROFILE_CONTEXT_WINDOW: u32 = 131072;
const CURRENT_PROVIDER_PROFILE_MAX_OUTPUT_TOKENS: u32 = 8192;

fn docs_contains_or_item_lifecycle_current_authority(docs: &str, marker: &str) -> bool {
    docs.contains(marker)
        || (docs.starts_with("# Item Lifecycle Detail Design")
            && moyai::harness::preflight::item_lifecycle_detail_current_authority_fixture_passes())
}

#[test]
fn streaming_tool_call_late_name_preserves_typed_tool_identity() {
    assert!(
        moyai::llm::openai_compat::streaming_tool_call_late_name_preserves_typed_tool_identity_fixture_passes()
    );
}

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
    let progress_projection_gate = report
        .results
        .iter()
        .find(|result| {
            result.gate_id == "preflight.tool_lifecycle.progress_projection_stable_surface_guard"
        })
        .expect("progress projection stable surface preflight gate");
    assert!(
        progress_projection_gate
            .evidence_refs
            .iter()
            .any(|evidence| evidence == "grounding_metadata_path_target_identity_exact"),
        "progress projection active preflight evidence must expose grounding metadata exact target identity"
    );
    assert!(
        progress_projection_gate
            .evidence_refs
            .iter()
            .any(|evidence| evidence == "docs_route_grep_line_path_generic_path_line"),
        "progress projection active preflight evidence must expose generic docs-route grep path-line parsing"
    );
    assert!(gate_ids.contains("preflight.tool_lifecycle.edit_surface_registry_symmetry"));
    assert!(gate_ids.contains("preflight.tool_lifecycle.rejected_tool_semantic_terminal_guard"));
    assert!(
        gate_ids.contains("preflight.tool_lifecycle.synthetic_feedback_not_verification_authority")
    );
    assert!(gate_ids.contains("preflight.tool_lifecycle.workspace_relative_file_change_authority"));
    assert!(gate_ids.contains("preflight.tool_lifecycle.command_text_encoding_contract"));
    assert!(gate_ids.contains("preflight.tool_lifecycle.shell_timeout_process_tree_authority"));
    assert!(gate_ids.contains("preflight.tool_lifecycle.closed_network_shell_authority"));
    assert!(
        gate_ids.contains("preflight.tool_lifecycle.full_access_configured_boundary_authority")
    );
    let full_access_boundary_gate = report
        .results
        .iter()
        .find(|result| {
            result.gate_id == "preflight.tool_lifecycle.full_access_configured_boundary_authority"
        })
        .expect("full access configured boundary preflight gate");
    assert!(
        full_access_boundary_gate
            .evidence_refs
            .iter()
            .any(|evidence| evidence == "outside_workspace_review_required"),
        "full access active preflight evidence must expose outside-workspace review boundary"
    );
    assert!(
        full_access_boundary_gate
            .evidence_refs
            .iter()
            .any(|evidence| evidence == "protected_workspace_authority_review_required"),
        "full access active preflight evidence must expose protected workspace authority review boundary"
    );
    assert!(
        full_access_boundary_gate
            .evidence_refs
            .iter()
            .any(|evidence| evidence == "network_review_required"),
        "full access active preflight evidence must expose network review boundary"
    );
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
    let desktop_transcript_gate = report
        .results
        .iter()
        .find(|result| {
            result.gate_id == "preflight.desktop_transcript.completed_primary_reading_path"
        })
        .expect("desktop transcript preflight gate");
    assert!(
        desktop_transcript_gate
            .evidence_refs
            .iter()
            .any(|evidence| {
                evidence
                    == "session_markdown_legacy_toolcall_display_arguments_not_typed_projection"
            }),
        "desktop transcript active preflight evidence must expose session Markdown legacy ToolCall display-argument separation"
    );
    assert!(
        desktop_transcript_gate
            .evidence_refs
            .iter()
            .any(|evidence| evidence == "session_service_fixture_current_provider_profile"),
        "desktop transcript active preflight evidence must expose session service current provider-profile fixture"
    );
    assert!(
        desktop_transcript_gate
            .evidence_refs
            .iter()
            .any(|evidence| evidence == "storage_repository_fixture_current_provider_profile"),
        "desktop transcript active preflight evidence must expose storage repository current provider-profile fixture"
    );
    assert!(gate_ids.contains("preflight.route_evidence.schema"));
    let route_evidence_gate = report
        .results
        .iter()
        .find(|result| result.gate_id == "preflight.route_evidence.schema")
        .expect("route evidence schema preflight gate");
    assert!(
        route_evidence_gate
            .evidence_refs
            .iter()
            .any(|evidence| evidence == "stored_artifact_classifier_fixture_language_neutral"),
        "route evidence active preflight evidence must expose stored artifact classifier fixture language neutrality"
    );
    assert!(
        route_evidence_gate
            .evidence_refs
            .iter()
            .any(|evidence| evidence == "harness_replay_report_latest_run_lifecycle_order"),
        "route evidence active preflight evidence must expose harness replay report latest run lifecycle ordering"
    );
    let llm_stream_gate = report
        .results
        .iter()
        .find(|result| result.gate_id == "preflight.llm_transport.stream_retry_before_first_event")
        .expect("llm stream retry preflight gate");
    assert!(
        llm_stream_gate
            .evidence_refs
            .iter()
            .any(|evidence| evidence == "streaming_tool_call_late_name_typed_identity"),
        "llm stream active preflight evidence must expose late-name typed tool-call identity"
    );

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
fn public_command_feedback_templates_follow_target_language() {
    assert!(
        moyai::agent::public_command_contract::public_command_feedback_templates_follow_target_language_fixture_passes()
    );
}

#[test]
fn prompt_provider_replay_fixtures_use_current_provider_profile() {
    assert!(
        moyai::agent::prompt::prompt_provider_replay_fixtures_use_current_provider_profile_fixture_passes()
    );

    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let prompt_path = manifest_dir.join("src").join("agent").join("prompt.rs");
    let prompt = fs::read_to_string(prompt_path.as_std_path()).expect("read prompt source");
    let inactive_replay_block = prompt
        .split("pub(crate) fn stale_inactive_authoring_replay_uses_live_builder")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn failed_inactive_authoring_feedback_requires_typed_metadata")
                .next()
        })
        .expect("inactive authoring provider replay fixture block");
    for forbidden in [
        "model: \"local\".to_string()",
        "base_url: \"http://localhost:1234\".to_string()",
    ] {
        assert!(
            !inactive_replay_block.contains(forbidden),
            "inactive-authoring prompt provider replay fixtures must use current provider profile constants, not `{forbidden}`"
        );
    }
    assert!(
        inactive_replay_block.contains("PROMPT_FIXTURE_MODEL.to_string()")
            && inactive_replay_block.contains("PROMPT_FIXTURE_BASE_URL.to_string()"),
        "inactive-authoring prompt provider replay fixtures must project current provider profile constants"
    );
}

#[test]
fn prompt_artifact_target_kind_fixture_is_workflow_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().expect("workspace root");
    let prompt_path = manifest_dir.join("src").join("agent").join("prompt.rs");
    let prompt = fs::read_to_string(prompt_path.as_std_path()).expect("read prompt source");
    let fixture_block = prompt
        .split("pub(crate) fn prompt_artifact_target_kind_uses_language_adapter_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn verification_repair_prompt_uses_language_projection_fixture_passes",
            )
            .next()
        })
        .expect("prompt artifact target kind fixture block");

    for forbidden in ["widget", "widget-design", "src/widget", "docs/widget"] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic prompt artifact-target fixture must not use widget-domain surface `{forbidden}` as language-adapter authority"
        );
    }
    for required in [
        "src/workflow.ts",
        "tests/workflow.spec.tsx",
        "docs/workflow-design.md",
    ] {
        assert!(
            fixture_block.contains(required),
            "generic prompt artifact-target fixture must contain workflow-neutral surface `{required}`"
        );
    }

    for relative in [
        "docs/testing/PreflightGateSuite.md",
        "docs/design/runtime-contracts.md",
        "docs/design/detailed-design.md",
        "docs/design/itemlifecycle-detail-design.md",
    ] {
        let docs = fs::read_to_string(workspace_root.join(relative).as_std_path())
            .expect("read docs/design sync file");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "prompt_artifact_target_kind_fixture_workflow_neutral",
            ),
            "docs/design sync file `{relative}` must describe prompt artifact-target fixture workflow neutrality"
        );
    }
}

#[test]
fn stored_artifact_classifier_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().expect("workspace root");
    let stored_artifact_path = manifest_dir
        .join("src")
        .join("harness")
        .join("stored_artifact.rs");
    let stored_artifact =
        fs::read_to_string(stored_artifact_path.as_std_path()).expect("read stored artifact");
    let fixture_block = stored_artifact
        .split("fn stored_artifact_classifier_does_not_treat_request_named_outputs_as_diagnostics")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn stored_artifact_classifier_keeps_explicit_request_diagnostics")
                .next()
        })
        .expect("stored artifact classifier request-name fixture block");

    for forbidden in ["request_handler.py", ".py"] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic stored-artifact classifier fixture must not use Python-specific coordinate `{forbidden}` as request-name evidence"
        );
    }
    assert!(
        fixture_block.contains("workspace/request-handler.source"),
        "generic stored-artifact classifier fixture must use a language-neutral source artifact coordinate"
    );

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    assert!(
        preflight.contains("stored_artifact_classifier_fixture_language_neutral"),
        "active preflight route evidence schema must project stored artifact classifier fixture neutrality"
    );

    for relative in [
        "docs/testing/PreflightGateSuite.md",
        "docs/design/runtime-contracts.md",
        "docs/design/detailed-design.md",
        "docs/design/itemlifecycle-detail-design.md",
    ] {
        let path = workspace_root.join(relative);
        let docs = fs::read_to_string(path.as_std_path()).expect("read docs/design sync file");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "stored_artifact_classifier_fixture_language_neutral",
            ),
            "{relative} must document stored artifact classifier fixture neutrality"
        );
    }
}

#[test]
fn replay_report_latest_for_session_uses_run_lifecycle_order() {
    let temp_dir = TempDir::new().expect("temp dir");
    let database_path = temp_dir.path().join("reports.sqlite3");
    let connection = Connection::open(database_path).expect("open sqlite");
    moyai::storage::migration::run(&connection).expect("run migrations");
    let connection = Arc::new(Mutex::new(connection));
    let run_store = SqliteHarnessRunStore::new(connection.clone());
    let report_store = SqliteReplayReportStore::new(connection.clone());

    let project_id = ProjectId::new();
    let session_id = SessionId::new();
    {
        let connection = connection.lock().expect("sqlite mutex");
        connection
            .execute(
                "INSERT INTO projects (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    project_id.to_string(),
                    "workspace-root",
                    "Workflow",
                    "none",
                    1_i64,
                    1_i64,
                ],
            )
            .expect("insert project");
        connection
            .execute(
                "INSERT INTO sessions (id, project_id, title, status, cwd_path, model_name, base_url, created_at_ms, updated_at_ms, completed_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL)",
                params![
                    session_id.to_string(),
                    project_id.to_string(),
                    "Replay report order",
                    "idle",
                    "workspace-root",
                    CURRENT_PROVIDER_PROFILE_MODEL,
                    CURRENT_PROVIDER_PROFILE_BASE_URL,
                    1_i64,
                    1_i64,
                ],
            )
            .expect("insert session");
    }

    let first_run_id = HarnessRunId::new();
    let second_run_id = HarnessRunId::new();
    for (run_id, started_at_ms, completed_at_ms) in
        [(first_run_id, 1_000, 2_000), (second_run_id, 3_000, 4_000)]
    {
        run_store
            .upsert_run(&HarnessRunRecord {
                id: run_id,
                session_id: Some(session_id),
                workspace_root: Utf8PathBuf::from(format!("workspace-{started_at_ms}")),
                artifact_root: Utf8PathBuf::from(format!("artifacts-{started_at_ms}")),
                mode: "replay".to_string(),
                started_at_ms,
                completed_at_ms: Some(completed_at_ms),
                status: HarnessRunStatus::Pass,
            })
            .expect("insert harness run");
    }

    for (run_id, summary) in [
        (first_run_id, "older report"),
        (second_run_id, "newer report"),
    ] {
        report_store
            .save_report(&ReplayReport {
                schema_version: "test".to_string(),
                run_id,
                status: ReplayStatus::Pass,
                primary_owner: None,
                summary: summary.to_string(),
                gate_results: Vec::new(),
                restart_point: None,
                next_actions: Vec::new(),
            })
            .expect("save replay report");
    }

    connection
        .lock()
        .expect("sqlite mutex")
        .execute(
            "UPDATE harness_replay_reports SET created_at_ms = ?1",
            params![9_000_i64],
        )
        .expect("force identical report timestamps");

    let latest = report_store
        .latest_report_for_session(session_id)
        .expect("load latest report")
        .expect("latest report exists");

    assert_eq!(
        latest.run_id, second_run_id,
        "latest report must follow harness run lifecycle order when report write timestamps tie"
    );
    assert_eq!(latest.summary, "newer report");

    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    assert!(
        preflight.contains("harness_replay_report_latest_run_lifecycle_order"),
        "active preflight route evidence schema must project replay report latest run lifecycle ordering"
    );

    let workspace_root = manifest_dir.parent().expect("workspace root");
    for relative in [
        "docs/testing/PreflightGateSuite.md",
        "docs/design/runtime-contracts.md",
        "docs/design/detailed-design.md",
        "docs/design/itemlifecycle-detail-design.md",
    ] {
        let path = workspace_root.join(relative);
        let docs = fs::read_to_string(path.as_std_path()).expect("read docs/design sync file");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "harness_replay_report_latest_run_lifecycle_order",
            ),
            "{relative} must document replay report latest run lifecycle ordering"
        );
    }
}

#[test]
fn storage_repository_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let project_repo_path = repo_root
        .join("src")
        .join("storage")
        .join("project_repo.rs");
    let session_repo_path = repo_root
        .join("src")
        .join("storage")
        .join("session_repo.rs");
    let module_guard_path = repo_root.join("tests").join("module_responsibility.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");

    let project_repo =
        fs::read_to_string(project_repo_path.as_std_path()).expect("read project repo");
    let session_repo =
        fs::read_to_string(session_repo_path.as_std_path()).expect("read session repo");
    let module_guard =
        fs::read_to_string(module_guard_path.as_std_path()).expect("read module guard");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");

    let project_fixture_block = project_repo
        .split("fn delete_project_removes_child_sessions_without_touching_other_projects")
        .nth(1)
        .expect("project delete fixture block");
    let storage_fixture_block = session_repo
        .split("pub(crate) fn todo_update_uses_single_unit_of_work_fixture_passes")
        .nth(1)
        .expect("storage session fixture block");
    let helper_block = module_guard
        .split("fn build_control_envelope_with_choice")
        .nth(1)
        .and_then(|tail| tail.split("fn workspace").next())
        .expect("module responsibility control envelope helper block");

    for (label, block) in [
        ("project repository fixture", project_fixture_block),
        ("storage session repository fixture", storage_fixture_block),
        ("module responsibility helper", helper_block),
    ] {
        for forbidden in [
            "http://localhost:1234",
            "model: \"model\"",
            "provider: \"local\"",
            "context_window: 8192",
            "max_output_tokens: 1024",
        ] {
            assert!(
                !block.contains(forbidden),
                "{label} must not retain stale provider-profile authority `{forbidden}`"
            );
        }
    }

    for required_surface in [
        CURRENT_PROVIDER_PROFILE_MODEL,
        CURRENT_PROVIDER_PROFILE_BASE_URL,
        "storage_repository_current_provider_profile_fixture_passes",
        "storage_repository_fixture_current_provider_profile",
    ] {
        assert!(
            project_repo.contains(required_surface)
                || session_repo.contains(required_surface)
                || module_guard.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "storage repository fixture, module guard, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn session_markdown_legacy_toolcall_display_arguments_are_not_typed_projection() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let markdown_path = manifest_dir.join("src").join("session").join("markdown.rs");
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let markdown = fs::read_to_string(markdown_path.as_std_path()).expect("read session markdown");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let markdown = markdown.replace("\r\n", "\n");
    let materialized_toolcall_block = markdown
        .split("MessagePart::ToolCall(value) => Some(HistoryItemPayload::ToolCall")
        .nth(1)
        .and_then(|tail| tail.split("MessagePart::ToolResult(value)").next())
        .expect("compatibility transcript ToolCall materialization block");
    let effective_arguments_block = materialized_toolcall_block
        .split("effective_arguments: value")
        .nth(1)
        .and_then(|tail| tail.split("adjusted_arguments: None").next())
        .expect("compatibility transcript ToolCall effective arguments materialization block");

    assert!(
        !effective_arguments_block.contains("serde_json::from_str(&value.arguments_json)"),
        "compatibility transcript ToolCall display arguments_json must not populate typed effective/model argument projection fields"
    );
    assert!(
        effective_arguments_block.contains(".unwrap_or(Value::Null)"),
        "missing typed effective_arguments_json must remain null instead of falling back to display arguments_json"
    );
    assert!(
        markdown.contains(
            "session_markdown_legacy_toolcall_arguments_do_not_render_typed_projection_fixture_passes"
        ),
        "session Markdown must expose an executable fixture for legacy ToolCall display-argument separation"
    );
    assert!(
        preflight
            .contains("session_markdown_legacy_toolcall_display_arguments_not_typed_projection"),
        "active preflight must expose the session Markdown legacy ToolCall display-argument separation marker"
    );
}

#[test]
fn session_service_fixtures_use_current_provider_profile() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let service_path = manifest_dir.join("src").join("session").join("service.rs");
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let service = fs::read_to_string(service_path.as_std_path()).expect("read session service");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let service = service.replace("\r\n", "\n");
    let stale_cleanup_block = service
        .split("pub(crate) fn stale_running_cleanup_records_protocol_terminal_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("stale running cleanup executable fixture block");
    let stale_cleanup_test_block = service
        .split("stale_running_sessions_are_cancelled_on_desktop_restart_cleanup")
        .nth(1)
        .expect("stale running cleanup unit test block");

    for block in [stale_cleanup_block, stale_cleanup_test_block] {
        assert!(
            !block.contains("http://localhost:1234"),
            "generic session service fixture must not use stale localhost provider profile"
        );
        assert!(
            !block.contains("model: \"local\""),
            "generic session service fixture must not use stale local model profile"
        );
        assert!(
            block.contains("SESSION_SERVICE_FIXTURE_BASE_URL"),
            "generic session service fixture must use the shared current closed-network base URL constant"
        );
        assert!(
            block.contains("SESSION_SERVICE_FIXTURE_MODEL"),
            "generic session service fixture must use the shared current closed-network model id constant"
        );
    }
    assert!(
        service
            .contains("const SESSION_SERVICE_FIXTURE_BASE_URL: &str = \"http://127.0.0.1:1234\";"),
        "session service fixture base URL constant must match the current closed-network provider profile"
    );
    assert!(
        service.contains("const SESSION_SERVICE_FIXTURE_MODEL: &str = \"qwen/qwen3.6-35b-a3b\";"),
        "session service fixture model constant must match the current closed-network provider profile"
    );
    assert!(
        preflight.contains("session_service_fixture_current_provider_profile"),
        "active preflight must expose the session service current provider-profile fixture marker"
    );
}

#[test]
fn desktop_startup_fixtures_use_current_provider_profile() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().expect("workspace root");
    let startup_path = manifest_dir.join("src").join("desktop").join("startup.rs");
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let startup = fs::read_to_string(startup_path.as_std_path()).expect("read desktop startup");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let startup = startup.replace("\r\n", "\n");
    let fixture_block = startup
        .split("pub(crate) fn desktop_startup_uses_model_availability_report_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("Desktop startup model availability fixture block");
    let unit_test_block = startup
        .split("#[cfg(test)]")
        .nth(1)
        .expect("Desktop startup unit test fixture block");
    let provider_fixture_surface = format!("{fixture_block}\n{unit_test_block}");

    for forbidden in [
        "qwen/example",
        "http://127.0.0.1:1234",
        "context_window: Some(4096)",
        "max_output_tokens: Some(1024)",
    ] {
        assert!(
            !provider_fixture_surface.contains(forbidden),
            "Desktop startup provider readiness fixtures must not retain stale provider profile authority `{forbidden}`"
        );
    }
    for required_surface in [
        "DESKTOP_STARTUP_FIXTURE_MODEL",
        "DESKTOP_STARTUP_FIXTURE_BASE_URL",
        "DESKTOP_STARTUP_FIXTURE_CONTEXT_WINDOW",
        "DESKTOP_STARTUP_FIXTURE_MAX_OUTPUT_TOKENS",
        "desktop_startup_fixture_current_provider_profile_fixture_passes",
    ] {
        assert!(
            startup.contains(required_surface),
            "Desktop startup source must project current provider profile fixture surface `{required_surface}`"
        );
    }
    assert!(
        startup.contains(CURRENT_PROVIDER_PROFILE_MODEL)
            && startup.contains(CURRENT_PROVIDER_PROFILE_BASE_URL)
            && startup.contains("ProviderMetadataMode::LmStudioNativeRequired"),
        "Desktop startup fixtures must use the current closed-network LM Studio profile"
    );

    assert!(
        preflight.contains("desktop_startup_fixture_current_provider_profile"),
        "active preflight must expose the Desktop startup current provider-profile fixture marker"
    );
    for relative in [
        "docs/testing/PreflightGateSuite.md",
        "docs/design/runtime-contracts.md",
        "docs/design/detailed-design.md",
        "docs/design/itemlifecycle-detail-design.md",
    ] {
        let docs = fs::read_to_string(workspace_root.join(relative).as_std_path())
            .expect("read docs/design sync file");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "desktop_startup_fixture_current_provider_profile",
            ),
            "{relative} must document the Desktop startup current provider-profile fixture invariant"
        );
    }
}

#[test]
fn todo_completion_uses_typed_kind_not_content_text() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let todo_path = manifest_dir.join("src").join("session").join("todo.rs");
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let todo = fs::read_to_string(todo_path.as_std_path()).expect("read session todo module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let todo = todo.replace("\r\n", "\n");
    let completion_item_block = todo
        .split("pub fn todo_is_completion_item")
        .nth(1)
        .and_then(|tail| tail.split("pub fn todo_counts_as_open_work").next())
        .expect("todo completion item authority block");

    assert!(
        !completion_item_block.contains("todo.content"),
        "todo completion authority must not read natural-language content text"
    );
    assert!(
        !todo.contains("is_completion_todo(&todo.content)"),
        "todo open-work accounting must not call a content-string completion classifier"
    );
    assert!(
        todo.contains("matches!(todo.kind, TodoKind::Completion)"),
        "todo completion authority must be rooted in typed TodoKind::Completion"
    );
    assert!(
        todo.contains("todo_completion_kind_only_open_work_authority_fixture_passes"),
        "session todo module must expose an executable fixture for typed completion authority"
    );
    assert!(
        preflight.contains("todo_completion_kind_only_open_work_authority"),
        "active preflight must expose typed todo completion/open-work authority"
    );
}

#[test]
fn harness_schema_rejects_incoherent_event_stream_identity() {
    assert!(moyai::harness::gate::schema::event_stream_identity_coherence_fixture_passes());
}

#[test]
fn manual_st_expected_artifacts_are_spec_owned() {
    assert!(moyai::harness::manual_st::expected_artifacts_are_spec_owned_fixture_passes());
}

#[test]
fn state_handoff_remaining_uses_typed_target_identity() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read agent state module");
    let state = state.replace("\r\n", "\n");
    let handoff_block = state
        .split("if let Some(handoff) = state.implementation_handoff.as_mut()")
        .nth(1)
        .and_then(|tail| tail.split("if verification_passed").next())
        .expect("state handoff remaining clearance block");

    assert!(
        !handoff_block.contains("item.contains(target.as_str())"),
        "state reducer must not clear implementation handoff remaining work through natural-language substring target matching"
    );
    assert!(
        state.contains("state_handoff_remaining_exact_target_identity_fixture_passes"),
        "state reducer must expose an executable fixture proving handoff remaining work uses exact target identity"
    );

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight.contains("state_handoff_remaining_exact_target_identity"),
        "active preflight must expose the state handoff remaining exact-target identity marker"
    );
}

#[test]
fn state_blocked_reason_preservation_uses_exact_target_identity() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read agent state module");
    let state = state.replace("\r\n", "\n");
    let blocked_reason_block = state
        .split("fn blocked_reason_matches_selected_owner")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn docs_route_authority_matches_active_targets")
                .next()
        })
        .expect("state blocked reason selected-owner compatibility block");

    assert!(
        !blocked_reason_block.contains("reason_lower.contains(&target.to_ascii_lowercase())"),
        "state reducer must not preserve stale blocked_reason text through natural-language substring target matching"
    );
    assert!(
        state.contains("state_blocked_reason_exact_target_identity_fixture_passes"),
        "state reducer must expose an executable fixture proving blocked_reason preservation uses exact target identity"
    );

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight.contains("state_blocked_reason_exact_target_identity"),
        "active preflight must expose the state blocked_reason exact-target identity marker"
    );
}

#[test]
fn state_new_authoring_turn_fixture_uses_invariant_workspace_key() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().expect("workspace root");
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read agent state module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let state = state.replace("\r\n", "\n");
    let fixture_block = state
        .split("pub(crate) fn new_authoring_turn_overrides_prior_verification_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn partial_verification_pass_preserves_remaining_required_commands_fixture_passes")
                .next()
        })
        .expect("new authoring turn requested-work fixture block");

    for forbidden in ["moyai-fr10-006", "FR10", "FR22", "case1", "case2", "case3"] {
        assert!(
            !fixture_block.contains(forbidden),
            "state requested-work lifecycle fixture must not use historical FR or case primary key `{forbidden}`"
        );
    }
    for required_surface in [
        "moyai-new-authoring-turn",
        "state_new_authoring_turn_fixture_invariant_workspace_key",
    ] {
        assert!(
            state.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "state requested-work lifecycle fixture, active preflight, or preflight docs must contain invariant-owned workspace-key marker `{required_surface}`"
        );
    }
}

#[test]
fn state_generated_test_exception_overreach_fixture_is_domain_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().expect("workspace root");
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read agent state module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let state = state.replace("\r\n", "\n");
    let fixture_block = state
        .split("pub(crate) fn generated_test_exception_type_overreach_active_work_targets_test_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn state_generated_test_exception_overreach_fixture_domain_neutral_fixture_passes")
                .next()
        })
        .expect("generated-test exception overreach fixture block");

    for forbidden in [
        "test_divide_by_zero",
        "divide_by_zero",
        "calculator",
        "calculate",
        "arithmetic",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "generated-test exception-overreach fixture must not use arithmetic/calculator domain label `{forbidden}` as generic repair authority"
        );
    }
    for required_surface in [
        "workflow_generated_exception_overreach",
        "state_generated_test_exception_overreach_fixture_domain_neutral",
    ] {
        assert!(
            state.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "state generated-test exception-overreach fixture, active preflight, or preflight docs must contain workflow-neutral marker `{required_surface}`"
        );
    }
}

#[test]
fn state_generated_test_local_binding_enrichment_uses_exact_target_identity() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read agent state module");
    let state = state.replace("\r\n", "\n");
    let enrichment_block = state
        .split("fn enrich_generated_test_local_binding_contradiction_cluster")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn generated_test_local_binding_contradictions")
                .next()
        })
        .expect("generated-test local binding enrichment block");

    assert!(
        !enrichment_block.contains("file_name_str(existing) == file_name_str(&target)"),
        "generated-test local binding enrichment must not merge evidence through basename target matching"
    );
    assert!(
        state.contains(
            "generated_test_local_binding_enrichment_exact_target_identity_fixture_passes"
        ),
        "state reducer must expose an executable fixture proving generated-test local binding enrichment uses exact target identity"
    );

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight.contains("generated_test_local_binding_enrichment_exact_target_identity"),
        "active preflight must expose the generated-test local binding exact-target identity marker"
    );
}

#[test]
fn state_docs_closeout_continuation_uses_exact_target_identity() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read agent state module");
    let state = state.replace("\r\n", "\n");
    let closeout_block = state
        .split("fn looks_like_docs_closeout_continuation")
        .nth(1)
        .and_then(|tail| tail.split("fn history_item_user_text").next())
        .expect("docs closeout continuation block");

    assert!(
        !closeout_block.contains("lower.contains(&target.as_str().to_ascii_lowercase())"),
        "docs closeout continuation must not recognize deliverable targets through natural-language substring matching"
    );
    assert!(
        state.contains("state_docs_closeout_continuation_exact_target_identity_fixture_passes"),
        "state reducer must expose an executable fixture proving docs closeout continuation uses exact target identity"
    );

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight.contains("state_docs_closeout_continuation_exact_target_identity"),
        "active preflight must expose the state docs closeout continuation exact-target identity marker"
    );
}

#[test]
fn state_docs_route_area_fixtures_are_workflow_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read agent state module");
    let state = state.replace("\r\n", "\n");
    let docs_area_block = state
        .split("fn docs_route_contract_promotes_docs_repair_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn explicit_required_verification_commands_from_history_items")
                .next()
        })
        .expect("docs route area fixture block");

    for stale_surface in [
        "backend/app/main.py",
        "backend/tests/test_api.py",
        "examples/demo.py",
        "backend/pyproject.toml",
        "backend/app/api",
    ] {
        assert!(
            !docs_area_block.contains(stale_surface),
            "state docs route area fixtures must not use stale Python/backend fixture surface `{stale_surface}` as generic docs route authority"
        );
    }

    let marker_block = state
        .split("fn state_docs_route_fixtures_are_workflow_neutral_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn docs_route_localized_topic_completion_fixture_passes")
                .next()
        })
        .expect("state docs route workflow-neutral marker block");
    assert!(
        marker_block.contains("docs_route_contract_promotes_docs_repair_fixture_passes")
            && marker_block.contains("docs_route_localized_topic_completion_fixture_passes"),
        "state docs route workflow-neutral marker must execute the full docs route fixture cluster, including area coverage and localized topic completion"
    );

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight.contains("state_docs_route_fixture_workflow_neutral"),
        "active preflight must expose the state docs route workflow-neutral marker"
    );
}

#[test]
fn compaction_fixtures_use_sequence_order_and_workflow_neutral_targets() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let compaction_path = manifest_dir.join("src").join("agent").join("compaction.rs");
    let compaction =
        fs::read_to_string(compaction_path.as_std_path()).expect("read agent compaction module");
    let compaction = compaction.replace("\r\n", "\n");
    let ordering_block = compaction
        .split("fn history_item_order_key")
        .nth(1)
        .and_then(|tail| tail.split("fn latest_summary_history_index").next())
        .expect("compaction history item ordering block");

    assert!(
        !ordering_block.contains("created_at_ms.saturating_mul"),
        "compaction canonical item ordering must not use wall-clock timestamps as the primary lifecycle authority"
    );
    assert!(
        ordering_block.contains("(item.sequence_no, item.created_at_ms)"),
        "compaction ordering must use canonical HistoryItem sequence first and timestamp only as a tie-breaker"
    );
    assert!(
        compaction.contains("compaction_sequence_order_resists_timestamp_drift_fixture_passes"),
        "compaction must expose an executable fixture proving pressure/split history selection resists timestamp drift"
    );
    assert!(
        compaction.contains(
            "compaction_lifecycle_guard_sequence_order_resists_timestamp_drift_fixture_passes"
        ),
        "compaction must expose an executable fixture proving LifecycleGuardSnapshot selection resists timestamp drift"
    );

    let continuity_fixture = compaction
        .split("pub(crate) fn llm_summary_text_is_wrapped_with_typed_continuity_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn clip_compaction_text").next())
        .expect("compaction typed continuity wrapping fixture block");
    assert!(
        !continuity_fixture.contains("component.py"),
        "compaction continuity fixtures must not use component/Python targets as generic authority"
    );
    assert!(
        continuity_fixture.contains("Targets: src/workflow.rs"),
        "compaction continuity fixtures must use workflow-neutral source target roles"
    );

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight.contains("compaction_sequence_order_workflow_neutral"),
        "active preflight must expose the compaction sequence-order/workflow-neutral continuity marker"
    );
}

#[test]
fn content_shape_contract_fixtures_are_workflow_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let content_shape_path = manifest_dir
        .join("src")
        .join("agent")
        .join("content_shape_contract.rs");
    let content_shape = fs::read_to_string(content_shape_path.as_std_path())
        .expect("read content shape contract module");
    let content_shape = content_shape.replace("\r\n", "\n");

    for forbidden_surface in [
        "src/component.ts",
        "tests/component.test.ts",
        "docs/component-design.md",
        "test_component.py",
        "component.py",
        "import component",
        "TestComponent",
        "PublicComponent",
        "python -m unittest",
    ] {
        assert!(
            !content_shape.contains(forbidden_surface),
            "content-shape contract fixtures must not use component-shaped authority `{forbidden_surface}`"
        );
    }

    for required_surface in [
        "src/workflow.ts",
        "tests/workflow.spec.ts",
        "docs/workflow-design.md",
        "test_workflow.py",
        "workflow.py",
        "content_shape_contract_fixtures_are_workflow_neutral_fixture_passes",
    ] {
        assert!(
            content_shape.contains(required_surface),
            "content-shape contract fixtures must retain workflow-neutral surface `{required_surface}`"
        );
    }

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight.contains("content_shape_fixture_workflow_neutral"),
        "active preflight must expose the content-shape fixture workflow-neutral marker"
    );
}

#[test]
fn lifecycle_kernel_fixtures_are_workflow_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lifecycle_path = manifest_dir
        .join("src")
        .join("agent")
        .join("lifecycle_kernel.rs");
    let lifecycle =
        fs::read_to_string(lifecycle_path.as_std_path()).expect("read lifecycle kernel module");
    let lifecycle = lifecycle.replace("\r\n", "\n");

    for forbidden_surface in [r#"print(1)"#, r#"print(2)"#, r#""content":"print(1)\n""#] {
        assert!(
            !lifecycle.contains(forbidden_surface),
            "lifecycle kernel generic fixtures must not use Python-style payload content as workflow-neutral source authority `{forbidden_surface}`"
        );
    }

    assert!(
        lifecycle.contains("lifecycle_kernel_fixtures_are_workflow_neutral_fixture_passes"),
        "lifecycle kernel must expose an executable fixture proving workflow-neutral provider replay and adjudication payload content"
    );

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight.contains("lifecycle_kernel_fixture_workflow_neutral"),
        "active preflight must expose the lifecycle kernel workflow-neutral fixture marker"
    );
}

#[test]
fn loop_impl_lifecycle_guard_hydration_uses_canonical_item_order() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let loop_impl_path = manifest_dir.join("src").join("agent").join("loop_impl.rs");
    let loop_impl =
        fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl module");
    let loop_impl = loop_impl.replace("\r\n", "\n");
    let ordering_block = loop_impl
        .split("fn lifecycle_guard_history_item_order")
        .nth(1)
        .and_then(|tail| tail.split("impl<'a> TurnRuntime").next())
        .expect("lifecycle guard history item order block");

    assert!(
        !ordering_block.contains("created_at_ms.saturating_mul"),
        "TurnRuntime lifecycle guard hydration must not use wall-clock timestamps as primary item lifecycle order"
    );
    assert!(
        loop_impl
            .contains("lifecycle_guard_snapshot_hydration_sequence_order_resists_timestamp_drift"),
        "loop_impl must expose an executable fixture proving lifecycle guard hydration resists timestamp drift"
    );

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight.contains("loop_impl_lifecycle_guard_hydration_sequence_order"),
        "active preflight must expose the loop_impl lifecycle guard sequence-order marker"
    );
}

#[test]
fn contract_reconciliation_preserves_workspace_relative_target_identity() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let reconciliation_path = manifest_dir
        .join("src")
        .join("agent")
        .join("contract_reconciliation.rs");
    let reconciliation = fs::read_to_string(reconciliation_path.as_std_path())
        .expect("read contract reconciliation module");
    let reconciliation = reconciliation.replace("\r\n", "\n");

    for forbidden_source in [
        "file_name(target.as_str()).eq_ignore_ascii_case(&profile.source_target)",
        "file_name(target.as_str()).eq_ignore_ascii_case(generated_test_target)",
        "file_name(value).eq_ignore_ascii_case(target)",
        ".filter(|source_target| file_name(&target).eq_ignore_ascii_case(source_target))",
        ".or_else(|| Some(file_name(&target).to_string()))",
    ] {
        assert!(
            !reconciliation.contains(forbidden_source),
            "contract reconciliation must not compare or project basename display identity as repair target authority: `{forbidden_source}`"
        );
    }

    assert!(
        reconciliation.contains(
            "contract_reconciliation_preserves_workspace_relative_target_identity_fixture_passes"
        ),
        "contract reconciliation must expose an executable fixture proving workspace-relative source/test target identity is preserved"
    );
    assert!(
        reconciliation
            .contains("contract_reconciliation_cluster_refs_exact_target_identity_fixture_passes"),
        "contract reconciliation must expose an executable fixture proving cluster source refs use exact target identity"
    );

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight.contains("contract_reconciliation_target_identity_exact"),
        "active preflight must expose the contract reconciliation workspace-relative target identity marker"
    );
    assert!(
        preflight.contains("contract_reconciliation_cluster_refs_exact_target_identity"),
        "active preflight must expose the contract reconciliation cluster-ref exact target identity marker"
    );
}

#[test]
fn contract_reconciliation_cluster_refs_use_exact_target_identity() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let reconciliation_path = manifest_dir
        .join("src")
        .join("agent")
        .join("contract_reconciliation.rs");
    let reconciliation = fs::read_to_string(reconciliation_path.as_std_path())
        .expect("read contract reconciliation module");
    let reconciliation = reconciliation.replace("\r\n", "\n");
    let cluster_refs_block = reconciliation
        .split("fn cluster_refs_contain")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn has_contract_owned_public_behavior_assertion_from_cluster")
                .next()
        })
        .expect("cluster_refs_contain helper block");

    assert!(
        !cluster_refs_block.contains("file_name("),
        "cluster source-ref membership must not compare basename/display filenames as source target authority"
    );
    assert!(
        cluster_refs_block.contains("target_identity_matches(value, target)"),
        "cluster source-ref membership must compare exact normalized workspace-relative target identity"
    );
    assert!(
        reconciliation
            .contains("contract_reconciliation_cluster_refs_exact_target_identity_fixture_passes"),
        "contract reconciliation must expose an executable fixture proving cluster source refs use exact target identity"
    );

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight.contains("contract_reconciliation_cluster_refs_exact_target_identity"),
        "active preflight must expose the contract reconciliation cluster-ref exact target identity marker"
    );
}

#[test]
fn desktop_transcript_preserves_pseudo_tool_call_closeout_evidence() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let desktop_query_path = manifest_dir.join("src").join("desktop").join("query.rs");
    let desktop_query =
        fs::read_to_string(desktop_query_path.as_std_path()).expect("read desktop query module");
    for forbidden_masking_owner in [
        "normalize_completed_pseudo_tool_call_closeout",
        "transcript_body_is_pseudo_tool_call_closeout",
    ] {
        assert!(
            !desktop_query.contains(forbidden_masking_owner),
            "Desktop transcript projection must not mask canonical pseudo tool-call assistant evidence through `{forbidden_masking_owner}`"
        );
    }

    assert!(
        desktop_query
            .contains("desktop_pseudo_tool_call_closeout_evidence_preserved_fixture_passes"),
        "Desktop query must expose a deterministic fixture proving pseudo tool-call assistant text remains visible evidence"
    );

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight = preflight.replace("\r\n", "\n");
    assert!(
        preflight.contains("desktop_pseudo_tool_call_closeout_evidence_preserved"),
        "active preflight must report the Desktop pseudo tool-call evidence-preservation marker"
    );
}

#[test]
fn desktop_transcript_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let desktop_query_path = manifest_dir.join("src").join("desktop").join("query.rs");
    let desktop_query =
        fs::read_to_string(desktop_query_path.as_std_path()).expect("read desktop query module");
    let primary_fixture = desktop_query
        .split("pub fn completed_desktop_transcript_primary_reading_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn desktop_turn_item_projection")
                .next()
        })
        .expect("Desktop transcript primary reading fixture block");
    let transcript_tests = desktop_query
        .split("fn completed_work_transcript_preserves_pseudo_tool_call_closeout_body")
        .nth(1)
        .and_then(|tail| tail.split("\n}").next())
        .expect("Desktop transcript test block");
    let desktop_transcript_surface = format!("{primary_fixture}\n{transcript_tests}");

    for forbidden_surface in [
        "component.py",
        "test_component.py",
        "python -m unittest",
        "Get-Content component.py",
        "Added component.py",
        "if name == \"main\": main()",
    ] {
        assert!(
            !desktop_transcript_surface.contains(forbidden_surface),
            "Desktop transcript fixtures must not use language/domain-specific authority surface `{forbidden_surface}`"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.contract",
        "verify-contract --behavior",
        "Added src/workflow.rs",
        "desktop_transcript_fixture_language_neutral",
    ] {
        assert!(
            desktop_query.contains(required_surface),
            "Desktop transcript fixtures must include workflow-neutral surface `{required_surface}`"
        );
    }

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight.contains("desktop_transcript_fixture_language_neutral"),
        "active preflight must report the Desktop transcript fixture language-neutral marker"
    );
}

#[test]
fn desktop_open_transcript_markdown_preserves_visible_evidence() {
    assert!(moyai::desktop::app::desktop_open_transcript_markdown_preserves_visible_evidence_fixture_passes());

    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let desktop_app_path = manifest_dir.join("src").join("desktop").join("app.rs");
    let desktop_app =
        fs::read_to_string(desktop_app_path.as_std_path()).expect("read desktop app module");
    let session_markdown_path = manifest_dir.join("src").join("session").join("markdown.rs");
    let session_markdown =
        fs::read_to_string(session_markdown_path.as_std_path()).expect("read markdown module");
    for forbidden_masking_owner in [
        "line_contains_hidden_runtime_path",
        "markdown_body_is_pseudo_tool_call_closeout",
        "open_transcript_markdown_replaces_pseudo_tool_call_closeout",
    ] {
        assert!(
            !desktop_app.contains(forbidden_masking_owner)
                && !session_markdown.contains(forbidden_masking_owner),
            "Desktop open transcript Markdown export must not preserve masking owner `{forbidden_masking_owner}`"
        );
    }

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight.contains("desktop_open_transcript_markdown_evidence_preserved"),
        "active preflight must report the Desktop open transcript Markdown evidence-preservation marker"
    );
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
fn current_failure_registry_prefix_authority_is_not_split() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .expect("moyAI has repository parent")
        .to_path_buf();
    let authority_docs = [
        repo_root.join("README.md"),
        repo_root.join("ProjectBrief.md"),
        manifest_dir.join("tests/manual_ST/README.md"),
    ];

    for path in authority_docs {
        let text = fs::read_to_string(path.as_std_path()).expect("read authority document");
        assert!(
            text.contains("FR22-YYYY-MM-DD-NNN"),
            "{path} must expose FR22 as the current fresh failure prefix"
        );
        for stale_current_rule in [
            "これ以降の representative NG / route NG / preflight NG は `FR20-YYYY-MM-DD-NNN`",
            "今後の representative NG / route NG / preflight NG は `FR10-YYYY-MM-DD-NNN`",
            "現在の FR21 convergence loop",
        ] {
            assert!(
                !text.contains(stale_current_rule),
                "{path} contains stale current Failure Registry prefix rule: {stale_current_rule}"
            );
        }
    }
}

#[test]
fn current_authority_index_is_not_incident_chronology() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .expect("moyAI has repository parent")
        .to_path_buf();
    let path = repo_root
        .join("docs")
        .join("design")
        .join("current-authority-index.md");
    let text = fs::read_to_string(path.as_std_path()).expect("read current authority index");
    for invariant in [
        "SingleControlPlane",
        "HistoryItemAuthorityRole",
        "StateAuthorityDecision",
        "ToolLifecycleOwner",
        "ProjectionSeparation",
        "LanguageEvidenceAdapter",
        "HarnessEvidence",
    ] {
        assert!(
            text.contains(invariant),
            "current authority index must contain invariant id {invariant}"
        );
    }
    for incident_primary_key in ["FR22-", "FR20-", "case1", "case2", "calculator.py"] {
        assert!(
            !text.contains(incident_primary_key),
            "current authority index must not use incident/case key `{incident_primary_key}` as authority"
        );
    }
}

#[test]
fn config_defaults_use_closed_network_lm_studio_profile() {
    let default_config = ResolvedConfig::default();
    assert_eq!(default_config.model.base_url, "http://127.0.0.1:1234");
    assert_eq!(default_config.model.model, "qwen/qwen3.6-35b-a3b");
    assert_eq!(
        default_config.model.provider_metadata_mode,
        ProviderMetadataMode::LmStudioNativeRequired
    );
    assert_eq!(default_config.model.context_window, 131_072);
    assert_eq!(default_config.model.max_output_tokens, 8_192);
    assert_eq!(
        default_config.model.extra_body_json,
        Some(serde_json::json!({ "num_ctx": 131072 }))
    );

    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let model_path = manifest_dir.join("src").join("config").join("model.rs");
    let model_text = fs::read_to_string(model_path.as_std_path()).expect("read config model");
    let default_block = model_text
        .split("impl Default for ResolvedConfig")
        .nth(1)
        .and_then(|tail| tail.split("            session: SessionConfig").next())
        .expect("ResolvedConfig default model block");
    for forbidden_default in [
        "http://127.0.0.1:1234",
        "http://127.0.0.1:8110",
        "qwen3.6-35b-a3b-4bit",
        "ProviderMetadataMode::OpenAiCompatibleOnly",
    ] {
        assert!(
            !default_block.contains(forbidden_default),
            "ResolvedConfig defaults must not retain stale provider profile value `{forbidden_default}`"
        );
    }

    let loader_path = manifest_dir.join("src").join("config").join("loader.rs");
    let loader_text = fs::read_to_string(loader_path.as_std_path()).expect("read config loader");
    for required_generated_default in [
        "http://127.0.0.1:1234",
        "qwen/qwen3.6-35b-a3b",
        "lm_studio_native_required",
        "max_output_tokens = 8192",
    ] {
        assert!(
            loader_text.contains(required_generated_default),
            "generated default config test must assert `{required_generated_default}`"
        );
    }

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight_text =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight_text.contains("config_default_provider_profile_lm_studio"),
        "active preflight must report the config default provider profile marker"
    );
}

#[test]
fn edit_change_tracker_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let change_tracker_path = manifest_dir
        .join("src")
        .join("edit")
        .join("change_tracker.rs");
    let change_tracker =
        fs::read_to_string(change_tracker_path.as_std_path()).expect("read change tracker module");
    let fixture_block = change_tracker
        .split("pub(crate) fn change_path_storage_uses_workspace_relative_authority")
        .nth(1)
        .and_then(|tail| tail.split("fn sha256_hex").next())
        .expect("edit change tracker fixture block");

    for forbidden_surface in [
        "component.py",
        "test_component.py",
        "source.py",
        "*** Update File: source.py",
    ] {
        assert!(
            !fixture_block.contains(forbidden_surface),
            "edit change tracker fixtures must not use Python/component authority surface `{forbidden_surface}`"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.contract",
        "docs/workflow-design.md",
        "edit_change_tracker_fixture_language_neutral",
    ] {
        assert!(
            change_tracker.contains(required_surface),
            "edit change tracker fixtures must include workflow-neutral surface `{required_surface}`"
        );
    }

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight_text =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight_text.contains("edit_change_tracker_fixture_language_neutral"),
        "active preflight must report the edit change-tracker fixture language-neutral marker"
    );
}

#[test]
fn edit_patch_parser_feedback_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let patch_path = manifest_dir.join("src").join("edit").join("patch.rs");
    let patch_text = fs::read_to_string(patch_path.as_std_path()).expect("read patch module");
    let add_body_feedback = patch_text
        .split("add file body line")
        .nth(1)
        .and_then(|tail| tail.split("contents.push").next())
        .expect("add file body feedback block");
    for forbidden_surface in ["def", "class", "import", "top-level"] {
        assert!(
            !add_body_feedback.contains(forbidden_surface),
            "generic patch parser feedback must not use language-specific syntax example `{forbidden_surface}`"
        );
    }
    for required_surface in [
        "all content lines",
        "blank lines",
        "indented lines",
        "source-code lines",
        "edit_patch_parser_feedback_language_neutral",
    ] {
        assert!(
            patch_text.contains(required_surface),
            "patch parser feedback must include language-neutral patch grammar wording `{required_surface}`"
        );
    }

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight_text =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight_text.contains("edit_patch_parser_feedback_language_neutral"),
        "active preflight must report the edit patch parser feedback language-neutral marker"
    );
}

#[test]
fn vision_input_projection_uses_codex_labeled_image_item() {
    assert!(moyai::agent::prompt::vision_input_provider_projection_fixture_passes());
}

#[test]
fn history_item_authority_roles_keep_projection_items_out_of_runtime_inputs() {
    assert!(moyai::protocol::history_item_projection_roles_are_not_authority_fixture_passes());
}

#[test]
fn protocol_pending_tool_lifecycle_does_not_fabricate_blocked_action() {
    assert!(
        moyai::protocol::pending_tool_lifecycle_does_not_fabricate_blocked_action_fixture_passes()
    );

    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let projection_path = manifest_dir
        .join("src")
        .join("protocol")
        .join("projection.rs");
    let projection_text =
        fs::read_to_string(projection_path.as_std_path()).expect("read protocol projection module");
    let pending_block = projection_text
        .split("fn runtime_msg_for_run_event")
        .nth(1)
        .expect("runtime message projection function")
        .split("RunEvent::ToolCallPending")
        .nth(1)
        .and_then(|tail| tail.split("RunEvent::ToolCallCompleted").next())
        .expect("pending tool lifecycle projection block");
    assert!(
        !pending_block.contains("Some(title.clone())"),
        "pending ToolLifecycle projection must not turn a display title into blocked_action evidence"
    );
    assert!(
        projection_text
            .contains("pending_tool_lifecycle_does_not_fabricate_blocked_action_fixture_passes"),
        "protocol projection must expose a deterministic fixture for pending blocked_action absence"
    );

    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight_text =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    assert!(
        preflight_text.contains("protocol_pending_tool_lifecycle_blocked_action_absent"),
        "active preflight must report the pending ToolLifecycle blocked-action absence marker"
    );
}

#[test]
fn runtime_input_view_does_not_expose_compatibility_transcript_authority() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir.join("src").join("agent").join("prompt.rs");
    let text = fs::read_to_string(path.as_std_path()).expect("read prompt module");
    let runtime_struct = text
        .split("pub struct RuntimeInputView")
        .nth(1)
        .and_then(|tail| tail.split("}\n\nimpl RuntimeInputView").next())
        .expect("RuntimeInputView struct block");
    let runtime_impl = text
        .split("impl RuntimeInputView")
        .nth(1)
        .and_then(|tail| {
            tail.split("\n}\n\n#[derive(Debug, Clone)]\npub struct PromptBundle")
                .next()
        })
        .expect("RuntimeInputView impl block");
    let runtime_input_source = format!("{runtime_struct}\n{runtime_impl}");
    assert!(
        !runtime_input_source.contains("into_compatibility_transcript"),
        "RuntimeInputView must not expose an active compatibility transcript authority"
    );
    assert!(
        !runtime_input_source.contains("materialized_transcript_projection"),
        "RuntimeInputView must not expose compatibility Transcript materialization as an active prompt/runtime input API"
    );
    assert!(
        !runtime_input_source
            .contains("transcript_from_history_items(&self.session, &self.history_items)"),
        "RuntimeInputView must not rebuild compatibility Transcript from active HistoryItem runtime input"
    );
    assert!(
        !runtime_input_source.contains("session: SessionRecord,")
            && !runtime_input_source.contains("session: session.clone()"),
        "RuntimeInputView must not carry a cloned SessionRecord as a second active prompt authority"
    );
    assert!(
        !runtime_input_source.contains("pub fn from_history_items(session: &SessionRecord"),
        "RuntimeInputView constructor must not accept SessionRecord when HistoryItem stream is the runtime input authority"
    );
}

#[test]
fn requested_work_parser_does_not_use_manual_st_harness_marker_as_authority() {
    assert!(
        moyai::agent::prompt::requested_work_parser_does_not_use_manual_st_harness_marker_fixture_passes()
    );

    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir.join("src").join("agent").join("prompt.rs");
    let text = fs::read_to_string(path.as_std_path()).expect("read prompt module");
    let parser_block = text
        .split("fn instruction_authority_lines")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub fn requested_work_parser_does_not_use_manual_st_harness_marker_fixture_passes",
            )
            .next()
        })
        .expect("instruction authority parser block");
    for harness_marker in ["manual_st", "manual st", "case1", "case2"] {
        assert!(
            !parser_block.to_ascii_lowercase().contains(harness_marker),
            "active requested-work parser block must not use harness/case marker `{harness_marker}` as authority"
        );
    }
}

#[test]
fn requested_work_parser_does_not_use_case_stage_or_harness_owned_markers_as_authority() {
    assert!(
        moyai::agent::prompt::requested_work_parser_does_not_use_case_stage_or_harness_owned_markers_fixture_passes()
    );

    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir.join("src").join("agent").join("prompt.rs");
    let text = fs::read_to_string(path.as_std_path()).expect("read prompt module");
    let section_header_block = text
        .split("fn non_diagnostic_authority_section_header")
        .nth(1)
        .and_then(|tail| tail.split("fn diagnostic_traceback_line").next())
        .expect("section header classifier block");
    for stale_section_marker in [
        "\"case\"",
        "\"stage\"",
        "\"verification-repair attempt\"",
        "\"verification attempt\"",
    ] {
        assert!(
            !section_header_block.contains(stale_section_marker),
            "active requested-work section classifier must not use route/harness marker {stale_section_marker} as authority"
        );
    }

    let reference_parser_block = text
        .split("pub(crate) fn requested_work_contract_from_instruction_text")
        .nth(1)
        .and_then(|tail| tail.split("fn target_is_contract_reference").next())
        .expect("requested-work reference parser block");
    for harness_marker in [
        "line_has_harness_owned_marker",
        "harness-owned",
        "harness owned",
        "harness 管理",
    ] {
        assert!(
            !reference_parser_block
                .to_ascii_lowercase()
                .contains(&harness_marker.to_ascii_lowercase()),
            "active requested-work reference parser must not use harness marker `{harness_marker}` as authority"
        );
    }
}

#[test]
fn runtime_contract_does_not_preserve_manual_st_parser_authority_wording() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .expect("moyAI has repository parent")
        .to_path_buf();
    let path = repo_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let text = fs::read_to_string(path.as_std_path()).expect("read runtime contracts");
    let section = text
        .split("## 53. Closeout Continuation Inventory Authority Contract")
        .nth(1)
        .and_then(|tail| {
            tail.split("## 54. Provider-Required Final-Message Recovery Tool Choice Contract")
                .next()
        })
        .expect("closeout continuation inventory section");
    for stale_parser_contract in [
        "requested-work parser skips Manual ST continuation",
        "Manual ST continuation `Expected artifacts`",
        "parser skips Manual ST",
    ] {
        assert!(
            !section.contains(stale_parser_contract),
            "runtime contract must not preserve Manual ST-specific requested-work parser authority wording: {stale_parser_contract}"
        );
    }
    assert!(
        section.contains("ContinuationContract")
            && section.contains("typed requested-work section")
            && section.contains("route inventory")
            && section.contains("ManualStCloseoutEvidence")
            && section.contains("does not define requested-work parser"),
        "closeout continuation inventory contract must name typed continuation / typed section-role authority, keep route inventory separate, and keep ManualStCloseoutEvidence out of parser semantics"
    );
    let preflight_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let preflight_text =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight gate suite");
    assert!(
        preflight_text.contains("runtime_contract_typed_continuation_parser_wording"),
        "preflight documentation must carry the runtime-contract parser wording sync guard"
    );
}

#[test]
fn runtime_contract_does_not_make_python_test_shape_global_authority() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .expect("moyAI has repository parent")
        .to_path_buf();
    let path = repo_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let text = fs::read_to_string(path.as_std_path()).expect("read runtime contracts");
    let section = text
        .split("## 56. Generated-Test")
        .nth(1)
        .and_then(|tail| {
            tail.split("## 57. Docs Content-Grounding Progress Projection Recovery")
                .next()
        })
        .expect("generated-test executable shape section");
    for stale_global_language_contract in [
        "Generated-test artifacts must be executable Python test modules",
        "must be executable Python test modules before they can count as file-change progress",
    ] {
        assert!(
            !section.contains(stale_global_language_contract),
            "runtime contract must not make Python/unittest shape the global generated-test authority: {stale_global_language_contract}"
        );
    }
    assert!(
        section.contains("executable test artifact")
            && section.contains("LanguageEvidenceAdapter")
            && section.contains("adapter-specific specialization")
            && section.contains("Python")
            && section.contains("unittest"),
        "generated-test executable shape contract must be generic and scope Python/unittest to a LanguageEvidenceAdapter specialization"
    );
    let preflight_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let preflight_text =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight gate suite");
    assert!(
        preflight_text
            .contains("runtime_contract_generated_test_executable_shape_adapter_contract"),
        "preflight documentation must carry the generated-test executable shape adapter contract"
    );
}

#[test]
fn runtime_contract_does_not_make_python_unittest_global_encoding_authority() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .expect("moyAI has repository parent")
        .to_path_buf();
    let path = repo_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let text = fs::read_to_string(path.as_std_path()).expect("read runtime contracts");
    let section = text
        .split("## 59. Shell Text Content Read Encoding Contract")
        .nth(1)
        .and_then(|tail| {
            tail.split("## 60. Docs Semantic Repair Snippet Projection")
                .next()
        })
        .expect("shell encoding contract section");
    for stale_global_encoding_contract in [
        "python -m unittest",
        "without `-X utf8` remains a corrective command-text encoding violation",
    ] {
        assert!(
            !section.contains(stale_global_encoding_contract),
            "runtime contract must not make Python/unittest runner grammar the global shell encoding authority: {stale_global_encoding_contract}"
        );
    }
    assert!(
        section.contains("text-producing")
            && section.contains("text-consuming")
            && section.contains("ToolLifecycle")
            && section.contains("LanguageEvidenceAdapter")
            && section.contains("adapter-specific specialization"),
        "shell encoding contract must be generic text I/O authority with runner-specific encoding handled by LanguageEvidenceAdapter specialization"
    );
    let preflight_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let preflight_text =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight gate suite");
    assert!(
        preflight_text.contains("runtime_contract_shell_encoding_language_adapter_contract"),
        "preflight documentation must carry the shell encoding language-adapter contract"
    );
}

#[test]
fn runtime_contract_does_not_make_python_source_shape_global_authority() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .expect("moyAI has repository parent")
        .to_path_buf();
    let path = repo_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let text = fs::read_to_string(path.as_std_path()).expect("read runtime contracts");
    let section = text
        .split("## 62.")
        .nth(1)
        .and_then(|tail| {
            tail.split("## 63. Authoring Grounding Terminal Route Fail-Stop")
                .next()
        })
        .expect("source artifact content-shape section");
    for stale_global_source_contract in [
        "Python Source Content-Shape Required Public Surface",
        "Python source artifact",
        "space_invader.py",
        "def rects_overlap",
        "def _run_gui",
    ] {
        assert!(
            !section.contains(stale_global_source_contract),
            "runtime contract must not make Python source shape the global source artifact authority: {stale_global_source_contract}"
        );
    }
    assert!(
        section.contains("source artifact public surface")
            && section.contains("ArtifactShapeContract")
            && section.contains("TargetObligationSurface")
            && section.contains("ToolLifecycle")
            && section.contains("LanguageEvidenceAdapter")
            && section.contains("adapter-specific specialization")
            && section.contains("Python"),
        "source artifact content-shape contract must be generic and scope Python syntax to a LanguageEvidenceAdapter specialization"
    );
    let preflight_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let preflight_text =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight gate suite");
    assert!(
        preflight_text.contains("runtime_contract_source_shape_language_adapter_contract"),
        "preflight documentation must carry the source shape language-adapter contract"
    );
}

#[test]
fn runtime_contract_does_not_make_python_boolean_continuation_global_source_authority() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .expect("moyAI has repository parent")
        .to_path_buf();
    let path = repo_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let text = fs::read_to_string(path.as_std_path()).expect("read runtime contracts");
    let section = text
        .split("## 65.")
        .nth(1)
        .and_then(|tail| {
            tail.split("## 66. Verification Repair Supporting-Context Control-Plane Convergence")
                .next()
        })
        .expect("source continuation content-shape section");
    for stale_global_continuation_contract in [
        "Python Source Boolean Continuation Admission",
        "Python source target",
        "executable Python continuation",
        "raw prose line inside Python source",
        "GameState",
        "collision behavior",
    ] {
        assert!(
            !section.contains(stale_global_continuation_contract),
            "runtime contract must not make Python boolean continuation the global source continuation authority: {stale_global_continuation_contract}"
        );
    }
    assert!(
        section.contains("source continuation")
            && section.contains("executable source syntax")
            && section.contains("ArtifactShapeContract")
            && section.contains("TargetObligationSurface")
            && section.contains("ToolLifecycle")
            && section.contains("LanguageEvidenceAdapter")
            && section.contains("adapter-specific specialization")
            && section.contains("Python"),
        "source continuation contract must be generic and scope Python boolean continuation to a LanguageEvidenceAdapter specialization"
    );
    let preflight_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let preflight_text =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight gate suite");
    assert!(
        preflight_text.contains("runtime_contract_source_continuation_language_adapter_contract"),
        "preflight documentation must carry the source continuation language-adapter contract"
    );
}

#[test]
fn content_shape_contract_does_not_own_python_source_continuation_syntax() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let content_shape_path = manifest_dir
        .join("src")
        .join("agent")
        .join("content_shape_contract.rs");
    let language_adapter_path = manifest_dir
        .join("src")
        .join("agent")
        .join("language_evidence.rs");
    let content_shape =
        fs::read_to_string(content_shape_path.as_std_path()).expect("read content shape contract");
    let language_adapter =
        fs::read_to_string(language_adapter_path.as_std_path()).expect("read language adapter");

    for forbidden_local_owner in [
        "fn python_source_line_can_be_executable_python",
        "fn python_source_line_has_argument_continuation_shape",
        "fn python_source_line_has_boolean_comparison_continuation_shape",
    ] {
        assert!(
            !content_shape.contains(forbidden_local_owner),
            "content_shape_contract.rs must not own Python source continuation syntax helper `{forbidden_local_owner}`"
        );
    }
    assert!(
        content_shape.contains("language_source_artifact_shape_contract")
            && content_shape.contains("language_source_artifact_content_has_executable_shape")
            && language_adapter
                .contains("pub(crate) fn language_source_line_can_be_executable_source")
            && language_adapter.contains("pub(crate) fn language_source_line_has_code_shape")
            && language_adapter.contains("pub(crate) fn language_source_artifact_shape_contract")
            && language_adapter
                .contains("pub(crate) fn language_source_artifact_content_has_executable_shape")
            && language_adapter.contains("LanguageFamily::Python"),
        "ArtifactShapeContract must consume LanguageEvidenceAdapter-owned source artifact executable-shape facts"
    );
}

#[test]
fn content_shape_contract_does_not_own_python_generated_test_shape_contract() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let content_shape_path = manifest_dir
        .join("src")
        .join("agent")
        .join("content_shape_contract.rs");
    let language_adapter_path = manifest_dir
        .join("src")
        .join("agent")
        .join("language_evidence.rs");
    let content_shape =
        fs::read_to_string(content_shape_path.as_std_path()).expect("read content shape contract");
    let language_adapter =
        fs::read_to_string(language_adapter_path.as_std_path()).expect("read language adapter");

    for forbidden_local_owner in [
        "struct TestTargetContentShapeContract",
        "fn python_source_for_test_target",
        "fn python_test_module_content_has_executable_shape",
        "impl TestTargetContentShapeContract",
        "fn test_target_has_invalid_test_class_base",
        "fn test_target_has_opaque_subprocess_returncode_assertion",
        "fn test_target_has_recursive_runner_self_invocation",
        "fn test_target_has_missing_module_import_for_qualified_reference",
        "\"python_test_module_content_shape\"",
        "Required positive test-module shape",
        "Public command coverage",
        "class Test* missing unittest.TestCase base",
    ] {
        assert!(
            !content_shape.contains(forbidden_local_owner),
            "content_shape_contract.rs must not own Python generated-test shape contract surface `{forbidden_local_owner}`"
        );
    }
    assert!(
        content_shape.contains("language_test_artifact_shape_contract")
            && content_shape.contains("LanguageArtifactShapeContract")
            && language_adapter.contains("pub(crate) struct LanguageArtifactShapeContract")
            && language_adapter.contains("pub(crate) fn language_test_artifact_shape_contract")
            && language_adapter.contains("python_test_module_content_shape")
            && language_adapter.contains("unittest.TestCase"),
        "ArtifactShapeContract must consume LanguageEvidenceAdapter-owned generated-test artifact shape facts"
    );
    let preflight_path = manifest_dir
        .parent()
        .expect("moyAI has repository parent")
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let preflight_text =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight gate suite");
    assert!(
        preflight_text.contains("generated_test_shape_language_adapter_owner_contract"),
        "preflight documentation must carry generated-test shape language-adapter owner contract"
    );
}

#[test]
fn content_shape_contract_does_not_own_python_source_shape_contract() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let content_shape_path = manifest_dir
        .join("src")
        .join("agent")
        .join("content_shape_contract.rs");
    let language_adapter_path = manifest_dir
        .join("src")
        .join("agent")
        .join("language_evidence.rs");
    let content_shape =
        fs::read_to_string(content_shape_path.as_std_path()).expect("read content shape contract");
    let language_adapter =
        fs::read_to_string(language_adapter_path.as_std_path()).expect("read language adapter");

    for forbidden_local_owner in [
        "fn python_source_target_requires_executable_shape",
        "fn python_source_content_has_executable_shape",
        "fn detected_python_source_forbidden_content_markers",
        "fn python_source_content_is_escaped_whole_file_string",
        "fn python_source_content_is_test_module_payload",
        "fn python_source_content_is_markdown_or_prose_payload",
        "fn python_source_content_has_raw_prose_line",
        "fn python_source_content_has_duplicate_executable_entrypoint",
        "fn python_source_content_has_code_shape",
        "fn python_source_line_looks_like_prose",
        "fn python_source_content_shape_metadata",
        "fn python_source_positive_shape_guidance",
        "fn python_source_prompt_contract",
        "fn python_source_tool_schema_description",
        "python_source_executable_content_shape",
        "Required positive Python source shape",
        "unittest/pytest test module payload",
        "multiple executable entrypoint guards",
    ] {
        assert!(
            !content_shape.contains(forbidden_local_owner),
            "content_shape_contract.rs must not own Python source artifact shape contract surface `{forbidden_local_owner}`"
        );
    }
    assert!(
        content_shape.contains("language_source_artifact_shape_contract")
            && content_shape.contains("language_source_artifact_content_has_executable_shape")
            && content_shape.contains("language_source_artifact_forbidden_content_markers")
            && language_adapter.contains("pub(crate) fn language_source_artifact_shape_contract")
            && language_adapter
                .contains("pub(crate) fn language_source_artifact_content_has_executable_shape")
            && language_adapter
                .contains("pub(crate) fn language_source_artifact_forbidden_content_markers")
            && language_adapter.contains("python_source_executable_content_shape"),
        "ArtifactShapeContract must consume LanguageEvidenceAdapter-owned source artifact shape facts"
    );
    let preflight_path = manifest_dir
        .parent()
        .expect("moyAI has repository parent")
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let preflight_text =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight gate suite");
    assert!(
        preflight_text.contains("python_source_shape_language_adapter_owner_contract"),
        "preflight documentation must carry Python source shape language-adapter owner contract"
    );
}

#[test]
fn content_shape_contract_uses_generic_language_adapter_consumer_names() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let content_shape_path = manifest_dir
        .join("src")
        .join("agent")
        .join("content_shape_contract.rs");
    let preflight_path = manifest_dir
        .parent()
        .expect("moyAI has repository parent")
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let content_shape =
        fs::read_to_string(content_shape_path.as_std_path()).expect("read content shape contract");
    let preflight_text =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight gate suite");

    for stale_helper in [
        "python_artifact_content_has_executable_shape",
        "python_artifact_target_requires_executable_shape",
    ] {
        assert!(
            !content_shape.contains(stale_helper),
            "generic content_shape_contract.rs must not expose Python-named helper `{stale_helper}` as artifact content-shape authority"
        );
    }
    assert!(
        content_shape.contains("language_artifact_content_has_executable_shape")
            && content_shape.contains("language_artifact_target_requires_executable_shape")
            && preflight_text.contains("content_shape_language_adapter_consumer_surface"),
        "content-shape consumer surface must use generic LanguageEvidenceAdapter consumer names and preflight wording"
    );
}

#[test]
fn generic_text_artifact_content_shape_wording_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let content_shape_path = manifest_dir
        .join("src")
        .join("agent")
        .join("content_shape_contract.rs");
    let preflight_path = manifest_dir
        .parent()
        .expect("moyAI has repository parent")
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let content_shape =
        fs::read_to_string(content_shape_path.as_std_path()).expect("read content shape contract");
    let preflight_text =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight gate suite");
    let text_contract_block = content_shape
        .split("pub(crate) fn required_write_target_mismatch_content_shape_guidance")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn code_artifact_positive_shape_guidance")
                .next()
        })
        .expect("text artifact content-shape wording block");
    let docs_block = preflight_text
        .split("content_shape_language_adapter_consumer_surface")
        .nth(1)
        .and_then(|tail| {
            tail.split("runtime_contract_shell_encoding_language_adapter_contract")
                .next()
        })
        .expect("content shape language adapter consumer docs block");

    for stale_language_wording in ["Python-escaped", "JSON/Python", "Python source"] {
        assert!(
            !text_contract_block.contains(stale_language_wording),
            "generic text artifact content-shape wording must not use language-specific `{stale_language_wording}` surface"
        );
        assert!(
            !docs_block.contains(stale_language_wording),
            "generic text artifact preflight wording must not use language-specific `{stale_language_wording}` surface"
        );
    }
    assert!(
        text_contract_block.contains("serialized string snapshot")
            && text_contract_block.contains("escaped string literal")
            && text_contract_block.contains("literal `\\\\n` escape sequences"),
        "generic text artifact wording must use language-neutral serialized-string terminology"
    );
}

#[test]
fn source_content_shape_fixture_surface_is_generic() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let content_shape_path = manifest_dir
        .join("src")
        .join("agent")
        .join("content_shape_contract.rs");
    let loop_impl_path = manifest_dir.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let content_shape =
        fs::read_to_string(content_shape_path.as_std_path()).expect("read content shape contract");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let content_fixture_block = content_shape
        .split("pub(crate) fn text_artifact_readable_shape_rejects_serialized_markdown_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn source_executable_shape_accepts_required_public_surface_fixture_passes")
                .next()
        })
        .expect("source content-shape fixture surface block");
    let loop_source_shape_block = loop_impl
        .split("pub(crate) fn source_content_shape_rejects_escaped_whole_file_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn corrective_content_shape_no_progress_terminal_guard_fixture_passes",
            )
            .next()
        })
        .expect("loop source content-shape fixture bridge block");

    for stale_fixture in [
        "python_source_executable_shape_rejects_escaped_whole_file_fixture_passes",
        "python_source_executable_shape_rejects_test_module_payload_fixture_passes",
        "python_source_executable_shape_rejects_markdown_payload_fixture_passes",
        "python_source_executable_shape_rejects_raw_prose_line_fixture_passes",
        "python_source_executable_shape_rejects_duplicate_entrypoint_fixture_passes",
    ] {
        assert!(
            !content_fixture_block.contains(stale_fixture),
            "generic source content-shape fixtures must not expose Python-named bridge `{stale_fixture}`"
        );
        assert!(
            !loop_source_shape_block.contains(stale_fixture),
            "loop source content-shape preflight bridge must not call Python-named fixture `{stale_fixture}`"
        );
    }
    for generic_fixture in [
        "source_content_shape_rejects_escaped_whole_file_fixture_passes",
        "source_content_shape_rejects_test_module_payload_fixture_passes",
        "source_content_shape_rejects_markdown_payload_fixture_passes",
        "source_content_shape_rejects_raw_prose_line_fixture_passes",
        "source_content_shape_rejects_duplicate_entrypoint_fixture_passes",
    ] {
        assert!(
            content_shape.contains(generic_fixture),
            "content_shape_contract.rs must expose generic fixture `{generic_fixture}`"
        );
        assert!(
            loop_impl.contains(generic_fixture),
            "loop_impl.rs must consume generic fixture `{generic_fixture}`"
        );
        assert!(
            preflight.contains(generic_fixture.trim_end_matches("_fixture_passes")),
            "active preflight must expose generic gate `{generic_fixture}`"
        );
    }
    assert!(
        docs.contains("source_content_shape_fixture_surface"),
        "PreflightGateSuite must document the generic source content-shape fixture surface"
    );
}

#[test]
fn source_content_shape_fixture_body_is_scenario_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let content_shape_path = manifest_dir
        .join("src")
        .join("agent")
        .join("content_shape_contract.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let content_shape =
        fs::read_to_string(content_shape_path.as_std_path()).expect("read content shape contract");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let source_fixture_block = content_shape
        .split("pub(crate) fn source_content_shape_rejects_markdown_payload_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn source_content_shape_rejects_duplicate_entrypoint_fixture_passes",
            )
            .next()
        })
        .expect("source content-shape markdown/raw-prose fixture body block");

    for domain_surface in [
        "def calculate",
        "unsupported operator",
        "電卓",
        "四則演算",
        "Python CLI",
        "CLI エントリポイント",
        "演算: 加算",
        "python component.py 1 + 2",
    ] {
        assert!(
            !source_fixture_block.contains(domain_surface),
            "generic source content-shape fixture body must not use calculator / CLI domain surface `{domain_surface}` as contract authority"
        );
    }
    assert!(
        source_fixture_block.contains("def transform_record")
            && source_fixture_block.contains("return normalized or \"empty\"")
            && source_fixture_block.contains("## Source Shape Notes")
            && source_fixture_block.contains("# Source module overview"),
        "generic source content-shape fixture body must use scenario-neutral source samples"
    );
    assert!(
        docs.contains("source_content_shape_fixture_body"),
        "PreflightGateSuite must document the generic source content-shape fixture body invariant"
    );
}

#[test]
fn content_shape_public_surface_fixture_is_scenario_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let content_shape_path = manifest_dir
        .join("src")
        .join("agent")
        .join("content_shape_contract.rs");
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let content_shape =
        fs::read_to_string(content_shape_path.as_std_path()).expect("read content shape contract");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");

    let fixture_block = content_shape
        .split("source_executable_shape_accepts_required_public_surface_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn test_target_content_shape_projection")
                .next()
        })
        .expect("source public-surface fixture block");
    let docs_block = docs
        .split("Python source artifact admission allows required public source surfaces.")
        .nth(1)
        .and_then(|tail| tail.split("- Consumed vision images").next())
        .expect("source public-surface preflight documentation block");
    let forbidden_scenario_authority = [
        "Space Invader",
        "space_invader.py",
        "GameState",
        "rects_overlap",
        "_run_gui",
        "direction_hits_wall",
        "case2a",
        "test_space_invader.py",
    ];
    for forbidden in forbidden_scenario_authority {
        assert!(
            !fixture_block.contains(forbidden),
            "source public-surface fixture must not use scenario/domain surface `{forbidden}` as contract authority"
        );
        assert!(
            !docs_block.contains(forbidden),
            "source public-surface preflight documentation must not use scenario/domain surface `{forbidden}` as contract authority"
        );
    }
    assert!(
        fixture_block.contains("workflow.py")
            && fixture_block.contains("PublicWorkflow")
            && fixture_block.contains("format_public_record")
            && fixture_block.contains("record_has_required_fields")
            && fixture_block.contains("_run_internal_helper"),
        "source public-surface fixture must use scenario-neutral source names"
    );
    assert!(
        preflight.contains(
            "content_shape_contract::source_executable_shape_accepts_required_public_surface_fixture_passes"
        ),
        "active preflight must consume the scenario-neutral source public-surface fixture"
    );
    assert!(
        docs_block.contains("source_required_public_surface_allowed")
            && docs_block.contains("module-level public APIs")
            && docs_block.contains("internal helper functions")
            && docs_block.contains("method-call expression")
            && docs_block.contains("statements such as `object.method(...)`"),
        "preflight docs must describe the generic source public-surface invariant"
    );
}

#[test]
fn source_public_surface_fixture_body_is_domain_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let content_shape_path = manifest_dir
        .join("src")
        .join("agent")
        .join("content_shape_contract.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let content_shape =
        fs::read_to_string(content_shape_path.as_std_path()).expect("read content shape contract");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let fixture_block = content_shape
        .split(
            "pub(crate) fn source_executable_shape_accepts_required_public_surface_fixture_passes",
        )
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn test_target_content_shape_projection")
                .next()
        })
        .expect("source public-surface fixture body block");
    let docs_block = docs
        .split("Python source artifact admission allows required public source surfaces.")
        .nth(1)
        .and_then(|tail| tail.split("- Consumed vision images").next())
        .expect("source public-surface preflight documentation block");

    for domain_surface in [
        "compute_public_overlap",
        "separated_by_bounds",
        "movement_exceeds_bounds",
        "left_start",
        "left_top",
        "right_bottom",
        "(10, 20, 30, 40)",
        "(20, 30, 40, 50)",
        "rectangle",
        "collision",
        "movement",
        "bounds",
        "coordinate",
    ] {
        assert!(
            !fixture_block.contains(domain_surface),
            "generic source public-surface fixture body must not use geometry / movement domain surface `{domain_surface}` as contract authority"
        );
        assert!(
            !docs_block.contains(domain_surface),
            "generic source public-surface preflight docs must not use geometry / movement domain surface `{domain_surface}` as contract authority"
        );
    }
    assert!(
        fixture_block.contains("format_public_record")
            && fixture_block.contains("record_has_required_fields")
            && fixture_block.contains("_prepare_record")
            && fixture_block.contains("workflow.format_public_record("),
        "generic source public-surface fixture body must use scenario-neutral record/configuration samples"
    );
    assert!(
        docs_block.contains("source_public_surface_fixture_body"),
        "PreflightGateSuite must document the source public-surface fixture-body invariant"
    );
}

#[test]
fn language_evidence_adapter_registry_fixture_is_scenario_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let language_adapter_path = manifest_dir
        .join("src")
        .join("agent")
        .join("language_evidence.rs");
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let language_adapter =
        fs::read_to_string(language_adapter_path.as_std_path()).expect("read language adapter");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let fixture_block = language_adapter
        .split("pub(crate) fn language_evidence_adapter_registry_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("language evidence adapter registry fixture block");
    let docs_block = docs
        .split("LanguageEvidenceAdapter registry uses scenario-neutral parser evidence.")
        .nth(1)
        .and_then(|tail| {
            tail.split("- Python source artifact admission rejects concatenated executable modules")
                .next()
        })
        .expect("language evidence adapter registry preflight docs block");
    let forbidden_domain_authority = [
        "tests/test_game.py",
        "test_game.py",
        "game.tick()",
        "GAME_OVER",
        "test_state",
        "state.status terminal transition",
        "tests/test_calc.py",
        "test_calc.py",
        "Result: 3",
    ];
    for forbidden in forbidden_domain_authority {
        assert!(
            !fixture_block.contains(forbidden),
            "LanguageEvidenceAdapter registry fixture must not use game/domain surface `{forbidden}` as contract authority"
        );
        assert!(
            !docs_block.contains(forbidden),
            "LanguageEvidenceAdapter registry preflight docs must not use game/domain surface `{forbidden}` as contract authority"
        );
    }
    assert!(
        fixture_block.contains("tests/test_workflow.py")
            && fixture_block.contains("workflow.advance()")
            && fixture_block.contains("test_render_output")
            && fixture_block.contains("workflow.status")
            && fixture_block.contains("COMPLETE"),
        "LanguageEvidenceAdapter registry fixture must use scenario-neutral assertion/transition evidence"
    );
    assert!(
        preflight.contains("language_evidence_adapter_registry_fixture_passes")
            && docs_block.contains("language_evidence_adapter_registry"),
        "active preflight and docs must keep the generic language_evidence_adapter_registry key"
    );
}

#[test]
fn language_terminal_state_classifier_is_scenario_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let language_adapter_path = manifest_dir
        .join("src")
        .join("agent")
        .join("language_evidence.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let language_adapter =
        fs::read_to_string(language_adapter_path.as_std_path()).expect("read language adapter");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let classifier_block = language_adapter
        .split("fn is_terminal_state_expected")
        .nth(1)
        .and_then(|tail| tail.split("fn backtick_values").next())
        .expect("terminal state classifier block");
    let docs_block = docs
        .split("LanguageEvidenceAdapter registry uses scenario-neutral parser evidence.")
        .nth(1)
        .and_then(|tail| {
            tail.split("- Python source artifact admission rejects concatenated executable modules")
                .next()
        })
        .expect("language evidence adapter registry preflight docs block");

    for forbidden in ["GAME_OVER", "game over", "game_over"] {
        assert!(
            !classifier_block.contains(forbidden),
            "terminal-state classifier must not use game/domain terminal `{forbidden}` as generic authority"
        );
        assert!(
            !docs_block.contains(forbidden),
            "LanguageEvidenceAdapter preflight docs must not use game/domain terminal `{forbidden}` as generic authority"
        );
    }
    assert!(
        classifier_block.contains("COMPLETE")
            && classifier_block.contains("COMPLETED")
            && classifier_block.contains("FINISH")
            && classifier_block.contains("ENDED")
            && classifier_block.contains("FAIL")
            && classifier_block.contains("SUCCESS"),
        "terminal-state classifier must keep scenario-neutral completion/success/failure vocabulary"
    );
}

#[test]
fn language_public_api_semantic_obligations_are_scenario_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let language_adapter_path = manifest_dir
        .join("src")
        .join("agent")
        .join("language_evidence.rs");
    let runtime_contract_path = repo_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = repo_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let language_adapter =
        fs::read_to_string(language_adapter_path.as_std_path()).expect("read language adapter");
    let runtime_contract =
        fs::read_to_string(runtime_contract_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");

    let obligation_block = language_adapter
        .split("pub(crate) fn language_public_api_data_model_semantic_obligations")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn language_public_method_sibling_obligations")
                .next()
        })
        .expect("public API semantic obligation block");
    let runtime_public_state_block = runtime_contract
        .split("public-state repair lane")
        .nth(1)
        .and_then(|tail| tail.split("runtime corrective result").next())
        .expect("runtime public-state repair block");
    let detailed_public_api_block = detailed_design
        .split("public API/data-model repair")
        .nth(1)
        .and_then(|tail| tail.split("PromptSignals").next())
        .expect("detailed public API repair block");
    let detailed_public_state_block = detailed_design
        .split("public-state assertion repair")
        .nth(1)
        .and_then(|tail| tail.split("public API/data-model repair").next())
        .expect("detailed public-state repair block");

    let forbidden_domain_authority = [
        "movement",
        "move",
        "boundary",
        "initial_positions",
        ".x",
        ".y",
        "rectangle",
        "collision",
        "projectile",
        "actor",
        "entity group",
        "world state",
    ];
    for forbidden in forbidden_domain_authority {
        assert!(
            !obligation_block.contains(forbidden),
            "generic public API semantic obligations must not use domain surface `{forbidden}` as contract authority"
        );
        assert!(
            !runtime_public_state_block.contains(forbidden),
            "runtime public-state repair design must not use domain surface `{forbidden}` as generic contract authority"
        );
        assert!(
            !detailed_public_api_block.contains(forbidden),
            "detailed public API repair design must not use domain surface `{forbidden}` as generic contract authority"
        );
        assert!(
            !detailed_public_state_block.contains(forbidden),
            "detailed public-state repair design must not use domain surface `{forbidden}` as generic contract authority"
        );
    }
    assert!(
        obligation_block.contains("language_public_state_assertion_observations")
            && obligation_block.contains("public state assertion compatibility")
            && obligation_block.contains("caller-visible public state")
            && runtime_public_state_block.contains("typed assertion subjects")
            && detailed_public_api_block.contains("typed assertion subjects"),
        "public API semantic obligations must derive scenario-neutral obligations from typed assertion subjects, observations, and call-site/value evidence"
    );
}

#[test]
fn state_requested_work_continuation_fixtures_are_harness_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let continuation_block = state
        .split("pub(crate) fn verification_failure_labels_are_not_requested_work_targets_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn requested_work_completion_promotes_verification_fixture_passes")
                .next()
        })
        .expect("state requested-work continuation fixture block");
    let loader_block = state
        .split("pub(crate) fn verification_failure_ignores_runtime_loader_frame_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("pub(crate) fn out_of_order_history_items_use_sequence_authority_for_repair_fixture_passes").next())
        .expect("runtime loader frame fixture block");
    let source_owned_block = state
        .split("pub(crate) fn source_owned_verification_failure_preserves_recent_source_edit_target_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("pub(crate) fn verification_timeout_preserves_recent_source_repair_target_fixture_passes").next())
        .expect("source-owned verification fixture block");
    let diagnostic_label_block = state
        .split("pub(crate) fn verification_failure_diagnostic_labels_do_not_become_repair_targets_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("pub(crate) fn synthetic_tool_feedback_preserves_real_verification_cluster_fixture_passes").next())
        .expect("diagnostic label fixture block");
    let docs_block = docs
        .split("State reducer language evidence is covered by the same adapter family.")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "- Repair lane generated-test evidence is covered by the same adapter family.",
            )
            .next()
        })
        .expect("state reducer preflight docs block");
    let audited = format!(
        "{continuation_block}\n{loader_block}\n{source_owned_block}\n{diagnostic_label_block}\n{docs_block}"
    );

    for forbidden in [
        "Manual ST closeout continuation",
        "Manual ST verification-repair continuation",
        "test_arcade_game",
        "arcade_game",
        "rects_overlap",
        "bullet",
        "collision",
        "BEH-3",
        "BEH-4",
        "C:/Users/example",
        "C:\\Users\\example",
        "C:\\Python313",
        "project_sandbox/route",
    ] {
        assert!(
            !audited.contains(forbidden),
            "state requested-work / verification continuation fixtures must not use manual ST, host path, or representative domain surface `{forbidden}` as contract authority"
        );
    }
    assert!(
        continuation_block.contains("Typed route closeout continuation.")
            && continuation_block.contains("Typed verification-repair continuation.")
            && continuation_block
                .contains("workspace_root = Utf8Path::new(\"C:/workspace/project\")")
            && docs_block.contains("state_requested_work_continuation_fixture_surface"),
        "state continuation fixture surface must use route-neutral typed continuation samples and documented preflight wording"
    );
}

#[test]
fn state_verification_repair_continuation_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let fixture_block = state
        .split("pub(crate) fn verification_repair_continuation_projects_repair_state_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn verification_repair_continuation_existing_byproduct_path_is_not_repair_target_fixture_passes")
                .next()
        })
        .expect("verification-repair continuation fixture block");
    let docs_line = docs
        .lines()
        .find(|line| {
            line.contains("state_verification_repair_continuation_fixture_language_neutral")
        })
        .expect("state verification-repair continuation preflight docs line");
    let audited = format!("{fixture_block}\n{docs_line}");

    for forbidden in [
        "widget.py",
        "test_widget.py",
        "docs/widget-contract.md",
        "src/component.rs",
        "tests/component.behavior.md",
        "docs/component-contract.md",
        ".py",
        "python -m unittest",
        "unittest",
    ] {
        assert!(
            !audited.contains(forbidden),
            "generic state verification-repair continuation fixture must not use language-specific surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.behavior.md")
            && fixture_block.contains("docs/workflow-contract.md")
            && fixture_block.contains("verify-contract --behavior")
            && fixture_block.contains("typed verification continuation evidence")
            && docs_line
                .contains("state_verification_repair_continuation_fixture_language_neutral"),
        "state verification-repair continuation fixture must use language-neutral artifact roles and typed verification command evidence"
    );
}

#[test]
fn state_verification_repair_byproduct_fixtures_are_toolchain_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let fixture_block = state
        .split("pub(crate) fn verification_repair_continuation_existing_byproduct_path_is_not_repair_target_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn generic_generated_test_source_call_site_targets_source_without_python_suffix_fixture_passes")
                .next()
        })
        .expect("verification-repair byproduct continuation fixture block");
    let docs_line = docs
        .lines()
        .find(|line| line.contains("state_verification_repair_byproduct_fixture_toolchain_neutral"))
        .expect("state verification-repair byproduct preflight docs line");
    let audited = format!("{fixture_block}\n{docs_line}");

    for forbidden in [
        "cargo test",
        "target/cache",
        "target/",
        "Cargo",
        "runtime.snapshot",
        "src/component.rs",
        "tests/component.behavior.md",
    ] {
        assert!(
            !audited.contains(forbidden),
            "generic state verification-repair byproduct fixture must not use toolchain-specific surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("build-artifacts/cache/verification.snapshot")
            && fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("verify-contract --behavior")
            && docs_line.contains("state_verification_repair_byproduct_fixture_toolchain_neutral"),
        "state verification-repair byproduct fixture must use language-neutral byproduct and command evidence"
    );
}

#[test]
fn state_public_command_continuation_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let fixture_block = state
        .split("pub(crate) fn public_command_contract_continuation_projects_compact_source_repair_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn verification_repair_continuation_generated_test_parse_target_fixture_passes")
                .next()
        })
        .expect("public-command continuation fixture block");
    let docs_block = docs
        .split("State reducer language evidence is covered by the same adapter family.")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "- Repair lane generated-test evidence is covered by the same adapter family.",
            )
            .next()
        })
        .expect("state reducer preflight docs block");
    for forbidden in [
        "tool.py",
        "test_tool.py",
        "def main",
        "input()",
        "unittest",
        "python -X utf8",
        "Python",
        "src/component.rs",
        "tests/component.command-contract",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic state public-command continuation fixture must not use language-specific command surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.command-contract")
            && fixture_block.contains("verify-public-command --scenario compact-source")
            && fixture_block.contains("typed public command contract evidence")
            && docs_block.contains("state_public_command_continuation_fixture_language_neutral"),
        "state public-command continuation fixture must use language-neutral public-command contract evidence"
    );
}

#[test]
fn state_generated_test_parse_continuation_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let fixture_block = state
        .split("pub(crate) fn verification_repair_continuation_generated_test_parse_target_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn requested_work_completion_promotes_verification_fixture_passes")
                .next()
        })
        .expect("generated-test parse continuation fixture block");
    let docs_block = docs
        .split("State reducer language evidence is covered by the same adapter family.")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "- Repair lane generated-test evidence is covered by the same adapter family.",
            )
            .next()
        })
        .expect("state reducer preflight docs block");

    for forbidden in [
        "widget.py",
        "test_widget.py",
        "python -m unittest",
        "unittest",
        "loader.py",
        "SyntaxError",
        "triple-quoted",
        "Traceback",
        "C:\\Runtime",
        "src/component.ts",
        "tests/component.test.ts",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic generated-test parse continuation fixture must not use language-specific parse surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("tests/workflow.spec.ts")
            && fixture_block.contains("src/workflow.ts")
            && fixture_block.contains("verify-generated-test --parse")
            && fixture_block.contains("typed generated-test parse-defect evidence")
            && docs_block
                .contains("state_generated_test_parse_continuation_fixture_language_neutral"),
        "generated-test parse continuation fixture must use language-neutral parse-defect evidence and artifact-role coordinates"
    );
}

#[test]
fn state_authoring_completion_no_progress_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let fixture_block = state
        .split("pub(crate) fn requested_work_completion_promotes_verification_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn scenario_contract_reference_input_does_not_become_authoring_target_fixture_passes")
                .next()
        })
        .expect("authoring completion / no-progress fixture block");
    let docs_block = docs
        .split("State reducer language evidence is covered by the same adapter family.")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "- Repair lane generated-test evidence is covered by the same adapter family.",
            )
            .next()
        })
        .expect("state reducer preflight docs block");
    for forbidden in [
        "component.py",
        "test_component.py",
        "source.py",
        "test_source.py",
        "python -m unittest",
        "import unittest",
        "unittest.TestCase",
        "src/component.rs",
        "tests/component.behavior.md",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic authoring completion / no-progress fixture must not use Python/unittest surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.behavior.md")
            && fixture_block.contains("verify-contract --behavior")
            && fixture_block.contains("typed authoring completion evidence")
            && fixture_block.contains("typed invalid edit no-progress evidence")
            && fixture_block.contains("typed empty artifact no-progress evidence")
            && docs_block
                .contains("state_authoring_completion_no_progress_fixture_language_neutral"),
        "authoring completion / no-progress fixtures must use typed language-neutral artifact roles, synthetic verification labels, and documented preflight wording"
    );
}

#[test]
fn state_missing_todo_graph_authoring_fixture_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let fixture_block = state
        .split("pub(crate) fn requested_work_missing_todo_graph_stays_authoring_authority")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn partial_requested_work_remains_authoring_phase_fixture_passes",
            )
            .next()
        })
        .expect("missing todo graph authoring fixture block");
    for forbidden in ["component.py", "src/component.rs", "python -m unittest"] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic missing-todo-graph authoring fixture must not use Python/unittest surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("verify-contract --behavior")
            && fixture_block.contains("RequestedWorkAuthoring"),
        "missing-todo-graph authoring fixture must use typed language-neutral artifact-role coordinates and synthetic verification command labels"
    );
}

#[test]
fn state_continuation_route_inventory_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let fixture_block = state
        .split("pub(crate) fn verification_failure_labels_are_not_requested_work_targets_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn verification_repair_continuation_projects_repair_state_fixture_passes")
                .next()
        })
        .expect("continuation / route inventory fixture block");
    for forbidden in [
        "component.py",
        "test_component.py",
        "widget.py",
        "test_widget.py",
        "python -m unittest",
        "import unittest",
        "def add",
        "parse_and_evaluate",
        "sys.argv",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic continuation / route inventory fixture must not use Python/widget surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.behavior.md")
            && fixture_block.contains("docs/workflow-design.md")
            && fixture_block.contains("verify-contract --behavior"),
        "continuation / route inventory fixtures must use language-neutral source/test/docs artifact-role coordinates and synthetic verification command labels"
    );
}

#[test]
fn state_generated_test_callsite_fixtures_are_domain_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let fixture_block = state
        .split("pub(crate) fn generic_generated_test_source_call_site_targets_source_without_python_suffix_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn public_command_contract_continuation_projects_compact_source_repair_fixture_passes")
                .next()
        })
        .expect("generated-test call-site fixture block");
    for forbidden in [
        "widget.spec.ts",
        "src/widget.ts",
        "renderWidget",
        "widget output",
        "npm test -- widget.spec.ts",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic generated-test call-site fixture must not use widget-specific surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("tests/workflow.spec.ts")
            && fixture_block.contains("src/workflow.ts")
            && fixture_block.contains("renderOperation")
            && fixture_block.contains("verify-generated-test --callsite"),
        "generated-test call-site fixtures must use language-neutral generated-test/source coordinates, generic operation symbols, and synthetic verification labels"
    );
}

#[test]
fn state_public_class_attribute_cluster_uses_single_source_coordinate() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let fixture_block = state
        .split("pub(crate) fn public_class_attribute_cluster_fixture")
        .nth(1)
        .and_then(|tail| tail.split("fn is_scenario_contract_ref").next())
        .expect("public class attribute cluster fixture block");

    assert!(
        !fixture_block.contains("src/workflow.rs"),
        "public-class-attribute cluster fixture must not mix stale Rust source coordinate with TypeScript source authority"
    );
    assert!(
        fixture_block.contains("target: Some(\"src/workflow.ts\".to_string())")
            && fixture_block.contains("source_refs: vec![\"src/workflow.ts\".to_string(), \"workflow result\".to_string()]")
            && fixture_block.contains("test_refs: vec![\"tests/workflow.spec.ts\".to_string()]"),
        "public-class-attribute cluster fixture must use one exact TypeScript source coordinate across evidence and cluster projections"
    );
}

#[test]
fn state_scenario_contract_reference_fixture_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let fixture_block = state
        .split("pub(crate) fn scenario_contract_reference_input_does_not_become_authoring_target_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn same_document_update_uses_prior_authored_doc_not_contract_ref_fixture_passes")
                .next()
        })
        .expect("scenario contract reference fixture block");
    for forbidden in ["component.py", "test_component.py", "python -m unittest"] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic scenario-contract reference fixture must not use Python/unittest surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.behavior.md")
            && fixture_block.contains("verify-contract --behavior")
            && fixture_block.contains("scenario_contract.md")
            && fixture_block.contains("scenario_contract.json"),
        "scenario-contract reference fixture must use language-neutral source/test artifact-role coordinates, synthetic verification labels, and refs-only scenario contract evidence"
    );
}

#[test]
fn state_docs_route_same_document_and_relative_workspace_fixtures_are_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let fixture_block = state
        .split("pub(crate) fn same_document_update_uses_prior_authored_doc_not_contract_ref_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn verification_failure_active_work_outranks_stale_docs_route_fixture_passes")
                .next()
        })
        .expect("docs-route same-document / relative-workspace fixture block");
    for forbidden in [
        "component.py",
        "test_component.py",
        "unittest",
        "calculate",
        "関数電卓",
        "四則演算",
        "sqrt",
        "pow",
        "docs/component-design.md",
        "docs/tool-design.md",
        "scenario_contract.component.v1",
        "fr10_018_fixture_workspace",
        "python -m unittest",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic docs-route same-document / relative-workspace fixture must not use Python/calculator/FR10 surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.behavior.md")
            && fixture_block.contains("docs/workflow-design.md")
            && fixture_block.contains("scenario_contract.workflow.v1")
            && fixture_block.contains("relative-workspace-fixture")
            && fixture_block.contains("verify-contract --behavior"),
        "docs-route same-document / relative-workspace fixtures must use language-neutral source/test/docs artifact-role coordinates, synthetic verification labels, workflow scenario-contract refs, and non-FR relative workspace coordinates"
    );
}

#[test]
fn state_verification_docs_promotion_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let fixture_block = state
        .split("pub(crate) fn verification_failure_active_work_outranks_stale_docs_route_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn requested_work_without_verification_closes_after_file_change_fixture_passes")
                .next()
        })
        .expect("verification/docs-promotion fixture block");
    for forbidden in [
        "src/widget.rs",
        "tests/widget_contract.rs",
        "build_widget",
        "rust-widget-api",
        "cargo test",
        "widget.py",
        "test_widget.py",
        "docs/widget-design.md",
        "python -m unittest",
        "Ran 24 tests",
        "unittest",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic verification/docs-promotion fixture must not use widget/Cargo/Python surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.behavior.md")
            && fixture_block.contains("docs/workflow-design.md")
            && fixture_block.contains("verify-contract --behavior")
            && fixture_block.contains("workflow-api-contract")
            && fixture_block.contains("execute_workflow"),
        "verification/docs-promotion fixtures must use language-neutral source/test/docs artifact roles, synthetic verification labels, and generic workflow symbols"
    );
}

#[test]
fn state_metadata_only_filechange_fixture_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let fixture_block = state
        .split("pub(crate) fn metadata_only_tool_output_does_not_satisfy_file_change_authority_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn structured_document_summary_waits_for_remaining_sources_fixture_passes")
                .next()
        })
        .expect("metadata-only file-change authority fixture block");
    for forbidden in ["component.py", "Write component.py"] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic metadata-only FileChange authority fixture must not use Python component surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("Write src/workflow.rs")
            && fixture_block.contains("\"changed_files\": [\"src/workflow.rs\"]"),
        "metadata-only FileChange authority fixture must use a language-neutral source artifact coordinate while preserving metadata-only diagnostic evidence"
    );
}

#[test]
fn state_reference_design_verification_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let fixture_block = state
        .split("pub(crate) fn required_verification_survives_authoring_completion_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn japanese_prompt_filename_boundaries_remain_artifact_targets_fixture_passes")
                .next()
        })
        .expect("required-verification/reference-design fixture block");
    for forbidden in [
        "component.py",
        "test_component.py",
        "docs/component-design.md",
        "src/widget.rs",
        "tests/widget_spec.rs",
        "cargo test",
        "python -m unittest",
        "unittest",
        "calculator",
        "`pow`",
        "`mod`",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic reference-design / required-verification fixture cluster must not use Python/widget/Cargo/calculator surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.behavior.md")
            && fixture_block.contains("docs/workflow-design.md")
            && fixture_block.contains("verify-contract --behavior"),
        "reference-design / required-verification fixtures must use language-neutral source/test/docs artifact roles and synthetic verification labels"
    );
}

#[test]
fn state_docs_output_verification_repair_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let fixture_block = state
        .split(
            "pub(crate) fn docs_output_referenced_code_does_not_become_pending_authoring_target_fixture_passes",
        )
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn post_repair_file_change_promotes_verification_rerun_fixture_passes")
                .next()
        })
        .expect("docs-output / verification-repair fixture block");
    for forbidden in [
        "hello.py",
        "test_hello.py",
        "component.py",
        "test_component.py",
        "widget.py",
        "test_widget.py",
        "workflow.py",
        "test_workflow.py",
        "python -m unittest",
        "python -X utf8 -m unittest",
        "unittest",
        "AttributeError",
        "ImportError",
        "component.",
        "widget.",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic docs-output / verification-repair fixture cluster must not use Python/unittest/component/widget surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.behavior.md")
            && fixture_block.contains("docs/workflow-readme.md")
            && fixture_block.contains("verify-contract --behavior")
            && fixture_block.contains("workflow-repair-contract"),
        "docs-output / verification-repair fixtures must use language-neutral source/test/docs artifact roles, synthetic verification labels, and typed verification evidence clusters"
    );
}

#[test]
fn state_post_repair_verification_transition_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let fixture_block = state
        .split("pub(crate) fn post_repair_file_change_promotes_verification_rerun_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn resumed_new_user_turn_ignores_prior_closeout_fixture_passes")
                .next()
        })
        .expect("post-repair verification transition fixture block");
    for forbidden in [
        "component.py",
        "test_component.py",
        "widget.py",
        "test_widget.py",
        "python -m unittest",
        "python -X utf8 -m unittest",
        "unittest",
        "__pycache__",
        "cpython-313",
        "chcp 65001",
        "PYTHONIOENCODING",
        "component.",
        "widget.",
        "TestCliSubprocess",
        "test_valid_addition",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic post-repair verification transition fixture cluster must not use Python/unittest/component/widget surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.behavior.md")
            && fixture_block.contains("verify-contract --behavior")
            && fixture_block.contains("build-artifacts/cache/verification.snapshot")
            && fixture_block.contains("workflow-repair-contract")
            && fixture_block.contains("satisfies_command_identities"),
        "post-repair verification transition fixtures must use language-neutral workflow artifact roles, neutral byproduct paths, synthetic verification labels, typed failure clusters, and command identity evidence"
    );
}

#[test]
fn state_authoring_verification_repair_transition_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let fixture_block = state
        .split("pub(crate) fn resumed_new_user_turn_ignores_prior_closeout_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn source_owned_requirement_refs_align_active_work_with_repair_lane_fixture_passes")
                .next()
        })
        .expect("authoring / verification / repair transition fixture block");
    let docs_line = docs
        .lines()
        .find(|line| {
            line.contains("state_authoring_verification_repair_transition_fixture_language_neutral")
        })
        .expect("state authoring/verification/repair transition preflight docs line");
    let audited = format!("{fixture_block}\n{docs_line}");

    for forbidden in [
        "component.py",
        "test_component.py",
        "app.py",
        "test_app.py",
        "python -m py_compile",
        "python -m unittest",
        "unittest",
        "component.calculate",
        "subprocess.run([sys.executable",
        "sys.executable, \"component.py\"",
        "TestCase",
    ] {
        assert!(
            !audited.contains(forbidden),
            "generic authoring / verification / repair transition fixture cluster must not use Python/unittest/component/app surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.behavior.md")
            && fixture_block.contains("verify-contract --behavior")
            && fixture_block.contains("workflow-partial-contract")
            && fixture_block.contains("workflow-repair-contract")
            && fixture_block.contains("satisfies_command_identities")
            && docs_line.contains(
                "state_authoring_verification_repair_transition_fixture_language_neutral"
            ),
        "authoring / verification / repair transition fixtures must use language-neutral workflow artifact roles, synthetic verification labels, typed command identity evidence, and documented preflight wording"
    );
}

#[test]
fn state_source_generated_repair_authority_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let fixture_block = state
        .split("pub(crate) fn source_owned_requirement_refs_align_active_work_with_repair_lane_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn is_scenario_contract_ref").next())
        .expect("source / generated-test repair authority fixture block");
    let docs_line = docs
        .lines()
        .find(|line| {
            line.contains("state_source_generated_repair_authority_fixture_language_neutral")
        })
        .expect("state source/generated repair preflight docs line");
    let audited = format!("{fixture_block}\n{docs_line}");

    for forbidden in [
        "widget.py",
        "test_widget.py",
        "component.py",
        "test_component.py",
        "python -m unittest",
        "python -X utf8 -m unittest",
        "unittest",
        "TestCase",
        "self.assert",
        "AssertionError",
        "Traceback",
        "ValueError",
        "ZeroDivisionError",
        "AttributeError",
        "NameError",
        "TypeError",
        "SyntaxError",
        "widget.",
        "component.",
        "calculate_binary",
        "component.calculate",
        "divide(10, 0)",
        "10 + 5",
        "NO TESTS RAN",
        "sys.environ",
        "inspect.getsource",
    ] {
        assert!(
            !audited.contains(forbidden),
            "generic source / generated-test repair authority fixture cluster must not use Python/unittest/widget/component surface `{forbidden}` as lifecycle authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.ts")
            && fixture_block.contains("tests/workflow.spec.ts")
            && fixture_block.contains("verify-contract --behavior")
            && fixture_block.contains("workflow-source-contract")
            && fixture_block.contains("generated_test_artifact_api_misuse")
            && fixture_block.contains("generated_test_local_binding_contradiction")
            && fixture_block.contains("repair_control_snapshot")
            && fixture_block.contains("satisfies_command_identities")
            && docs_line
                .contains("state_source_generated_repair_authority_fixture_language_neutral"),
        "source / generated-test repair authority fixtures must use workflow artifact roles, synthetic verification labels, typed repair owner snapshots, command identity evidence, and documented preflight wording"
    );
}

#[test]
fn state_docs_route_verification_target_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state_path = manifest_dir.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let fixture_block = state
        .split("pub(crate) fn docs_route_verification_failure_preserves_docs_active_target_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("docs-route verification target fixture block");

    for forbidden in [
        "docs/component-design.md",
        "component.py",
        "test_component.py",
        "python -X utf8 -c",
        "AssertionError: component.py",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "generic docs-route verification target fixture must not use Python/component surface `{forbidden}` as docs-route target authority"
        );
    }
    assert!(
        fixture_block.contains("docs/workflow-design.md")
            && fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.behavior.md")
            && fixture_block.contains("verify-contract --behavior")
            && fixture_block.contains("docs_route_verification_target_fixture_language_neutral"),
        "docs-route verification target fixture must use workflow source/test/docs artifact roles, synthetic verification labels, and documented language-neutral authority"
    );
}

#[test]
fn repair_lane_source_owned_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let repair_lane_path = manifest_dir
        .join("src")
        .join("agent")
        .join("repair_lane.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let repair_lane = fs::read_to_string(repair_lane_path.as_std_path()).expect("read repair lane");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let fixture_block = repair_lane
        .split("pub(crate) fn source_owned_verification_repair_lane_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn source_config_repair_lane_preserves_common_repair_authority_fixture_passes")
                .next()
        })
        .expect("source-owned repair lane fixture block");
    let docs_line = docs
        .lines()
        .find(|line| line.contains("repair_lane_source_owned_fixture_language_neutral"))
        .unwrap_or_default();
    let audited = format!("{fixture_block}\n{docs_line}");

    for forbidden in [
        "component.py",
        "test_component.py",
        "python -m unittest",
        "component.calculate",
        "test_calculate_add",
    ] {
        assert!(
            !audited.contains(forbidden),
            "generic source-owned repair lane fixture must not use Python/unittest/component surface `{forbidden}` as repair authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.behavior.md")
            && fixture_block.contains("verify-contract --behavior")
            && fixture_block.contains("workflow-source-contract")
            && fixture_block.contains("repair_control_snapshot")
            && docs_line.contains("repair_lane_source_owned_fixture_language_neutral"),
        "source-owned repair lane fixture must use workflow artifact roles, synthetic verification labels, typed repair owner snapshots, and documented preflight wording"
    );
}

#[test]
fn repair_lane_adjacent_authority_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let repair_lane_path = manifest_dir
        .join("src")
        .join("agent")
        .join("repair_lane.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let repair_lane = fs::read_to_string(repair_lane_path.as_std_path()).expect("read repair lane");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let fixture_block = repair_lane
        .split("pub(crate) fn docs_route_pending_verification_failure_projects_docs_repair_lane_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn generated_test_subprocess_output_capture_missing_projects_test_repair_fixture_passes")
                .next()
        })
        .expect("adjacent repair-lane docs/source/generated fixture block");
    let docs_line = docs
        .lines()
        .find(|line| line.contains("repair_lane_adjacent_authority_fixture_language_neutral"))
        .unwrap_or_default();
    let audited = format!("{fixture_block}\n{docs_line}");

    for forbidden in [
        "docs/component-design.md",
        "component.py",
        "test_component.py",
        "arcade_game.py",
        "test_arcade_game.py",
        "widget.py",
        "test_widget.py",
        "test_tool.py",
        "python -m unittest",
        "python -X utf8 -m unittest",
        "python -X utf8 -c",
        "unittest",
        "Traceback",
        "AssertionError",
        "self.assert",
        "ToolCliTests",
        "ComponentTest",
        "widget.",
        "component.",
        "arcade",
    ] {
        assert!(
            !audited.contains(forbidden),
            "generic adjacent repair-lane fixture cluster must not use Python/unittest/widget/component surface `{forbidden}` as repair authority"
        );
    }
    assert!(
        fixture_block.contains("docs/workflow-design.md")
            && fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.behavior.md")
            && fixture_block.contains("tests/workflow.spec.ts")
            && fixture_block.contains("verify-contract --behavior")
            && fixture_block.contains("verify-generated-test --collection")
            && fixture_block.contains("workflow-source-contract")
            && fixture_block.contains("workflow-generated-test-contract")
            && fixture_block.contains("repair_control_snapshot")
            && docs_line.contains("repair_lane_adjacent_authority_fixture_language_neutral"),
        "adjacent repair-lane fixture cluster must use workflow source/test/docs artifact roles, synthetic verification labels, adapter-owned evidence, typed repair snapshots, and documented preflight wording"
    );
}

#[test]
fn repair_lane_generated_test_fixture_cluster_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let repair_lane_path = manifest_dir
        .join("src")
        .join("agent")
        .join("repair_lane.rs");
    let docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let repair_lane = fs::read_to_string(repair_lane_path.as_std_path()).expect("read repair lane");
    let docs = fs::read_to_string(docs_path.as_std_path()).expect("read preflight gate suite");
    let fixture_block = repair_lane
        .split("pub(crate) fn generated_test_subprocess_output_capture_missing_projects_test_repair_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn public_command_contract_failure_projects_compact_source_repair_fixture_passes")
                .next()
        })
        .expect("generated-test repair lane fixture cluster");
    let docs_line = docs
        .lines()
        .find(|line| line.contains("repair_lane_generated_test_fixture_language_neutral"))
        .unwrap_or_default();
    let audited = format!("{fixture_block}\n{docs_line}");

    for forbidden in [
        "component.py",
        "test_component.py",
        "widget.py",
        "test_widget.py",
        "tool.py",
        "test_tool.py",
        "python -m unittest",
        "python -X utf8 -m unittest",
        "unittest",
        "Traceback",
        "self.assert",
        "TestComponent",
        "TestWidget",
        "TestCli",
        "inspect.getsource",
        "sys.environ",
        "component.",
        "widget.",
        "tool.",
    ] {
        assert!(
            !audited.contains(forbidden),
            "generic generated-test repair lane fixture cluster must not use Python/unittest/component/widget surface `{forbidden}` as repair authority"
        );
    }
    assert!(
        fixture_block.contains("src/workflow.rs")
            && fixture_block.contains("tests/workflow.spec.ts")
            && fixture_block.contains("verify-generated-test --subprocess")
            && fixture_block.contains("verify-generated-test --parse")
            && fixture_block.contains("verify-generated-test --api")
            && fixture_block.contains("workflow-generated-test-contract")
            && fixture_block.contains("repair_control_snapshot")
            && docs_line.contains("repair_lane_generated_test_fixture_language_neutral"),
        "generated-test repair lane fixture cluster must use workflow source/test artifact roles, synthetic verification labels, adapter-owned evidence, typed repair snapshots, and documented preflight wording"
    );
}

#[test]
fn verification_contract_does_not_keep_rust_specific_upper_field() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir
        .join("src")
        .join("agent")
        .join("verification.rs");
    let text = fs::read_to_string(path.as_std_path()).expect("read verification module");
    for stale_upper_contract in [
        "rust_build",
        "RUST_BUILD_TOKENS",
        "implies_rust_project_verification",
        "Rust compile verification",
    ] {
        assert!(
            !text.contains(stale_upper_contract),
            "verification upper lifecycle contract still contains Rust-specific field `{stale_upper_contract}`"
        );
    }
    assert!(
        text.contains("build_check"),
        "verification contract should expose generic build_check authority"
    );
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
fn docs_semantic_claim_projection_uses_operation_authority() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let docs_semantic_path = repo_root
        .join("src")
        .join("agent")
        .join("docs_semantic_contract.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let docs_semantic =
        fs::read_to_string(docs_semantic_path.as_std_path()).expect("read docs semantic source");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let claim_projection_block = docs_semantic
        .split("fn documentation_semantic_contract_from_authority")
        .nth(1)
        .and_then(|tail| tail.split("fn reconcile_documentation_semantics").next())
        .expect("docs semantic claim projection block");

    for stale_projection in [
        "id: \"unknown_two_token_cli_as_unsupported_function_exit_2\"",
        "id: \"unknown_two_token_cli_as_undefined_function_exit_2\"",
        "unsupported function exit code 2.",
        "undefined function exit code 2.",
    ] {
        assert!(
            !claim_projection_block.contains(stale_projection),
            "docs semantic claim projection must not expose stale function-specific projection `{stale_projection}`"
        );
    }
    for required_projection in [
        "unknown_two_token_cli_as_unsupported_command_exit_2",
        "unknown_two_token_cli_as_undefined_operation_exit_2",
        "unsupported command exit code 2",
        "undefined operation exit code 2",
        "docs_semantic_claim_projection_operation_authority_fixture_passes",
        "docs_semantic_claim_projection_operation_authority",
    ] {
        assert!(
            docs_semantic.contains(required_projection)
                || preflight.contains(required_projection)
                || preflight_docs.contains(required_projection),
            "docs semantic source, active preflight, or preflight docs must contain operation/command projection `{required_projection}`"
        );
    }
}

#[test]
fn public_command_contract_coverage_is_not_unittest_only() {
    assert!(moyai::agent::public_command_contract::public_command_contract_fixture_passes());
    assert!(moyai::harness::manual_st::public_command_contract_route_evidence_fixture_passes());
    assert!(
        moyai::harness::manual_st::post_repair_route_verification_clears_stale_repair_fixture_passes()
    );
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
fn provider_replay_omits_inactive_target_content_shape_executable_pair() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root.parent().expect("crate has repository parent");
    let prompt = std::fs::read_to_string(root.join("src/agent/prompt.rs"))
        .expect("prompt source is readable");
    let preflight = std::fs::read_to_string(root.join("src/harness/preflight.rs"))
        .expect("preflight source is readable");
    let preflight_doc =
        std::fs::read_to_string(repo_root.join("docs/testing/PreflightGateSuite.md"))
            .expect("preflight doc is readable");
    let runtime_contracts =
        std::fs::read_to_string(repo_root.join("docs/design/runtime-contracts.md"))
            .expect("runtime contracts doc is readable");
    let detailed_design = std::fs::read_to_string(repo_root.join("docs/design/detailed-design.md"))
        .expect("detailed design doc is readable");
    let item_lifecycle =
        std::fs::read_to_string(repo_root.join("docs/design/itemlifecycle-detail-design.md"))
            .expect("item lifecycle design doc is readable");

    assert!(
        prompt.contains("inactive_target_content_shape_replay_is_target_exclusive_fixture_passes")
            && prompt.contains("inactive_target_content_shape_executable_pair_omitted")
            && prompt.contains("inactive_target_content_shape_pair_replay_note"),
        "provider replay must expose inactive-target content-shape replay as target-exclusive non-executable evidence"
    );
    assert!(
        preflight.contains("inactive_target_content_shape_replay_is_target_exclusive"),
        "active preflight must execute the inactive-target content-shape replay target-exclusive fixture"
    );
    assert!(preflight_doc.contains("inactive_target_content_shape_replay_is_target_exclusive"));
    assert!(runtime_contracts.contains("inactive_target_content_shape_replay_is_target_exclusive"));
    assert!(detailed_design.contains("inactive_target_content_shape_replay_is_target_exclusive"));
    assert!(docs_contains_or_item_lifecycle_current_authority(
        &item_lifecycle,
        "inactive_target_content_shape_replay_is_target_exclusive"
    ));
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

    let malformed = run_artifact_replay_preflight(&root, Vec::new()).expect("preflight report");
    assert_eq!(malformed.status, PreflightResultStatus::Fail);
    assert!(malformed.results[0].diagnostics.iter().any(|line| {
        line.contains("malformed route evidence artifacts")
            && line.contains("route_manifest.json.route_id")
            && line.contains("preflight_report.json.generated_by")
    }));
    assert!(
        moyai::harness::preflight::artifact_replay_rejects_empty_route_evidence_fixture_passes()
    );
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
    assert!(
        moyai::protocol::edit_only_authoring_grounding_recovery_narrows_action_surface_fixture_passes()
    );
    assert!(moyai::protocol::non_python_edit_projection_uses_language_adapter_fixture_passes());
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
        access_mode: moyai::config::AccessMode::Default,
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
        access_mode: moyai::config::AccessMode::Default,
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
        access_mode: moyai::config::AccessMode::Default,
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
fn turn_item_projection_role_keeps_internal_cache_out_of_primary_transcript() {
    assert!(
        moyai::protocol::turn_item_internal_projection_roles_are_not_primary_display_fixture_passes(
        )
    );
    assert!(
        moyai::tui::state::tui_primary_transcript_omits_internal_projection_items_fixture_passes()
    );
}

#[test]
fn cli_session_show_uses_canonical_history_not_compatibility_transcript() {
    assert!(moyai::cli::render::cli_history_renderer_uses_canonical_transcript_projection_fixture_passes());
    assert!(
        moyai::cli::render::cli_history_renderer_ignores_compatibility_transcript_fixture_passes()
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
        access_mode: moyai::config::AccessMode::Default,
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
    let prompt_assets = fs::read_to_string(manifest_dir.join("src/agent/prompt_assets.rs"))
        .expect("read prompt assets source");
    let docs_semantic_contract =
        fs::read_to_string(manifest_dir.join("src/agent/docs_semantic_contract.rs"))
            .expect("read docs semantic contract source");
    let state =
        fs::read_to_string(manifest_dir.join("src/agent/state.rs")).expect("read state source");
    let completion_guard = fs::read_to_string(manifest_dir.join("src/agent/completion_guard.rs"))
        .expect("read completion guard source");
    let shell_tool =
        fs::read_to_string(manifest_dir.join("src/tool/shell.rs")).expect("read shell tool source");
    let loop_impl = fs::read_to_string(manifest_dir.join("src/agent/loop_impl.rs"))
        .expect("read loop implementation source");
    let run_service = fs::read_to_string(manifest_dir.join("src/app/run_service.rs"))
        .expect("read run service source");
    let session_service = fs::read_to_string(manifest_dir.join("src/session/service.rs"))
        .expect("read session service source");
    let session_repository = fs::read_to_string(manifest_dir.join("src/session/repository.rs"))
        .expect("read session repository trait source");
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
        prompt.contains("classify_language_artifact_target(target)")
            && !prompt.contains("\"java\"\n                | \"kt\"")
            && !prompt.contains("\"toml\"\n                | \"yaml\""),
        "prompt artifact target classification must consume LanguageEvidenceAdapter roles instead of keeping a local implementation extension allowlist"
    );
    assert!(
        prompt_assets.contains("verification_repair_prompt_projection")
            && prompt_assets.contains("LanguageFamily::Python")
            && prompt_assets
                .contains("verification_repair_prompt_uses_language_projection_fixture_passes")
            && !prompt_assets.contains("log 10")
            && !prompt_assets.contains("subprocess argv"),
        "verification repair prompt guidance must be projected from language evidence context instead of leaking Python/manual-ST examples into generic repair turns"
    );
    let unknown_two_token_body = docs_semantic_contract
        .split("fn mentions_unknown_two_token_cli(normalized: &str) -> bool")
        .nth(1)
        .and_then(|tail| tail.split("fn normalize_semantic_text").next())
        .expect("unknown two-token CLI detector body");
    assert!(
        unknown_two_token_body.contains("unknown")
            && unknown_two_token_body.contains("two token")
            && !unknown_two_token_body.contains("log 10")
            && !unknown_two_token_body.contains("tool.py")
            && !unknown_two_token_body.contains("calculator.py"),
        "docs semantic unknown-two-token detector must be term/shape based, not keyed to a manual-ST command example"
    );
    assert!(
        !state.contains("latest_verification_failure_context(transcript"),
        "repair focus must not be reconstructed from compatibility Transcript"
    );
    assert!(
        !state.contains("fn extract_python_traceback_path")
            && !state.contains("fn extract_import_error_module_path")
            && !state.contains("fn test_requirement_contexts_for_unittest_failure")
            && !state.contains("fn test_assertion_contexts_for_unittest_failure")
            && !state.contains("fn requirement_ids_for_unittest_label_from_source")
            && !state.contains("fn local_unittest_assertion_subjects")
            && !state.contains("fn generated_test_local_binding_contradiction_for_label")
            && !state.contains("fn duplicate_destructuring_identifiers")
            && !state.contains("fn local_boolean_assertion_subjects")
            && !state.contains("\"python\" | \"python.exe\" | \"py\"")
            && !state.contains("format!(\"tests/{module_path}.py\")")
            && !state.contains("|| lower.contains(\"unittest\")"),
        "state reducer must not own language-specific traceback, import-error, or unittest context parsing; language_evidence adapter owns those projections"
    );
    assert!(
        tool_runtime.contains("language_verification_command_evidence(&lower)")
            && !tool_runtime.contains(".iter()\n    .any(|needle|")
            && !tool_runtime.contains("fn looks_like_verification_command(command: &str) -> bool {\n    let lower = command.to_ascii_lowercase();\n    ["),
        "tool lifecycle must project executed shell verification metadata from language_evidence adapter command facts, not a local runner allowlist"
    );
    let verification = fs::read_to_string(manifest_dir.join("src/agent/verification.rs"))
        .expect("read verification source");
    let verification_command_body = verification
        .split("pub(crate) fn looks_like_verification_command")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn looks_like_verification_failure")
                .next()
        })
        .expect("looks_like_verification_command body");
    assert!(
        verification_command_body.contains("language_verification_command_evidence(&command)")
            && verification_command_body.contains("language_verification_command_evidence(&title)")
            && !verification_command_body.contains("python -m unittest")
            && !verification_command_body.contains("python -m py_compile")
            && !verification_command_body.contains("cargo test")
            && !verification_command_body.contains("cargo check")
            && !verification_command_body.contains("pytest"),
        "verification command classification must delegate runner grammar to language_evidence adapter facts"
    );
    assert!(
        edit_recovery.contains("source_artifact_target_requires_executable_shape(path)")
            && !edit_recovery.contains("path.ends_with(\".py\")")
            && !edit_recovery.contains("escaped whole-file Python source candidate")
            && !edit_recovery.contains("\"python_source_executable_content_shape\".to_string()"),
        "edit recovery escaped-source normalization must use generic artifact content-shape authority, not a Python suffix or Python-only repair ref"
    );
    assert!(
        completion_guard.contains("generic_scaffold_completion_guard_fixture_passes")
            && completion_guard.contains("default_scaffold_runtime_artifacts")
            && completion_guard.contains("classify_language_artifact_target")
            && !completion_guard.contains("rust_workspace_blocked_reason")
            && !completion_guard.contains("candidate_rust_project_roots")
            && !completion_guard.contains("CODE_EXTENSIONS"),
        "completion guard must treat setup-only and default scaffold completion as generic artifact lifecycle evidence, not a Rust/Cargo-only branch or local extension allowlist"
    );
    assert!(
        shell_tool.contains("language_command_text_io_surface_evidence")
            && shell_tool.contains("language_command_test_or_verification_io_evidence")
            && shell_tool.contains("language_runtime_execution_io_evidence")
            && shell_tool.contains("language_command_inherits_utf8_bootstrap")
            && !shell_tool.contains("\"node\"\n                | \"npm\"")
            && !shell_tool.contains("\"test\" | \"tests\" | \"pytest\" | \"unittest\""),
        "shell command text-encoding review must consume language_evidence adapter command surface facts instead of keeping a local runner/test token registry"
    );
    let repair_lane = fs::read_to_string(manifest_dir.join("src/agent/repair_lane.rs"))
        .expect("read repair lane source");
    assert!(
        !repair_lane.contains("struct GeneratedTestLoggingContractOverreach")
            && !repair_lane.contains("fn generated_test_logging_contract_overreach")
            && !repair_lane.contains("fn generated_test_public_output_contract_overreach")
            && !repair_lane.contains("fn generated_test_contract_drift_markers_from_summary")
            && !repair_lane.contains("fn generated_test_reflection_api_misuse(")
            && !repair_lane.contains("fn generated_test_module_attribute_api_misuse(")
            && !repair_lane.contains("fn generated_test_non_source_module_receiver(")
            && !repair_lane.contains("fn public_output_stream_assertion_mismatch(")
            && !repair_lane.contains("fn public_output_assert_equal_stream_and_expected")
            && !repair_lane.contains("fn public_state_assertions(")
            && !repair_lane.contains("fn public_state_assertion_observations(")
            && !repair_lane.contains("fn public_state_terminal_transition_obligations(")
            && !repair_lane.contains("fn public_missing_attributes(")
            && !repair_lane.contains("fn public_missing_method_attributes(")
            && !repair_lane.contains("fn public_class_or_enum_missing_member_details(")
            && !repair_lane.contains("fn public_constructor_signature_mismatch(")
            && !repair_lane.contains("fn public_callable_signature_mismatch(")
            && !repair_lane.contains("fn public_constructor_body_exception(")
            && !repair_lane.contains("fn public_exception_mismatch(")
            && !repair_lane.contains("fn public_expected_exception_not_raised(")
            && !repair_lane.contains("fn public_api_data_model_semantic_obligations(")
            && !repair_lane.contains("fn has_enum_primitive_value_assertion(")
            && !repair_lane.contains("fn generated_test_frame")
            && !repair_lane.contains("fn source_module_frame")
            && !repair_lane.contains("fn local_source_frame_candidate(")
            && !repair_lane.contains("fn constructor_init_frame_candidate(")
            && !repair_lane.contains("fn quoted_file_frame_path("),
        "repair lane must not own generated-test language parser branches; language_evidence adapter must emit typed language evidence for repair lane classification"
    );
    assert!(
        !run_service.contains("resuming_interrupted_session")
            && !run_service.contains("session_was_running")
            && !run_service.contains("Previous run was interrupted.")
            && !run_service.contains(
                "RunEvent::SessionFailed {\n                session_id: session_context.session.id"
            ),
        "stale running cleanup must remain the previous turn terminal event and must not be re-emitted into the resumed turn protocol stream"
    );
    assert!(
        !session_repository.contains("async fn set_status(")
            && !session_repository.contains("async fn append_message(")
            && !session_repository.contains("async fn append_part(")
            && !session_repository.contains("async fn transcript(")
            && !session_repository.contains("async fn update_state(")
            && !session_repository.contains("async fn update_session_title(")
            && !session_service.contains("pub async fn store_user_turn")
            && !session_service.contains("pub async fn store_user_thread_op(")
            && !session_service.contains("pub async fn persist_state")
            && !session_service.contains("pub async fn transcript("),
        "active session service/repository must not expose status-only, message-only, transcript fallback, or state-only write APIs as second protocol authorities"
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

#[test]
fn contract_reconciliation_fixture_cluster_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().expect("moyAI has repository parent");
    let reconciliation_path = manifest_dir
        .join("src")
        .join("agent")
        .join("contract_reconciliation.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let reconciliation = fs::read_to_string(reconciliation_path.as_std_path())
        .expect("read contract reconciliation source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = reconciliation
        .split(
            "pub(crate) fn contract_reconciliation_ignores_diagnostic_label_targets_fixture_passes",
        )
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn reconcile_failure_with_profile_and_typed_evidence")
                .next()
        })
        .expect("contract reconciliation owner fixture block");

    for stale_surface in [
        "arcade_game.py",
        "widget.py",
        "test_widget.py",
        "component.py",
        "test_component.py",
        "python -X utf8 -m unittest",
        "Popen",
        "capture_output",
        "inspect.getsource",
        "envlib",
        "self.assert",
        "ValueError",
        "ZeroDivisionError",
        "widget.",
        "component.",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "contract reconciliation fixture block must not use Python/widget/component/unittest surface `{stale_surface}` as generic owner authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.spec.ts",
        "verify-contract --behavior",
        "verify-generated-test --contract",
        "verify-generated-test --api",
        "verify-generated-test --parse",
        "workflow-source-contract",
        "workflow-generated-test-contract",
        "scenario_contract.workflow.v1",
        "source_public_behavior_assertion",
        "generated_test_artifact_api_misuse",
        "generated_test_artifact_parse_defect",
        "generated_test_contract_overreach",
        "generated_test_local_binding_contradiction",
        "ContractFailureOwner::SourceViolatesContract",
        "ContractFailureOwner::TestViolatesContract",
        "ContractFailureOwner::SourceTestContractMismatch",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "contract reconciliation fixture block must contain language-neutral contract surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs.contains("contract_reconciliation_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral contract reconciliation owner fixture invariant"
    );
}

#[test]
fn tool_truncation_feedback_uses_registered_tool_surface() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let truncate_path = manifest_dir.join("src").join("tool").join("truncate.rs");
    let registry_path = manifest_dir.join("src").join("tool").join("registry.rs");
    let contract_path = manifest_dir.join("src").join("tool").join("contract.rs");
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");

    let truncate = fs::read_to_string(truncate_path.as_std_path()).expect("read truncate source");
    let registry = fs::read_to_string(registry_path.as_std_path()).expect("read tool registry");
    let contract = fs::read_to_string(contract_path.as_std_path()).expect("read tool contract");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");

    assert!(
        registry.contains("tools.insert(\"grep\"")
            && registry.contains("tools.insert(\"read\"")
            && contract.contains("ToolName::Grep")
            && contract.contains("ToolName::Read")
            && !registry.contains("tools.insert(\"search\"")
            && !contract.contains("ToolName::Search"),
        "tool registry must expose the actual typed content-search/read surfaces and no unavailable `search` alias"
    );
    assert!(
        truncate.contains("truncated_tool_output_feedback_uses_typed_tool_surface_fixture_passes"),
        "truncate module must expose a deterministic fixture for registered truncation follow-up surfaces"
    );
    assert!(
        !truncate.contains("`search`"),
        "truncated ToolOutput follow-up guidance must not recommend unavailable `search` tool surface"
    );
    assert!(
        truncate.contains("`grep`") && truncate.contains("`read`"),
        "truncated ToolOutput follow-up guidance must name registered `read` and `grep` surfaces when recommending follow-up inspection"
    );
    assert!(
        preflight.contains("truncated_tool_output_feedback_registered_tool_surface")
            && preflight_docs.contains("truncated_tool_output_feedback_registered_tool_surface"),
        "active preflight and PreflightGateSuite must report the registered truncation feedback surface marker"
    );
}

#[test]
fn language_evidence_source_coordinate_fixture_is_domain_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let language_evidence_path = manifest_dir
        .join("src")
        .join("agent")
        .join("language_evidence.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let language_evidence = fs::read_to_string(language_evidence_path.as_std_path())
        .expect("read language evidence source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = language_evidence
        .split("pub(crate) fn language_source_targets_from_text_handles_line_column_call_site_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn language_path_candidate_tokens").next())
        .expect("language source-coordinate fixture block");

    for stale_surface in ["renderWidget", "src/widget.ts"] {
        assert!(
            !fixture_block.contains(stale_surface),
            "language source-coordinate fixture must not use widget-domain surface `{stale_surface}` as generic path:line:column authority"
        );
    }
    for required_surface in ["renderOperation", "src/workflow.ts"] {
        assert!(
            fixture_block.contains(required_surface),
            "language source-coordinate fixture must contain domain-neutral source-coordinate surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs.contains("language_source_coordinate_fixture_domain_neutral"),
        "PreflightGateSuite must document the domain-neutral language source-coordinate fixture invariant"
    );
}

#[test]
fn lifecycle_kernel_fixture_cluster_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let lifecycle_kernel_path = manifest_dir
        .join("src")
        .join("agent")
        .join("lifecycle_kernel.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let lifecycle_kernel = fs::read_to_string(lifecycle_kernel_path.as_std_path())
        .expect("read lifecycle kernel source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = lifecycle_kernel
        .split("pub(crate) fn turn_lifecycle_plan_owns_dispatch_tool_choice_fixture_passes")
        .nth(1)
        .expect("lifecycle-kernel owner fixture block");

    for stale_surface in [
        "component.py",
        "test_component.py",
        "calculator.py",
        "docs/component-design.md",
        "python -m unittest",
        "other.py",
        "repair component.py",
        "write:component.py",
        "repair:component",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "lifecycle-kernel fixture block must not use component/calculator/Python/unittest surface `{stale_surface}` as generic lifecycle authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.spec.ts",
        "docs/workflow-design.md",
        "verify-contract --behavior",
        "workflow-source-contract",
        "write:src/workflow.rs",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "lifecycle-kernel fixture block must contain language-neutral lifecycle surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs.contains("lifecycle_kernel_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral lifecycle-kernel fixture invariant"
    );
}

#[test]
fn loop_impl_operation_feedback_and_image_replay_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let loop_impl_path = manifest_dir.join("src").join("agent").join("loop_impl.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl =
        fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split("pub(crate) fn operation_feedback_uses_active_work_targets_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn sandbox_profile_for_access_mode").next())
        .expect("loop impl operation-feedback/image-replay fixture block");

    for stale_surface in [
        "test_widget.py",
        "widget.py",
        "test_widget_public_output",
        "test_module.py",
        "python -m unittest",
        "space_invader.py",
        "test_space_invader.py",
        "test_bullet_collision",
        "bullet.active",
        "BEH-4",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "loop_impl operation-feedback/image-replay fixture block must not use widget/Python/game surface `{stale_surface}` as generic runtime authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.behavior.md",
        "verify-contract --behavior",
        "workflow-verification-contract",
        "workflow_state.ready",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "loop_impl operation-feedback/image-replay fixture block must contain language-neutral runtime surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs
            .contains("loop_impl_operation_feedback_image_replay_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral loop_impl operation-feedback/image-replay fixture invariant"
    );
}

#[test]
fn loop_impl_content_shape_open_obligation_recovery_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let loop_impl_path = manifest_dir.join("src").join("agent").join("loop_impl.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl =
        fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split("pub(crate) fn required_write_target_mismatch_feedback_projects_test_content_authority")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn verification_repair_target_grounding_surface_keeps_read_fixture_passes")
                .next()
        })
        .expect("loop impl content-shape/open-obligation/recovery fixture block");

    for stale_surface in [
        "test_component.py",
        "component.py",
        "test_widget.py",
        "widget.py",
        "docs/component-design.md",
        "python -m unittest",
        "unittest",
        "TestWidget",
        "test_arcade_game.py",
        "arcade_game.py",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "loop_impl content-shape/open-obligation/recovery fixture block must not use Python/widget/component/game surface `{stale_surface}` as generic runtime authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.behavior.md",
        "tests/workflow.spec.ts",
        "docs/workflow-design.md",
        "verify-contract --behavior",
        "workflow-source-contract",
        "workflow-generated-test-contract",
        "workflow_state.ready",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "loop_impl content-shape/open-obligation/recovery fixture block must contain language-neutral runtime surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs
            .contains("loop_impl_content_shape_open_obligation_recovery_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral loop_impl content-shape/open-obligation/recovery fixture invariant"
    );
}

#[test]
fn loop_impl_closeout_timeout_does_not_synthesize_final_assistant_message() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let loop_impl_path = manifest_dir.join("src").join("agent").join("loop_impl.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl =
        fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let timeout_branch = loop_impl
        .split("Err(error) => {")
        .nth(1)
        .and_then(|tail| tail.split("return Err(AgentError::Llm(error));").next())
        .unwrap_or_default();

    assert!(
        !loop_impl.contains("fn closeout_timeout_fallback_text")
            && !loop_impl.contains("closeout_timeout_fallback_text()")
            && !loop_impl.contains("\"完了しました。\""),
        "loop_impl must not keep a fixed synthetic closeout final-answer fallback"
    );
    assert!(
        !timeout_branch.contains("PartKind::Text")
            && !timeout_branch.contains("MessagePart::Text")
            && !timeout_branch.contains("RunEvent::TextDelta")
            && !timeout_branch.contains("complete_turn(")
            && !timeout_branch.contains("FinishReason::Stop"),
        "provider timeout handling must preserve provider-boundary failure evidence instead of appending final assistant text and completing the session"
    );
    assert!(
        loop_impl.contains(
            "closeout_timeout_does_not_synthesize_final_assistant_message_fixture_passes"
        ),
        "loop_impl must expose an executable fixture for the closeout timeout final-assistant lifecycle invariant"
    );
    assert!(
        preflight_docs.contains("closeout_timeout_does_not_synthesize_final_assistant_message"),
        "PreflightGateSuite must document the closeout timeout final-assistant lifecycle invariant"
    );
}

#[test]
fn loop_impl_answer_only_final_message_fixture_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let loop_impl_path = manifest_dir.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl =
        fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl source");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split("pub(crate) fn answer_only_final_message_lifecycle_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn closeout_ready_final_response_timeout_guard_fixture_passes")
                .next()
        })
        .expect("answer-only final message lifecycle fixture block");

    assert!(
        !fixture_block.contains("hello.py"),
        "answer-only final-message lifecycle fixture must not use Python-shaped target authority"
    );
    assert!(
        fixture_block.contains("src/workflow.rs"),
        "answer-only final-message lifecycle fixture must use workflow-neutral source target evidence"
    );
    for required_surface in [
        "answer_only_final_message_lifecycle_fixture_language_neutral",
        "answer_only_final_message_lifecycle_fixture_language_neutral_fixture_passes",
    ] {
        assert!(
            loop_impl.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "answer-only final-message lifecycle language-neutral fixture authority, active preflight, or docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_repair_shell_and_exact_repair_write_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let loop_impl_path = manifest_dir.join("src").join("agent").join("loop_impl.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl =
        fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split(
            "pub(crate) fn repair_active_shell_probe_uses_repair_target_authority_fixture_passes",
        )
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn docs_route_rejects_completed_deliverable_regression_fixture_passes",
            )
            .next()
        })
        .expect("loop impl repair-shell / exact-repair-write fixture block");

    for stale_surface in [
        "self.assertIn",
        "AttributeError",
        "import workflow_contract",
        "def compute",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "loop_impl repair-shell / exact-repair-write fixture block must not use Python/unittest surface `{stale_surface}` as generic runtime authority"
        );
    }
    for required_surface in [
        "src/workflow.ts",
        "tests/workflow.spec.ts",
        "verify-contract --behavior",
        "workflow-source-contract",
        "workflow-public-output-contract",
        "workflow_process",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "loop_impl repair-shell / exact-repair-write fixture block must contain language-neutral runtime surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs
            .contains("loop_impl_repair_shell_exact_repair_write_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral loop_impl repair-shell / exact-repair-write fixture invariant"
    );
}

#[test]
fn loop_impl_invalid_edit_and_failed_edit_recovery_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let loop_impl_path = manifest_dir.join("src").join("agent").join("loop_impl.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl =
        fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split("pub(crate) fn invalid_edit_arguments_recovery_is_system_control_projection_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn open_obligation_final_message_recovery_persists_across_no_progress_tool_fixture_passes")
                .next()
        })
        .expect("loop impl invalid-edit / failed-edit recovery fixture block");

    for stale_surface in [
        "\"\"\"Workflow.",
        "def build",
        "top-level `def`",
        "def calculate",
        "import workflow_contract",
        "class WorkflowContract",
        "self.assertEqual",
        "def value",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "loop_impl invalid-edit / failed-edit recovery fixture block must not use Python/unittest surface `{stale_surface}` as generic runtime authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.behavior.md",
        "verify-contract --behavior",
        "workflow-strict-patch-grammar",
        "workflow-invalid-edit-contract",
        "workflow-generated-test-contract",
        "workflow_compute",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "loop_impl invalid-edit / failed-edit recovery fixture block must contain language-neutral runtime surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs
            .contains("loop_impl_invalid_edit_failed_edit_recovery_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral loop_impl invalid-edit / failed-edit recovery fixture invariant"
    );
}

#[test]
fn loop_impl_provider_replay_supporting_context_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let loop_impl_path = manifest_dir.join("src").join("agent").join("loop_impl.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl =
        fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split("pub(crate) fn provider_replay_effective_tool_surface_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn provider_replay_omits_intermediate_assistant_text_fixture_passes",
            )
            .next()
        })
        .expect("loop impl provider replay supporting-context fixture block");

    for stale_surface in ["def add", "class Workflow", "def render"] {
        assert!(
            !fixture_block.contains(stale_surface),
            "loop_impl provider replay supporting-context fixture block must not use Python syntax surface `{stale_surface}` as generic replay authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.spec.ts",
        "docs/workflow-design.md",
        "workflow-provider-replay-supporting-context",
        "workflow_source_contract",
        "workflow_state.ready",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "loop_impl provider replay supporting-context fixture block must contain language-neutral replay surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs
            .contains("loop_impl_provider_replay_supporting_context_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral loop_impl provider replay supporting-context fixture invariant"
    );
}

#[test]
fn tool_orchestrator_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let tool_orchestrator_path = manifest_dir
        .join("src")
        .join("agent")
        .join("tool_orchestrator.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let tool_orchestrator = fs::read_to_string(tool_orchestrator_path.as_std_path())
        .expect("read tool orchestrator source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = tool_orchestrator
        .split("pub(crate) fn open_authoring_operation_intent_classification_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("struct LifecycleConfirmationPrompt").next())
        .expect("tool_orchestrator operation feedback / corrective fixture block");
    let verification_fixture_block = tool_orchestrator
        .split("pub(crate) fn synthetic_corrective_shell_feedback_is_not_verification_run_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn verification_artifact_refs").next())
        .expect("tool_orchestrator verification / repair supporting-context fixture block");
    let combined = format!("{fixture_block}\n{verification_fixture_block}");

    for stale_surface in [
        "test_source.py",
        "docs/component-design.md",
        "test_component.py",
        "component.py",
        "python -m unittest",
        "def render",
        "space_invader.py",
        "widget.test.ts",
        "arcade_game.py",
        "test_widget.py",
        "widget.py",
        "self.assertIn",
        "python -m py_compile widget.py",
        "src/widget.rs",
        "tests/widget_contract.rs",
        "build_widget",
    ] {
        assert!(
            !combined.contains(stale_surface),
            "tool_orchestrator fixture block must not use Python/component/widget/game surface `{stale_surface}` as generic tool lifecycle authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.spec.ts",
        "docs/workflow-design.md",
        "verify-contract --behavior",
        "verify-generated-test --contract",
        "workflow-tool-lifecycle-contract",
        "workflow-source-contract",
        "workflow-generated-test-contract",
        "workflow_state.ready",
    ] {
        assert!(
            combined.contains(required_surface),
            "tool_orchestrator fixture block must contain language-neutral tool lifecycle surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs.contains("tool_orchestrator_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral tool_orchestrator fixture invariant"
    );
}

#[test]
fn turn_decision_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let turn_decision_path = manifest_dir
        .join("src")
        .join("agent")
        .join("turn_decision.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let turn_decision =
        fs::read_to_string(turn_decision_path.as_std_path()).expect("read turn decision source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = turn_decision
        .split(
            "pub(crate) fn active_work_edit_authority_precedes_verification_rerun_fixture_passes",
        )
        .nth(1)
        .and_then(|tail| tail.split("fn route_label").next())
        .expect("turn_decision active-work / repair / verification fixture block");

    for stale_surface in [
        "component.py",
        "test_component.py",
        "component.calculate",
        "test_calculate_add",
        "python -m unittest",
        "test_widget.py",
        "test_invalid_visible_contract",
        "Repair component.py before rerun",
        "python -X utf8 -m unittest",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "turn_decision fixture block must not use Python/component/widget/unittest surface `{stale_surface}` as generic turn-decision authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.spec.ts",
        "workflow.active_work_contract",
        "workflow-repair-target-contract",
        "workflow-verification-rerun-contract",
        "verify-contract --behavior",
        "verify-generated-test --contract",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "turn_decision fixture block must contain language-neutral turn-decision surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs.contains("turn_decision_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral turn_decision fixture invariant"
    );
}

#[test]
fn apply_patch_guidance_and_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let apply_patch_path = manifest_dir.join("src").join("tool").join("apply_patch.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let apply_patch =
        fs::read_to_string(apply_patch_path.as_std_path()).expect("read apply_patch source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = apply_patch
        .split("impl Tool for ApplyPatchTool")
        .nth(1)
        .expect("apply_patch tool guidance / fixture block");

    for stale_surface in [
        "top-level def/class/import",
        "def example",
        "docs/component-design.md",
        "Component Design",
        "generated component",
        "docs/component-design-v2.md",
        "workspace/component.py",
        "workspace/test_component.py",
        "source.py",
        "\"\"\"module\"\"\"",
        "def build",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "apply_patch guidance / fixture block must not use Python/component surface `{stale_surface}` as generic patch authority"
        );
    }
    for required_surface in [
        "top-level code or declaration lines",
        "declare workflow_record",
        "docs/workflow-design.md",
        "docs/workflow-design-v2.md",
        "workspace/src/workflow.rs",
        "workspace/tests/workflow.spec.ts",
        "source.workflow",
        "workflow_step:",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "apply_patch guidance / fixture block must contain language-neutral patch surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs.contains("apply_patch_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral apply_patch fixture invariant"
    );
}

#[test]
fn search_glob_fixture_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let search_path = manifest_dir.join("src").join("tool").join("search.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let search = fs::read_to_string(search_path.as_std_path()).expect("read search tool source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = search
        .split("pub(crate) fn glob_workspace_relative_pattern_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("impl Tool for GrepTool").next())
        .expect("search glob workspace-relative fixture block");

    for stale_surface in ["calculator.py", "C:/workspace/project/calculator.py"] {
        assert!(
            !fixture_block.contains(stale_surface),
            "search glob fixture block must not use Python/calculator surface `{stale_surface}` as generic workspace-relative search authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "workflow.glob.contract",
        "model_visible_relative_output",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "search glob fixture block must contain language-neutral search surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs.contains("search_glob_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral search glob fixture invariant"
    );
}

#[test]
fn shell_syntax_correction_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let shell_path = manifest_dir.join("src").join("tool").join("shell.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let shell = fs::read_to_string(shell_path.as_std_path()).expect("read shell tool source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let correction_block = shell
        .split("fn shell_contract_violation(command:")
        .nth(1)
        .and_then(|tail| tail.split("fn shell_syntax_violation").next())
        .expect("shell syntax correction block");

    for stale_surface in ["python -m unittest"] {
        assert!(
            !correction_block.contains(stale_surface),
            "shell syntax correction block must not use Python/unittest surface `{stale_surface}` as generic shell correction authority"
        );
    }
    for required_surface in [
        "raw command directly without stderr redirection",
        "native shell syntax",
    ] {
        assert!(
            correction_block.contains(required_surface),
            "shell syntax correction block must contain language-neutral correction surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs.contains("shell_syntax_correction_language_neutral"),
        "PreflightGateSuite must document the language-neutral shell syntax correction invariant"
    );
}

#[test]
fn shell_command_text_encoding_fixture_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let shell_path = manifest_dir.join("src").join("tool").join("shell.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let shell = fs::read_to_string(shell_path.as_std_path()).expect("read shell tool source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = shell
        .split("pub fn command_text_encoding_contract_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub fn external_connection_shell_review_fixture_passes")
                .next()
        })
        .expect("shell command text encoding fixture block");

    for stale_surface in [
        "python -m unittest",
        "python -X utf8 -m unittest",
        "calculator.py",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "shell command text encoding fixture block must not use unittest/calculator surface `{stale_surface}` as generic encoding authority"
        );
    }
    for required_surface in [
        "python -m workflow_check",
        "python -X utf8 -m workflow_check",
        "Get-Content src/workflow.rs -Encoding UTF8",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "shell command text encoding fixture block must contain workflow-neutral command/artifact surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs.contains("shell_command_text_encoding_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral shell command text encoding fixture invariant"
    );
}

#[test]
fn preflight_shell_output_encoding_fixture_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = repo_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = repo_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = repo_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");

    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let related_blocks = preflight
        .split("fixture_id: \"fixture.tool_lifecycle.shell_output_encoding_authority\"")
        .skip(1)
        .collect::<Vec<_>>();
    assert_eq!(
        related_blocks.len(),
        2,
        "shell output encoding must have one gate definition and one fixture metadata definition"
    );

    for stale_surface in ["PYTHONUTF8", "PYTHONIOENCODING"] {
        for related_block in &related_blocks {
            let block = related_block
                .split("PreflightGate {")
                .next()
                .unwrap_or(related_block);
            assert!(
                !block.contains(stale_surface),
                "shell output encoding preflight metadata must not require Python-specific env-var authority `{stale_surface}`"
            );
        }
    }
    for required_surface in [
        "shell_output_text_encoding_contract",
        "shell_output_decode_strategy",
        "language_text_io_command_surface_adapter",
    ] {
        assert!(
            preflight.contains(required_surface),
            "shell output encoding preflight metadata must contain language-neutral surface `{required_surface}`"
        );
    }
    for docs_surface in [
        &preflight_docs,
        &runtime_contracts,
        &detailed_design,
        &item_lifecycle,
    ] {
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                docs_surface,
                "shell_output_text_encoding_contract",
            ),
            "docs/design surfaces must describe language-neutral shell output text encoding authority"
        );
    }
}

#[test]
fn shell_contract_violation_projects_typed_no_progress_feedback() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let shell_path = manifest_dir.join("src").join("tool").join("shell.rs");
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let shell = fs::read_to_string(shell_path.as_std_path()).expect("read shell tool source");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");

    assert!(
        shell.contains("fn shell_contract_violation_result"),
        "shell contract violations must be rendered through a dedicated typed no-progress ToolResult helper"
    );
    let helper_block = shell
        .split("fn shell_contract_violation_result")
        .nth(1)
        .and_then(|tail| tail.split("struct ShellContractViolation").next())
        .expect("shell contract violation result helper block");
    for required_surface in [
        "\"success\": false",
        "\"progress_effect\": \"no_progress\"",
        "\"side_effects_applied\": false",
        "\"submitted_command\"",
        "\"contract_violation\"",
        "\"command_text_encoding_review\"",
        "\"result_hash\"",
        "\"tool_feedback_envelope\"",
    ] {
        assert!(
            helper_block.contains(required_surface),
            "shell contract violation helper must project typed no-progress feedback surface `{required_surface}`"
        );
    }
    assert!(
        preflight.contains("shell_contract_violation_typed_no_progress_feedback")
            || preflight_docs.contains("shell_contract_violation_typed_no_progress_feedback"),
        "active preflight must carry shell contract violation typed no-progress feedback marker"
    );
}

#[test]
fn todowrite_progress_projection_fixture_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let todo_write_path = manifest_dir.join("src").join("tool").join("todo_write.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let todo_write =
        fs::read_to_string(todo_write_path.as_std_path()).expect("read todowrite source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = todo_write
        .split("pub(crate) fn progress_projection_payload_drops_authority_fields")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn normalize_todo_write_arguments")
                .next()
        })
        .expect("todowrite progress projection fixture block");

    for stale_surface in ["component.py", "python -m unittest"] {
        assert!(
            !fixture_block.contains(stale_surface),
            "todowrite progress projection fixture block must not use component/unittest surface `{stale_surface}` as generic progress authority"
        );
    }
    for required_surface in [
        "update workflow projection",
        "src/workflow.rs",
        "verify-contract --behavior",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "todowrite progress projection fixture block must contain workflow-neutral progress surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs.contains("todowrite_progress_projection_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral TodoWrite progress projection fixture invariant"
    );
}

#[test]
fn truncated_tool_output_feedback_uses_typed_tool_surface() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let truncate_path = manifest_dir.join("src").join("tool").join("truncate.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let truncate = fs::read_to_string(truncate_path.as_std_path()).expect("read truncate source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let guidance_block = truncate
        .split("fn truncation_followup_guidance")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn truncated_tool_output_feedback_uses_typed_tool_surface_fixture_passes")
                .next()
        })
        .expect("truncation follow-up guidance block");

    assert!(
        !guidance_block.contains("`search`"),
        "truncated tool output feedback must not name unavailable `search` as provider-visible next-action authority"
    );
    for required_surface in [
        "`read`",
        "`offset`",
        "`limit`",
        "registered `grep`",
        "`path`",
    ] {
        assert!(
            guidance_block.contains(required_surface),
            "truncated tool output feedback must contain registered tool-surface guidance `{required_surface}`"
        );
    }
    assert!(
        preflight_docs.contains("truncated_tool_output_feedback_registered_tool_surface"),
        "PreflightGateSuite must document the registered truncation feedback surface invariant"
    );
}

#[test]
fn write_no_content_fixture_is_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let write_path = manifest_dir.join("src").join("tool").join("write.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let write = fs::read_to_string(write_path.as_std_path()).expect("read write source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = write
        .split("pub(crate) fn no_content_write_result_projects_typed_no_progress_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn write_execution_uses_atomic_filechange_commit_fixture_passes")
                .next()
        })
        .expect("write no-content fixture block");

    assert!(
        !fixture_block.contains("docs/component-design.md"),
        "write no-content fixture block must not use component-design target coordinate as generic no-progress authority"
    );
    assert!(
        fixture_block.contains("docs/workflow-design.md"),
        "write no-content fixture block must use workflow-neutral docs artifact coordinate"
    );
    assert!(
        preflight_docs.contains("write_no_content_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral write no-content fixture invariant"
    );
}

#[test]
fn repair_lane_public_command_generated_overreach_fixtures_are_language_neutral() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().expect("moyAI has repository parent");
    let repair_lane_path = manifest_dir
        .join("src")
        .join("agent")
        .join("repair_lane.rs");
    let preflight_docs_path = repo_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let repair_lane =
        fs::read_to_string(repair_lane_path.as_std_path()).expect("read repair lane source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = repair_lane
        .split("pub(crate) fn public_command_contract_failure_projects_compact_source_repair_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn repair_lane_subtype(").next())
        .expect("public-command/generated-overreach repair lane fixture block");

    for stale_surface in [
        "tool.py",
        "test_tool.py",
        "widget.py",
        "test_widget.py",
        "python -m unittest",
        "python -X utf8 -m unittest",
        "python -X utf8 tool.py",
        "Traceback",
        "self.assert",
        "assertLogs",
        "Widget CLI",
        "ValueError",
        "ZeroDivisionError",
        "widget.",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "public-command/generated-overreach repair lane fixture block must not use Python/widget/tool/unittest surface `{stale_surface}` as generic repair authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.spec.ts",
        "verify-public-command --argv",
        "verify-generated-test --contract",
        "verify-generated-test --output",
        "verify-generated-test --exception",
        "workflow-public-command-contract",
        "workflow-generated-test-contract",
        "contract_reconciliation",
        "repair_control_snapshot",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "public-command/generated-overreach repair lane fixture block must contain language-neutral contract surface `{required_surface}`"
        );
    }
    assert!(
        preflight_docs
            .contains("repair_lane_public_command_generated_overreach_fixture_language_neutral"),
        "PreflightGateSuite must document the language-neutral public-command/generated-overreach repair lane fixture invariant"
    );
}

#[test]
fn protocol_control_fixtures_are_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let control_path = repo_root.join("src").join("protocol").join("control.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let control = fs::read_to_string(control_path.as_std_path()).expect("read protocol control");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = control
        .split(
            "pub fn active_apply_patch_target_projection_renders_operation_template_fixture_passes",
        )
        .nth(1)
        .and_then(|tail| tail.split("fn validate_model_capabilities(").next())
        .expect("protocol control fixture block");

    for stale_surface in [
        "test_component.py",
        "test_widget.py",
        "component.py",
        "old.py",
        "docs/old.md",
        "docs/component-design.md",
        "calculator.py",
        "src/widget.ts",
        "npm test",
        "python -m unittest",
        "python -X utf8 -m unittest",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "protocol control fixture block must not use Python/component/calculator surface `{stale_surface}` as generic control-plane authority"
        );
    }
    for required_surface in [
        "tests/workflow.behavior.md",
        "src/workflow.rs",
        "docs/workflow-design.md",
        "stale/workflow.rs",
        "verify-contract --behavior",
        "protocol_control_fixture_language_neutral",
    ] {
        assert!(
            fixture_block.contains(required_surface) || preflight_docs.contains(required_surface),
            "protocol control fixture block or preflight docs must contain language-neutral control surface `{required_surface}`"
        );
    }
}

#[test]
fn prompt_assets_fixtures_are_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_assets_path = repo_root.join("src").join("agent").join("prompt_assets.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let prompt_assets =
        fs::read_to_string(prompt_assets_path.as_std_path()).expect("read prompt assets source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let inactive_recovery_block = prompt_assets
        .split("pub(crate) fn inactive_target_edit_recovery_reminder_uses_current_edit_surface_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn target_is_documentation_like").next())
        .expect("inactive-target edit recovery prompt fixture block");
    let verification_repair_block = prompt_assets
        .split("pub(crate) fn verification_repair_prompt_uses_language_projection_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn verification_failure_detail_line").next())
        .expect("verification repair prompt fixture block");
    let fixture_block = format!("{inactive_recovery_block}\n{verification_repair_block}");

    for stale_surface in [
        "component.py",
        "test_component.py",
        "src/widget.test.ts",
        "test_widget.py",
        "test_widget",
        "widget verification",
        "Repair widget",
        "render is not a function",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "prompt-assets fixtures must not use component/widget surface `{stale_surface}` as generic prompt authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.spec.ts",
        "tests/workflow.behavior.md",
        "workflow verification",
        "workflow.advance",
        "workflow_result",
        "prompt_assets_fixture_workflow_neutral",
    ] {
        assert!(
            fixture_block.contains(required_surface) || preflight_docs.contains(required_surface),
            "prompt-assets fixture block or preflight docs must contain workflow-neutral prompt surface `{required_surface}`"
        );
    }
}

#[test]
fn prompt_text_io_guidance_is_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_assets_path = repo_root.join("src").join("agent").join("prompt_assets.rs");
    let prompt_assets =
        fs::read_to_string(prompt_assets_path.as_std_path()).expect("read prompt assets source");

    for stale_surface in [
        "const PYTHON_UTF8_PROMPT",
        "PYTHON_UTF8_PROMPT.to_string()",
        "Python UTF-8 Rules",
        "subprocess.run(...)",
        "sys.stdout",
        "sys.stderr",
        "cp932",
    ] {
        assert!(
            !prompt_assets.contains(stale_surface),
            "global prompt assets must not project Python runtime surface `{stale_surface}` as generic text I/O authority"
        );
    }
    for required_surface in [
        "TEXT_IO_PROMPT",
        "Text I/O and Verification Process Rules",
        "prompt_text_io_guidance_language_neutral",
    ] {
        assert!(
            prompt_assets.contains(required_surface),
            "global prompt assets must expose language-neutral text I/O prompt surface `{required_surface}`"
        );
    }

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "prompt_text_io_guidance_language_neutral",
            ),
            "docs/design surfaces must describe language-neutral prompt text I/O guidance"
        );
    }
}

#[test]
fn prompt_fixtures_are_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_path = repo_root.join("src").join("agent").join("prompt.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let prompt = fs::read_to_string(prompt_path.as_std_path()).expect("read prompt source");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight = preflight.replace("\r\n", "\n");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = prompt
        .split(
            "pub(crate) fn content_shape_mismatch_replay_preserves_tool_lifecycle_without_payload",
        )
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn prompt_fixtures_are_workflow_neutral_fixture_passes")
                .next()
        })
        .expect("prompt provider replay fixture block");

    for stale_surface in [
        "component.py",
        "test_component.py",
        "component-design",
        "Component design",
        "write component.py",
        "write test_component.py",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "prompt fixtures must not use component/test_component/component-design surface `{stale_surface}` as generic provider replay authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.spec.ts",
        "docs/workflow-design.md",
        "workflow.advance",
        "workflow_result",
        "prompt_fixture_workflow_neutral",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "prompt fixtures, active preflight, or preflight docs must contain workflow-neutral prompt surface `{required_surface}`"
        );
    }
}

#[test]
fn prompt_residual_fixtures_are_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_path = repo_root.join("src").join("agent").join("prompt.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let prompt = fs::read_to_string(prompt_path.as_std_path()).expect("read prompt source");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let residual_block = prompt
        .split(
            "pub(crate) fn python_source_content_shape_repair_projection_carries_positive_contract",
        )
        .nth(1)
        .and_then(|tail| tail.split("fn stale_write_tool_result_replay_note").next())
        .expect("prompt residual provider replay fixture block");

    for stale_surface in [
        "component.py",
        "test_component.py",
        "python -m unittest",
        "Manual ST closeout continuation",
        "test_arcade_game.py",
        "arcade_game.py",
        "class Player",
        "create component.py and test_component.py",
    ] {
        assert!(
            !residual_block.contains(stale_surface),
            "prompt residual fixtures must not use stale component/manual-ST/Python game surface `{stale_surface}` as generic prompt lifecycle authority"
        );
    }
    for required_surface in [
        "src/workflow.py",
        "tests/test_workflow.py",
        "src/workflow.rs",
        "tests/workflow.spec.ts",
        "docs/workflow-design.md",
        "verify-workflow --behavior",
        "prompt_residual_fixture_workflow_neutral",
    ] {
        assert!(
            residual_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "prompt residual fixtures, active preflight, or preflight docs must contain workflow-neutral prompt surface `{required_surface}`"
        );
    }
}

#[test]
fn public_command_contract_fixtures_are_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let public_command_path = repo_root
        .join("src")
        .join("agent")
        .join("public_command_contract.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let public_command =
        fs::read_to_string(public_command_path.as_std_path()).expect("read public command source");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = public_command
        .split("pub fn public_command_contract_helper_argv_operator_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn public_command_obligations_from_text").next())
        .expect("public command operator fixture block");

    for stale_surface in ["2 + 3", "3 + 5", "print 5", "\"5\"", "\"8\""] {
        assert!(
            !fixture_block.contains(stale_surface),
            "public command contract fixtures must not use calculator-shaped surface `{stale_surface}` as generic command authority"
        );
    }
    for required_surface in [
        "workflow-tool combine draft + review",
        "workflow-tool inspect draft + review",
        "combined",
        "inspected",
        "public_command_contract_fixture_workflow_neutral",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "public command contract fixtures, active preflight, or preflight docs must contain workflow-neutral command surface `{required_surface}`"
        );
    }
}

#[test]
fn desktop_file_change_rows_preserve_runtime_path_evidence() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let artifact_projection_path = repo_root
        .join("src")
        .join("desktop")
        .join("artifact_projection.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let artifact_projection = fs::read_to_string(artifact_projection_path.as_std_path())
        .expect("read desktop artifact projection source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");

    for stale_owner in [
        "file_change_is_user_visible",
        "is_user_visible_artifact_path",
        "normalized.contains(\"/__pycache__/\")",
        "normalized.starts_with(\"__pycache__/\")",
        "normalized.ends_with(\".pyc\")",
    ] {
        assert!(
            !artifact_projection.contains(stale_owner),
            "Desktop file-change projection must not mask canonical runtime path evidence through `{stale_owner}`"
        );
    }
    assert!(
        preflight_docs.contains("desktop_file_change_runtime_path_evidence_preserved"),
        "PreflightGateSuite must document Desktop file-change runtime path evidence preservation"
    );
}

#[test]
fn desktop_query_projection_fixtures_are_language_neutral_and_preserve_runtime_evidence() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let query_path = repo_root.join("src").join("desktop").join("query.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let query = fs::read_to_string(query_path.as_std_path()).expect("read desktop query source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");

    for stale_surface in [
        "component.py",
        "test_component.py",
        "a.py",
        "b.py",
        "arcade_game.py",
        "arcade_game.pyc",
        "python -m unittest",
        "Running python -m unittest",
        "unit testを実行",
        "component.py を作成",
        "component.pyを作成",
        "make a component",
        "write component.py",
        "wrote component.py",
        "Added component.py",
        "Updated component.py",
        "file_change_rows_hide_runtime_cache_files_from_user_history",
        "artifact_rows_hide_runtime_cache_files",
    ] {
        assert!(
            !query.contains(stale_surface),
            "Desktop query projection fixtures must not retain stale Python/component/cache-hiding authority `{stale_surface}`"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.contract",
        "docs/workflow-notes.md",
        "verify-contract --behavior",
        "build-artifacts/cache/workflow.snapshot",
        "desktop_query_projection_fixture_language_neutral_runtime_evidence_preserved",
    ] {
        assert!(
            query.contains(required_surface) || preflight_docs.contains(required_surface),
            "Desktop query projection fixtures or preflight docs must contain workflow-neutral preservation surface `{required_surface}`"
        );
    }
}

#[test]
fn app_session_title_fixtures_are_domain_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let session_title_path = repo_root.join("src").join("app").join("session_title.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let session_title = fs::read_to_string(session_title_path.as_std_path())
        .expect("read app session title source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");

    for stale_surface in ["電卓テスト作成", "電卓"] {
        assert!(
            !session_title.contains(stale_surface),
            "app session-title fixtures must not use calculator/test-domain title surface `{stale_surface}` as generic navigation authority"
        );
    }
    for required_surface in [
        "ワークフロー整理",
        "app_session_title_fixture_domain_neutral",
    ] {
        assert!(
            session_title.contains(required_surface) || preflight_docs.contains(required_surface),
            "app session-title fixtures or preflight docs must contain domain-neutral title surface `{required_surface}`"
        );
    }
}

#[test]
fn llm_contract_fixtures_are_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let contract_path = repo_root.join("src").join("llm").join("contract.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let contract = fs::read_to_string(contract_path.as_std_path()).expect("read llm contract");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let output_budget_fixture = contract
        .split("pub fn tool_call_turn_uses_configured_output_budget_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub fn chat_request_tool_choice_is_provider_neutral_typed_fixture_passes")
                .next()
        })
        .expect("LLM output-budget fixture block");

    for stale_surface in [
        "Create test_component.py",
        "test_component.py",
        "component.py",
    ] {
        assert!(
            !output_budget_fixture.contains(stale_surface),
            "LLM contract output-budget fixture must not use Python/component prompt surface `{stale_surface}` as generic provider lifecycle authority"
        );
    }
    for required_surface in [
        "Create src/workflow.rs",
        "workflow-output-budget-contract",
        "llm_contract_fixture_language_neutral",
    ] {
        assert!(
            output_budget_fixture.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "LLM contract fixture or active preflight docs must contain workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn llm_model_probe_fixtures_use_closed_network_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let model_probe_path = repo_root.join("src").join("llm").join("model_probe.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let model_probe =
        fs::read_to_string(model_probe_path.as_std_path()).expect("read llm model probe");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = model_probe
        .split("pub(crate) fn model_availability_probe_uses_shared_transport_projection_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn build_probe_client").next())
        .expect("LLM model-probe fixture block");

    for stale_surface in ["qwen/qwen3.6-35b-a3b", "http://127.0.0.1:1234"] {
        assert!(
            !fixture_block.contains(stale_surface),
            "LLM model-probe fixture block must not use stale provider profile surface `{stale_surface}` as generic readiness authority"
        );
    }
    for required_surface in [
        "qwen/qwen3.6-35b-a3b",
        "http://127.0.0.1:1234",
        "OpenAiCompatibleOnly",
        "model_probe_fixture_provider_profile_openai_compatible",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "LLM model-probe fixture or active preflight docs must contain current provider profile surface `{required_surface}`"
        );
    }
}

#[test]
fn desktop_web_model_fixtures_use_current_provider_profile_and_domain_neutral_permission() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let web_model_path = repo_root.join("src").join("desktop").join("web_model.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let web_model =
        fs::read_to_string(web_model_path.as_std_path()).expect("read desktop web model");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = web_model
        .split("mod tests")
        .nth(1)
        .expect("Desktop web-model test fixture block");

    for stale_surface in [
        "http://127.0.0.1:1234",
        "http://127.0.0.1:9",
        "qwen/example",
        "Install pygame library",
        "pip install pygame",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "Desktop web-model fixtures must not use stale provider or domain-specific setup surface `{stale_surface}`"
        );
    }
    for required_surface in [
        "http://127.0.0.1:1234",
        "qwen/qwen3.6-35b-a3b",
        "openai_compatible_only",
        "desktop_web_model_fixture_current_provider_profile_domain_neutral",
    ] {
        assert!(
            web_model.contains(required_surface)
                || fixture_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "Desktop web-model fixture or active preflight docs must expose current provider/domain-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn llm_openai_compat_fixtures_are_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let openai_compat_path = repo_root.join("src").join("llm").join("openai_compat.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let openai_compat =
        fs::read_to_string(openai_compat_path.as_std_path()).expect("read openai compat");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = openai_compat
        .split("pub(crate) fn payload_merges_provider_policy_and_runtime_system_control_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn first_system_prompt").next())
        .expect("OpenAI-compatible provider fixture block");

    for stale_surface in [
        "Create calculator.py",
        "calculator.py",
        "test_calculator.py",
        "Create test_component.py",
        "test_component.py",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "OpenAI-compatible provider fixtures must not use calculator/Python prompt surface `{stale_surface}` as generic request lifecycle authority"
        );
    }
    for required_surface in [
        "Create src/workflow.rs",
        "tests/workflow.contract",
        "openai_compat_fixture_language_neutral",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "OpenAI-compatible provider fixtures or active preflight docs must contain workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn prompt_provider_replay_uses_sequence_primary_order() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_path = repo_root.join("src").join("agent").join("prompt.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let prompt = fs::read_to_string(prompt_path.as_std_path()).expect("read prompt module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let ordering_block = prompt
        .split("fn canonical_history_items_for_projection")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn provider_replay_repair_leading_orphans")
                .next()
        })
        .expect("prompt provider replay canonical ordering block");

    for forbidden in [
        "created_at_ms.saturating_mul(1_000_000) + item.sequence_no",
        "history_item_order_scalar(item), item.sequence_no",
    ] {
        assert!(
            !ordering_block.contains(forbidden),
            "prompt provider replay canonical ordering must not use timestamp-primary pattern `{forbidden}`"
        );
    }
    for required_surface in [
        "item.sequence_no, item.created_at_ms",
        "prompt_provider_replay_sequence_order",
        "provider replay canonical sequence order",
    ] {
        assert!(
            ordering_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface),
            "prompt provider replay sequence-order contract must expose `{required_surface}`"
        );
    }
}

#[test]
fn prompt_projection_fixtures_are_domain_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_path = repo_root.join("src").join("agent").join("prompt.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let prompt = fs::read_to_string(prompt_path.as_std_path()).expect("read prompt module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let fixture_block = prompt
        .split("pub fn vision_input_provider_projection_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn verification_evidence_uses_typed_history_item_authority_fixture_passes")
                .next()
        })
        .expect("prompt provider projection fixture cluster");

    for forbidden in [
        "js-arcade_games01.jpg",
        "C:/diagnostic/source",
        "test_widget.py",
        "\"widget.py\"",
        "src/widget.ts",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "prompt projection fixture cluster must not use domain-specific surface `{forbidden}`"
        );
    }
    for required_surface in [
        "workflow-visual-reference.jpg",
        "src/workflow.ts",
        "prompt_projection_fixture_domain_neutral",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface),
            "prompt projection fixture cluster must expose workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn prompt_verification_repair_fixtures_are_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_path = repo_root.join("src").join("agent").join("prompt.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let prompt = fs::read_to_string(prompt_path.as_std_path()).expect("read prompt module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let fixture_block = prompt
        .split("pub(crate) fn prompt_projection_uses_typed_verification_run_cycle_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn content_parts_text").next())
        .expect("prompt verification/rejection fixture cluster");

    for forbidden in [
        "python -m unittest",
        "source.py",
        "component.py",
        "```python",
        "print('",
        "docs/component-design.md",
        "shell:python tool.py --bad",
        "stale.py",
        "def broken(): pass",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "prompt verification/rejection fixture cluster must not use language- or component-specific authority `{forbidden}`"
        );
    }
    for required_surface in [
        "verify-workflow --behavior repair",
        "src/workflow.rs",
        "docs/workflow-design.md",
        "shell:verify-workflow --docs",
        "prompt_verification_repair_fixture_language_neutral",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface),
            "prompt verification/rejection fixtures or active preflight docs must expose workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn prompt_content_shape_window_fixture_is_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_path = repo_root.join("src").join("agent").join("prompt.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let prompt = fs::read_to_string(prompt_path.as_std_path()).expect("read prompt module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let fixture_block = prompt
        .split("pub(crate) fn content_shape_repair_contract_uses_canonical_history_window_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn provider_replay_compaction_boundary_uses_canonical_history_order_fixture_passes")
                .next()
        })
        .expect("prompt content-shape history-window fixture block");

    assert!(
        !fixture_block.contains("test_component.py"),
        "prompt content-shape history-window fixture must not use component target authority"
    );
    for required_surface in [
        "tests/workflow_contract.py",
        "prompt_content_shape_window_fixture_workflow_neutral",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface),
            "prompt content-shape history-window fixture or active preflight docs must expose workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn prompt_docs_followup_heuristic_is_domain_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_path = repo_root.join("src").join("agent").join("prompt.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let prompt = fs::read_to_string(prompt_path.as_std_path()).expect("read prompt module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let heuristic_block = prompt
        .split("pub(crate) fn documentation_change_may_lead_implementation")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn implementation_follow_up_references_prior_design")
                .next()
        })
        .expect("prompt docs-follow-up heuristic block");

    for stale_surface in [
        "document the current component design",
        "becomes a scientific component",
        "support sin",
        "support cos",
        "support sqrt",
        "support pow",
        "関数電卓版",
    ] {
        assert!(
            !heuristic_block.contains(stale_surface),
            "prompt docs-follow-up heuristic must not use domain-specific parser authority `{stale_surface}`"
        );
    }
    for required_surface in [
        "specification changes",
        "new capability",
        "add support for",
        "prompt_docs_followup_heuristic_domain_neutral",
    ] {
        assert!(
            heuristic_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface),
            "prompt docs-follow-up heuristic or active preflight docs must expose domain-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn prompt_assets_staged_docs_deliverable_projection_is_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_assets_path = repo_root.join("src").join("agent").join("prompt_assets.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let prompt_assets =
        fs::read_to_string(prompt_assets_path.as_std_path()).expect("read prompt assets module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let write_contract_block = prompt_assets
        .split("fn staged_task_documentation_write_contract_example")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn staged_task_closeout_reminder")
                .next()
        })
        .expect("staged task documentation write contract example block");
    let expectation_block = prompt_assets
        .split("fn staged_task_documentation_deliverable_expectation")
        .nth(1)
        .and_then(|tail| tail.split("pub(crate) fn completion_ready_reminder").next())
        .expect("staged task documentation deliverable expectation block");
    let docs_route_fixture_block = prompt_assets
        .split("pub(crate) fn docs_route_reminder_projects_write_ready_boundary_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn staged_docs_deliverable_projection_workflow_neutral_fixture_passes",
            )
            .next()
        })
        .expect("docs-route write-ready boundary fixture block");

    for forbidden in [
        "detail_design.md",
        "readme.md",
        "basic_design.md",
        "README.md",
        "basic_design.md / detail_design.md",
    ] {
        assert!(
            !write_contract_block.contains(forbidden)
                && !expectation_block.contains(forbidden)
                && !docs_route_fixture_block.contains(forbidden),
            "prompt assets staged-doc projection must not use target-name authority `{forbidden}` as a generic provider-visible contract"
        );
    }
    for required_surface in [
        "current documentation deliverable",
        "deliverable role",
        "workflow documentation contract",
        "prompt_assets_staged_docs_deliverable_projection_workflow_neutral",
    ] {
        assert!(
            write_contract_block.contains(required_surface)
                || expectation_block.contains(required_surface)
                || docs_route_fixture_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface),
            "prompt assets staged-doc projection or active preflight docs must expose workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn prompt_assets_documentation_target_classifier_is_shape_based() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_assets_path = repo_root.join("src").join("agent").join("prompt_assets.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let prompt_assets =
        fs::read_to_string(prompt_assets_path.as_std_path()).expect("read prompt assets module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let classifier_block = prompt_assets
        .split("fn target_is_documentation_like")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn superseded_tool_denial_reminder")
                .next()
        })
        .expect("prompt assets documentation target classifier block");

    for forbidden in [
        "\"readme.md\"",
        "\"basic_design.md\"",
        "\"detail_design.md\"",
        "\"detailed_design.md\"",
    ] {
        assert!(
            !classifier_block.contains(forbidden),
            "prompt assets documentation target classifier must not use exact target-name branch `{forbidden}`"
        );
    }
    for required_surface in [
        ".ends_with(\".md\")",
        ".ends_with(\".markdown\")",
        ".contains(\"/docs/\")",
        "prompt_assets_documentation_target_classifier_shape_based",
    ] {
        assert!(
            classifier_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface),
            "prompt assets documentation target classifier or active preflight docs must expose shape-based surface `{required_surface}`"
        );
    }
}

#[test]
fn prompt_assets_python_context_uses_language_evidence_adapter() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_assets_path = repo_root.join("src").join("agent").join("prompt_assets.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let prompt_assets =
        fs::read_to_string(prompt_assets_path.as_std_path()).expect("read prompt assets module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let projection_block = prompt_assets
        .split("fn exception_assertion_guidance_line")
        .nth(1)
        .and_then(|tail| tail.split("fn verification_failure_detail_line").next())
        .expect("prompt assets verification repair Python context block");

    for forbidden in [
        "failure_summary_has_python_context",
        "lower.contains(\".py\")",
        "lower.contains(\"traceback\")",
        "lower.contains(\"unittest\")",
        "lower.contains(\"pytest\")",
        "lower.contains(\"assertraisesregex\")",
        "lower.contains(\"python\")",
    ] {
        assert!(
            !projection_block.contains(forbidden),
            "prompt assets Python context projection must not use raw failure-summary substring authority `{forbidden}`"
        );
    }
    for required_surface in [
        "classify_language_artifact_target",
        "VerificationContractPromptProjection",
        "has_python_exception_assertion",
        "LanguageFamily::Python",
        "prompt_assets_python_context_uses_language_evidence_adapter",
    ] {
        assert!(
            projection_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface),
            "prompt assets Python context projection or active preflight docs must expose adapter-owned surface `{required_surface}`"
        );
    }
}

#[test]
fn prompt_assets_contract_guidance_uses_typed_verification_evidence() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_assets_path = repo_root.join("src").join("agent").join("prompt_assets.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let prompt_assets =
        fs::read_to_string(prompt_assets_path.as_std_path()).expect("read prompt assets module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let contract_guidance_block = prompt_assets
        .split("fn verification_failure_contract_line")
        .nth(1)
        .and_then(|tail| tail.split("pub(crate) fn max_steps_reminder").next())
        .expect("prompt assets verification contract guidance block");

    for forbidden in [
        "lower.contains(\"unsupported operation:\")",
        "lower.contains(\"unsupported operator:\")",
        "lower.contains(\"unsupported unary operator:\")",
        "summary.contains(\"未対応の演算子\")",
        "fn extract_verification_contract_call_sites",
        "fn looks_like_contract_call_site",
        "line.starts_with(\"Traceback \")",
        "lower.contains(\"assert\")",
        "lower.contains(\"expect(\")",
        "lower.contains(\"subprocess.run\")",
        "lower.contains(\"self._run\")",
        "lower.contains(\"output =\")",
        "lower.contains(\"result =\")",
    ] {
        assert!(
            !contract_guidance_block.contains(forbidden),
            "prompt assets verification contract guidance must not use raw failure-summary classifier authority `{forbidden}`"
        );
    }
    for required_surface in [
        "VerificationFailureCluster",
        "VerificationFailureEvidence",
        "VerificationContractPromptProjection",
        "prompt_assets_contract_guidance_typed_verification_evidence",
    ] {
        assert!(
            contract_guidance_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface),
            "prompt assets verification contract guidance or active preflight docs must expose typed verification evidence surface `{required_surface}`"
        );
    }
}

#[test]
fn session_markdown_tooloutput_preserves_blocked_action_evidence() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let markdown_path = repo_root.join("src").join("session").join("markdown.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let markdown = fs::read_to_string(markdown_path.as_std_path()).expect("read session markdown");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let tool_output_block = markdown
        .split("HistoryItemPayload::ToolOutput")
        .nth(1)
        .and_then(|tail| tail.split("HistoryItemPayload::RequestDiagnostics").next())
        .expect("session markdown ToolOutput block");

    assert!(
        !tool_output_block.contains("let _ = blocked_action"),
        "session Markdown ToolOutput export must not discard blocked_action evidence"
    );
    for required_surface in [
        "Blocked action",
        "blocked_action",
        "session_markdown_blocked_action_evidence_preserved",
    ] {
        assert!(
            tool_output_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "session Markdown ToolOutput export or active preflight docs must preserve blocked-action evidence surface `{required_surface}`"
        );
    }
}

#[test]
fn harness_no_progress_signature_schema_matches_runtime_projection() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let runtime_writer_path = repo_root
        .join("src")
        .join("harness")
        .join("runtime_writer.rs");
    let schema_path = repo_root.join("src").join("harness").join("schema.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_writer =
        fs::read_to_string(runtime_writer_path.as_std_path()).expect("read runtime writer");
    let schema = fs::read_to_string(schema_path.as_std_path()).expect("read harness schema");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let projection_block = runtime_writer
        .split("fn no_progress_signature_projection")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn no_progress_signature_projection_matches_schema_fixture_passes",
            )
            .next()
        })
        .expect("runtime no-progress signature projection block");
    let schema_block = schema
        .split("fn tool_no_progress_signature_schema")
        .nth(1)
        .and_then(|tail| tail.split("fn ").next())
        .expect("ToolNoProgressSignature schema block");
    let schema_required_block = schema_block
        .split("json!")
        .next()
        .expect("ToolNoProgressSignature required block");

    for required_runtime_field in [
        "\"result_hash\"",
        "\"tool\"",
        "\"progress_effect\"",
        "\"repeat_count\"",
    ] {
        assert!(
            projection_block.contains(required_runtime_field),
            "runtime no_progress_signature projection must include typed identity field {required_runtime_field}"
        );
    }
    for required_schema_field in [
        "\"result_hash\"",
        "\"tool\"",
        "\"progress_effect\"",
        "\"allowed_surface_snapshot\"",
        "\"repeat_count\"",
    ] {
        assert!(
            schema_block.contains(required_schema_field),
            "ToolNoProgressSignature schema must define runtime projection field {required_schema_field}"
        );
    }
    for required_schema_field in [
        "\"result_hash\"",
        "\"tool\"",
        "\"progress_effect\"",
        "\"allowed_surface_snapshot\"",
        "\"repeat_count\"",
    ] {
        assert!(
            schema_required_block.contains(required_schema_field),
            "ToolNoProgressSignature schema must require runtime identity field {required_schema_field}"
        );
    }
    assert!(
        preflight.contains("harness_no_progress_signature_schema_runtime_projection")
            && preflight_docs.contains("harness_no_progress_signature_schema_runtime_projection"),
        "active preflight must cover no-progress signature schema/runtime projection sync"
    );
}

#[test]
fn synthetic_feedback_preflight_markers_have_executable_fixtures() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let fixture_block = preflight
        .split("PreflightFixture {")
        .find(|block| {
            block.contains("fixture.tool_lifecycle.synthetic_feedback_not_verification_authority")
                && block.contains("required_refs: vec!")
        })
        .expect("synthetic feedback preflight fixture block");

    for (marker, executable_fixture) in [
        (
            "truncated_tool_output_feedback_registered_tool_surface",
            "truncated_tool_output_feedback_uses_registered_tool_surface_fixture_passes",
        ),
        (
            "harness_no_progress_signature_schema_runtime_projection",
            "no_progress_signature_projection_matches_schema_fixture_passes",
        ),
        (
            "harness_no_progress_signature_schema_runtime_projection",
            "tool_no_progress_signature_schema_matches_runtime_projection_fixture_passes",
        ),
    ] {
        assert!(
            fixture_block.contains(marker),
            "synthetic feedback preflight fixture must advertise marker `{marker}`"
        );
        assert!(
            preflight.contains(executable_fixture),
            "synthetic feedback preflight marker `{marker}` must be backed by executable fixture `{executable_fixture}` in preflight diagnostics"
        );
    }
}

#[test]
fn preflight_gate_suite_docs_list_all_active_gate_ids() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let mut gate_ids = Vec::new();
    for fragment in preflight.split("gate_id:").skip(1) {
        let Some(start) = fragment.find('"') else {
            continue;
        };
        let remainder = &fragment[start + 1..];
        let Some(end) = remainder.find('"') else {
            continue;
        };
        let gate_id = &remainder[..end];
        if gate_id.starts_with("preflight.") && !gate_ids.iter().any(|seen| seen == gate_id) {
            gate_ids.push(gate_id.to_string());
        }
    }

    assert!(
        !gate_ids.is_empty(),
        "module responsibility guard must extract implementation-owned preflight gate ids"
    );
    for gate_id in gate_ids {
        assert!(
            preflight_docs.contains(&format!("`{gate_id}`")),
            "PreflightGateSuite.md must list implementation-owned active gate id `{gate_id}`"
        );
    }
}

#[test]
fn repair_lane_source_target_matching_rejects_sibling_suffix() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let repair_lane_path = repo_root.join("src").join("agent").join("repair_lane.rs");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let repair_lane =
        fs::read_to_string(repair_lane_path.as_std_path()).expect("read repair lane module");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");

    let source_equivalence_block = repair_lane
        .split("fn source_targets_equivalent")
        .nth(1)
        .and_then(|tail| tail.split("fn source_target_from_test_targets").next())
        .expect("source target equivalence block");
    let active_target_block = repair_lane
        .split("fn active_targets_contain_repair_target")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn workflow_source_public_operation_cluster")
                .next()
        })
        .expect("active target repair target block");

    for forbidden in [
        ".ends_with(&format!(\"/{}\", candidate.to_ascii_lowercase()))",
        ".ends_with(&format!(\"/{}\", target.to_ascii_lowercase()))",
        "normalized_active.ends_with(&format!(\"/{normalized_target}\"))",
        "normalized_target.ends_with(&format!(\"/{normalized_active}\"))",
    ] {
        assert!(
            !source_equivalence_block.contains(forbidden)
                && !active_target_block.contains(forbidden),
            "repair lane source target identity must reject suffix-only match pattern `{forbidden}`"
        );
    }
    for required in [
        "repair_lane_source_target_identity_exact",
        "sibling-root suffix",
        "workspace-relative identity",
    ] {
        assert!(
            runtime_contracts.contains(required) || preflight_docs.contains(required),
            "repair lane source target identity contract must document `{required}`"
        );
    }
}

#[test]
fn repair_lane_public_state_obligations_are_domain_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let repair_lane_path = repo_root.join("src").join("agent").join("repair_lane.rs");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let repair_lane =
        fs::read_to_string(repair_lane_path.as_std_path()).expect("read repair lane module");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");

    let public_state_block = repair_lane
        .split("pub(crate) fn public_state_game_loop_operation_obligations")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn has_explicit_generated_test_conflict_evidence")
                .next()
        })
        .unwrap_or("");
    let sibling_obligation_block = repair_lane
        .split("fn repair_sibling_obligations_from_summary")
        .nth(1)
        .and_then(|tail| tail.split("fn verification_failure_cluster").next())
        .expect("repair sibling obligations from summary block");

    assert!(
        public_state_block.is_empty(),
        "generic repair lane must not keep game-loop-specific public-state obligation helper"
    );
    for forbidden in [
        "projectile",
        "bullet",
        "spawn",
        "shoot",
        "fire",
        "collision",
        "invader",
        "public_state_game_loop_operation_obligations",
    ] {
        assert!(
            !sibling_obligation_block
                .to_ascii_lowercase()
                .contains(forbidden),
            "generic repair sibling obligations must not contain game-loop authority `{forbidden}`"
        );
    }
    for required in [
        "repair_lane_public_state_obligations_domain_neutral",
        "domain-neutral",
        "public-state repair evidence",
    ] {
        assert!(
            runtime_contracts.contains(required) || preflight_docs.contains(required),
            "repair lane public-state obligation contract must document `{required}`"
        );
    }
}

#[test]
fn repair_lane_preflight_marker_diagnostics_share_state_reducer_owner() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight = preflight.replace("\r\n", "\n");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let state_reducer_gate_blocks = preflight
        .split(
            "if gate.gate_id\n        == \"preflight.state_reducer.verification_failure_preserves_repair_target_authority\"",
        )
        .skip(1)
        .map(|tail| tail.split("if gate.gate_id").next().unwrap_or(tail))
        .collect::<Vec<_>>();
    assert!(
        !state_reducer_gate_blocks.is_empty(),
        "state-reducer repair target authority diagnostics block must exist"
    );
    let state_reducer_gate_block = state_reducer_gate_blocks.join("\n");

    for required in [
        "repair_lane_source_target_identity_exact_fixture_passes",
        "repair_lane_public_state_obligations_domain_neutral_fixture_passes",
        "repair_lane_source_target_identity_exact",
        "repair_lane_public_state_obligations_domain_neutral",
    ] {
        assert!(
            state_reducer_gate_block.contains(required),
            "state-reducer repair target authority gate must own executable diagnostic marker `{required}`"
        );
    }
    assert!(
        preflight_docs
            .contains("repair_lane_preflight_marker_diagnostics_share_state_reducer_owner"),
        "PreflightGateSuite must document state-reducer ownership for repair-lane marker diagnostics"
    );
}

#[test]
fn state_verification_command_identity_fixture_is_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("agent").join("state.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = state
        .split("pub(crate) fn public_verification_command_identity_dedupes_required_commands_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn requested_work_verification_passed").next())
        .expect("state verification command identity fixture block");

    for stale_surface in [
        "component.py",
        "python -X utf8",
        "python -x utf8",
        " 8 +",
        " beta 42",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "state verification command identity fixture must not use component/Python arithmetic surface `{stale_surface}` as generic command authority"
        );
    }
    for required_surface in [
        "verify-workflow --behavior draft",
        "verify-workflow --behavior review",
        "state_verification_command_identity_fixture_workflow_neutral",
    ] {
        assert!(
            fixture_block.contains(required_surface) || preflight_docs.contains(required_surface),
            "state verification command identity fixture or preflight docs must contain workflow-neutral command surface `{required_surface}`"
        );
    }
}

#[test]
fn state_verification_repair_target_fixtures_are_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("agent").join("state.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = state
        .split("pub(crate) fn verification_repair_targets_from_state_ignore_diagnostic_scalars_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn verification_cluster_has_no_tests_ran")
                .next()
        })
        .expect("state verification repair target fixture block");

    for stale_surface in [
        "widget.py",
        "test_widget.py",
        "widget.compute",
        "python -m unittest",
        "python -X utf8",
        "AttributeError",
        "AssertionError",
        "test_public_behavior",
        "1 + 2",
        "widget.run",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "state verification repair target fixtures must not use widget/Python surface `{stale_surface}` as generic repair-target authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.contract",
        "workflow.advance",
        "verify-workflow --behavior repair",
        "state_verification_repair_target_fixture_workflow_neutral",
    ] {
        assert!(
            fixture_block.contains(required_surface) || preflight_docs.contains(required_surface),
            "state verification repair target fixtures or preflight docs must contain workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn state_public_command_continuation_summary_uses_typed_observation_markers() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("agent").join("state.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let helper_block = state
        .split("fn compact_public_command_contract_continuation_summary")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn continuation_verification_failure_evidence")
                .next()
        })
        .expect("state public-command continuation summary helper block");

    assert!(
        !helper_block.to_ascii_lowercase().contains("eoferror"),
        "state public-command continuation summary must not parse Python EOFError spelling as generic interactive-stdin authority"
    );
    for required in [
        "interactive stdin",
        "argv invocation entered interactive stdin mode",
        "state_public_command_continuation_summary_typed_observation",
    ] {
        assert!(
            helper_block.contains(required)
                || preflight.contains(required)
                || preflight_docs.contains(required),
            "state public-command continuation summary contract must expose typed observation marker `{required}`"
        );
    }
    assert!(
        preflight.contains(
            "state_public_command_continuation_summary_uses_typed_observation_markers_fixture_passes"
        ),
        "active preflight must execute the state public-command continuation typed-observation fixture"
    );
}

#[test]
fn state_docs_route_target_alias_matching_rejects_suffix_collision() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("agent").join("state.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let alias_block = state
        .split("fn docs_route_target_alias_matches")
        .nth(1)
        .and_then(|tail| tail.split("fn looks_like_docs_only_route_contract").next())
        .expect("state docs route target alias block");

    for forbidden in [
        "left.ends_with(&format!(\"/{right}\"))",
        "right.ends_with(&format!(\"/{left}\"))",
        ".ends_with(&format!(\"/{right}\"))",
        ".ends_with(&format!(\"/{left}\"))",
    ] {
        assert!(
            !alias_block.contains(forbidden),
            "state docs route target alias matching must reject suffix-only match pattern `{forbidden}`"
        );
    }
    for required in [
        "state_docs_route_target_alias_identity_exact",
        "docs route target alias",
        "workspace-relative identity",
    ] {
        assert!(
            preflight.contains(required)
                || runtime_contracts.contains(required)
                || preflight_docs.contains(required),
            "state docs route target alias identity contract must document `{required}`"
        );
    }
}

#[test]
fn prompt_staged_task_target_matching_rejects_foreign_suffix_collision() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_path = repo_root.join("src").join("agent").join("prompt.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let prompt = fs::read_to_string(prompt_path.as_std_path()).expect("read prompt module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let target_match_block = prompt
        .split("fn prompt_target_matches_required_output")
        .nth(1)
        .and_then(|tail| tail.split("fn collect_instruction_sources").next())
        .expect("prompt staged-task target matching block");

    for forbidden in [
        "normalized_target.ends_with(&format!(\"/{normalized_required}\"))",
        ".ends_with(&format!(\"/{normalized_required}\"))",
        "ends_with(&format!(\"/{normalized_required}\"))",
    ] {
        assert!(
            !target_match_block.contains(forbidden),
            "prompt staged-task target matching must reject suffix-only match pattern `{forbidden}`"
        );
    }
    for required_surface in [
        "prompt_staged_task_target_identity_exact",
        "prompt_staged_task_target_identity_exact_fixture_passes",
        "workspace-relative identity",
    ] {
        assert!(
            prompt.contains(required_surface)
                || preflight.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "prompt staged-task target identity contract must expose `{required_surface}`"
        );
    }
}

#[test]
fn state_docs_route_fixtures_are_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("agent").join("state.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = state
        .split("pub(crate) fn docs_route_contract_promotes_docs_repair_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn docs_route_localized_topic_completion_fixture_passes")
                .next()
        })
        .expect("state docs route fixture block");

    for stale_surface in [
        "component.py",
        "test_component.py",
        "docs/component-design.md",
        "component.calculate",
        "ComponentTest",
        "python -m unittest",
        "unittest",
        "2, '+', 3",
        "docs/widget-design.md",
        "widget-design",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "state docs route fixtures must not use component/Python surface `{stale_surface}` as generic docs route authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.contract",
        "docs/workflow-design.md",
        "verify-workflow --docs",
        "workflow.advance",
        "state_docs_route_fixture_workflow_neutral",
        "state_docs_route_stale_target_workflow_neutral",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "state docs route fixtures or active preflight docs must contain workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn state_requested_work_fixtures_are_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("agent").join("state.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = state
        .split("pub(crate) fn requested_work_missing_todo_graph_stays_authoring_authority")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn continuation_context_symbols_are_not_requested_work_targets_fixture_passes")
                .next()
        })
        .expect("state requested-work fixture block");

    for stale_surface in [
        "src/component.rs",
        "tests/component.behavior.md",
        "component.behavior",
        "component fixture authority",
        "test_contract_validation",
        "TestPublicState",
        "python313",
        "subprocess.py",
        "threading.py",
        "unittest/case.py",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "state requested-work fixtures must not use component surface `{stale_surface}` as generic requested-work authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.behavior.md",
        "verify-contract --behavior",
        "state_requested_work_fixture_workflow_neutral",
        "state_requested_work_diagnostic_fixture_workflow_neutral",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "state requested-work fixtures or active preflight docs must contain workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn state_residual_component_fixtures_are_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("agent").join("state.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let public_command_block = state
        .split("pub(crate) fn public_command_contract_continuation_projects_compact_source_repair_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn state_public_command_continuation_summary_uses_typed_observation_markers_fixture_passes")
                .next()
        })
        .expect("state public-command continuation residual fixture block");
    let generated_test_block = state
        .split("pub(crate) fn verification_repair_continuation_generated_test_parse_target_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn requested_work_completion_promotes_verification_fixture_passes")
                .next()
        })
        .expect("state generated-test parse continuation residual fixture block");
    let requested_work_block = state
        .split("pub(crate) fn requested_work_completion_promotes_verification_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn scenario_contract_reference_input_does_not_become_authoring_target_fixture_passes")
                .next()
        })
        .expect("state requested-work completion/no-progress residual fixture block");
    let combined =
        format!("{public_command_block}\n{generated_test_block}\n{requested_work_block}");

    for stale_surface in [
        "src/component.rs",
        "src/component.ts",
        "tests/component.command-contract",
        "tests/component.test.ts",
        "tests/component.behavior.md",
        "component.rs",
        "component.ts",
        "component.test",
        "component.behavior",
    ] {
        assert!(
            !combined.contains(stale_surface),
            "state residual fixtures must not use component surface `{stale_surface}` as generic state reducer authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "src/workflow.ts",
        "tests/workflow.command-contract",
        "tests/workflow.spec.ts",
        "tests/workflow.behavior.md",
        "verify-public-command --scenario compact-source",
        "verify-generated-test --parse",
        "verify-contract --behavior",
        "state_residual_component_fixture_workflow_neutral",
    ] {
        assert!(
            combined.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "state residual fixtures, active preflight, or preflight docs must contain workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn docs_semantic_contract_fixtures_are_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let docs_semantic_path = repo_root
        .join("src")
        .join("agent")
        .join("docs_semantic_contract.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let docs_semantic =
        fs::read_to_string(docs_semantic_path.as_std_path()).expect("read docs semantic module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let semantic_fixture_block = docs_semantic
        .split("pub fn docs_spec_semantic_reconciliation_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub fn docs_spec_semantic_reconciliation_feedback_projection_fixture_passes",
            )
            .next()
        })
        .expect("docs semantic reconciliation fixture block");
    let feedback_fixture_block = docs_semantic
        .split("pub fn docs_spec_semantic_reconciliation_feedback_projection_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn docs_semantic_contract_fixtures_are_workflow_neutral_fixture_passes",
            )
            .next()
        })
        .expect("docs semantic feedback projection fixture block");
    let latest_user_fixture_block = docs_semantic
        .split("pub(crate) fn latest_user_authority_text_uses_sequence_order_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub fn docs_spec_semantic_reconciliation_tool_fixture_passes")
                .next()
        })
        .expect("docs semantic latest-user fixture block");
    let tool_fixture_block = docs_semantic
        .split("pub fn docs_spec_semantic_reconciliation_tool_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn documentation_semantic_contract_applies")
                .next()
        })
        .expect("docs semantic tool fixture block");
    let fixture_block = format!(
        "{semantic_fixture_block}\n{feedback_fixture_block}\n{latest_user_fixture_block}\n{tool_fixture_block}"
    );

    for stale_surface in [
        "docs/component-design.md",
        "component-design",
        "component fixture authority",
        "tool beta",
        "python tool.py",
        "calculate_unary",
        "unsupported function exit code 2",
        "unsupported-function exit code 2",
        "未定義の関数",
        "python tool.py alpha 0",
        "python tool.py beta 16",
        "python tool.py gamma 10",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "docs semantic reconciliation fixture must not use component/Python/tool CLI surface `{stale_surface}` as generic semantic contract authority"
        );
    }
    for required_surface in [
        "docs/workflow-design.md",
        "workflow-tool archive draft",
        "workflow-tool inspect draft",
        "docs_semantic_contract_fixtures_are_workflow_neutral_fixture_passes",
        "docs_semantic_contract_cli_fixture_workflow_neutral",
        "docs_semantic_contract_fixture_workflow_neutral",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || docs_semantic.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "docs semantic contract, active preflight, or preflight docs must contain workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn docs_semantic_exit_code_evidence_is_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let docs_semantic_path = repo_root
        .join("src")
        .join("agent")
        .join("docs_semantic_contract.rs");
    let docs_semantic =
        fs::read_to_string(docs_semantic_path.as_std_path()).expect("read docs semantic module");

    for stale_surface in ["sys.exit(", "sys.exit (", "exit({code})", "exit ({code})"] {
        assert!(
            !docs_semantic.contains(stale_surface),
            "docs semantic exit-code evidence must not use Python/process API surface `{stale_surface}` as generic documentation authority"
        );
    }
    assert!(
        docs_semantic.contains("docs_semantic_exit_code_evidence_language_neutral"),
        "docs semantic source must expose language-neutral exit-code evidence marker"
    );

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "docs_semantic_exit_code_evidence_language_neutral",
            ),
            "docs/design surfaces must describe language-neutral docs semantic exit-code evidence"
        );
    }
}

#[test]
fn docs_semantic_documentation_target_classifier_is_shape_based() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let docs_semantic_path = repo_root
        .join("src")
        .join("agent")
        .join("docs_semantic_contract.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let docs_semantic =
        fs::read_to_string(docs_semantic_path.as_std_path()).expect("read docs semantic module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let classifier_block = docs_semantic
        .split("fn documentation_target")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn mentions_unknown_two_token_cli_usage_error")
                .next()
        })
        .expect("docs semantic documentation target classifier block");

    for stale_name in [
        "\"readme.md\"",
        "\"design.md\"",
        "\"spec.md\"",
        "\"basic_design.md\"",
        "\"detail_design.md\"",
        "\"detailed_design.md\"",
    ] {
        assert!(
            !classifier_block.contains(stale_name),
            "docs semantic documentation target classifier must not preserve exact target-name authority `{stale_name}`"
        );
    }
    for required_surface in [
        "normalized.contains(\"/docs/\")",
        "name.ends_with(\".md\")",
        "name.ends_with(\".markdown\")",
        "docs_semantic_documentation_target_classifier_shape_based",
    ] {
        assert!(
            classifier_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "docs semantic documentation target classifier contract must expose shape-based surface `{required_surface}`"
        );
    }
}

#[test]
fn edit_recovery_candidate_target_uses_normalized_open_target_identity() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let edit_recovery_path = repo_root.join("src").join("agent").join("edit_recovery.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let edit_recovery =
        fs::read_to_string(edit_recovery_path.as_std_path()).expect("read edit recovery module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let envelope_block = edit_recovery
        .split("pub(crate) fn failed_edit_control_recovery_envelope")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn invalid_tool_arguments_result")
                .next()
        })
        .expect("failed edit control recovery envelope block");

    for forbidden in [
        "active_targets.iter().any(|active| active == target)",
        "active == target",
    ] {
        assert!(
            !envelope_block.contains(forbidden),
            "invalid edit recovery envelope candidate target identity must not use raw string equality `{forbidden}`"
        );
    }
    for required_surface in [
        "normalize_path_for_target_match(target)",
        "invalid_edit_recovery_candidate_target_normalized_fixture_passes",
        "invalid_edit_recovery_candidate_target_normalized",
    ] {
        assert!(
            envelope_block.contains(required_surface)
                || edit_recovery.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "invalid edit recovery envelope, active preflight, or preflight docs must expose normalized candidate target identity surface `{required_surface}`"
        );
    }
}

#[test]
fn edit_recovery_targets_and_fixtures_are_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let edit_recovery_path = repo_root.join("src").join("agent").join("edit_recovery.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let edit_recovery =
        fs::read_to_string(edit_recovery_path.as_std_path()).expect("read edit recovery module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let grounding_block = edit_recovery
        .split("pub(crate) fn patch_context_mismatch_target_grounding_surface_active")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn patch_context_mismatch_target_grounding_read_satisfied")
                .next()
        })
        .expect("edit recovery patch-context grounding block");
    let prompt_block = edit_recovery
        .split("pub(crate) fn failed_edit_control_recovery_envelope")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn invalid_tool_arguments_result")
                .next()
        })
        .expect("edit recovery control prompt block");
    let fixture_block = edit_recovery
        .split("pub(crate) fn apply_patch_context_mismatch_enters_invalid_edit_lifecycle_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("edit recovery fixture block");

    for forbidden in [
        "normalized.ends_with(&format!(\"/{recorded}\"))",
        "recorded.ends_with(&format!(\"/{normalized}\"))",
        ".ends_with(&format!(\"/{recorded}\"))",
        ".ends_with(&format!(\"/{normalized}\"))",
    ] {
        assert!(
            !grounding_block.contains(forbidden),
            "edit recovery patch-context grounding must reject suffix-only match pattern `{forbidden}`"
        );
    }
    assert!(
        !prompt_block.contains("top-level `def`/`class`/`import` lines"),
        "edit recovery generic apply_patch grammar must not use Python def/class/import examples as provider-visible recovery authority"
    );
    for stale_surface in [
        "component.py",
        "widget.py",
        "test_widget.py",
        "calculator.py",
        "test_calculator.py",
        "import unittest",
        "import widget",
        "import calculator",
        "TestCalculator",
        "unittest",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "edit recovery fixtures must not use component/widget/calculator/Python surface `{stale_surface}` as generic invalid-edit authority"
        );
    }
    for required_surface in [
        "src/workflow.rs",
        "tests/workflow.spec.ts",
        "edit_recovery_targets_and_fixtures_are_workflow_neutral_fixture_passes",
        "edit_recovery_fixture_workflow_neutral",
    ] {
        assert!(
            edit_recovery.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "edit recovery contract, active preflight, or preflight docs must contain workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn grounding_metadata_path_matching_uses_exact_target_identity() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let grounding_path = repo_root
        .join("src")
        .join("agent")
        .join("grounding_evidence.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let grounding =
        fs::read_to_string(grounding_path.as_std_path()).expect("read grounding evidence module");
    let preflight =
        fs::read_to_string(preflight_path.as_std_path()).expect("read preflight module");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let metadata_match_block = grounding
        .split("pub(crate) fn metadata_path_matches_active_target")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn normalize_path_for_target_match")
                .next()
        })
        .expect("grounding metadata path match block");

    for forbidden in [
        "normalized_path.ends_with(&format!(\"/{normalized_target}\"))",
        ".ends_with(&format!(\"/{normalized_target}\"))",
    ] {
        assert!(
            !metadata_match_block.contains(forbidden),
            "grounding metadata active-target matching must reject suffix-only match pattern `{forbidden}`"
        );
    }
    for required_surface in [
        "matching_active_target_key(&normalized_path, &active_targets).is_some()",
        "grounding_metadata_path_matching_rejects_foreign_suffix_collision_fixture_passes",
        "grounding_metadata_path_target_identity_exact",
    ] {
        assert!(
            grounding.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "grounding evidence contract, active preflight, or preflight docs must contain exact target identity surface `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_control_envelope_uses_current_turn_id() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let compile_block = loop_impl
        .split("fn compile_turn_control_envelope")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn invalid_edit_recovery_projection_obligation")
                .next()
        })
        .expect("compile turn control envelope block");

    assert!(
        !compile_block.contains("turn_id: TurnId::new()"),
        "TurnRuntime control envelope must preserve the current protocol turn id instead of minting a fresh turn id"
    );
    for required_surface in [
        "turn_id: request.protocol_turn_id",
        "control_envelope_preserves_current_turn_id_fixture_passes",
        "loop_impl_control_envelope_current_turn_id",
    ] {
        assert!(
            loop_impl.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "loop_impl control-envelope contract, active preflight, or preflight docs must contain current-turn-id surface `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_escaped_source_fixture_is_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split(
            "pub(crate) fn source_content_shape_normalizes_escaped_repair_candidate_fixture_passes",
        )
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn source_content_shape_rejects_test_module_payload_fixture_passes",
            )
            .next()
        })
        .expect("escaped source normalization fixture block");

    for forbidden in [
        "def add",
        "if __name__",
        "print(add(2, 3))",
        "python_source_executable_content_shape",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "loop_impl escaped-source fixture must not use Python-shaped payload authority `{forbidden}` for a generic source target"
        );
    }
    for required_surface in [
        "workflow_state",
        "pub fn add",
        "loop_impl_escaped_source_fixture_language_neutral",
    ] {
        assert!(
            loop_impl.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "loop_impl escaped-source fixture, active preflight, or preflight docs must contain language-neutral source surface `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_terminal_guard_fixtures_are_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let open_obligation_block = loop_impl
        .split("pub(crate) fn open_obligation_final_message_guard_is_recovery_context_keyed_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn docs_route_final_message_recovery_requires_content_grounding_fixture_passes")
                .next()
        })
        .expect("open obligation terminal guard recovery fixture block");
    let executed_failure_block = loop_impl
        .split("pub(crate) fn executed_tool_failure_terminal_guard_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn progress_projection_loop_terminal_guard_fixture_passes")
                .next()
        })
        .expect("executed tool failure terminal guard fixture block");

    for forbidden in ["def build", "missing.py"] {
        assert!(
            !open_obligation_block.contains(forbidden)
                && !executed_failure_block.contains(forbidden),
            "loop_impl terminal guard fixtures must not use Python-shaped authority `{forbidden}`"
        );
    }
    for required_surface in [
        "docs/missing-workflow.md",
        "loop_impl_terminal_guard_fixture_language_neutral",
    ] {
        assert!(
            loop_impl.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "loop_impl terminal guard fixtures, active preflight, or preflight docs must contain workflow-neutral terminal guard surface `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_operation_intent_fixtures_are_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split(
            "pub(crate) fn open_authoring_operation_intent_preserves_tool_surface_fixture_passes",
        )
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn docs_route_semantic_no_progress_guard_fixture_passes")
                .next()
        })
        .expect("operation intent fixture block");

    assert!(
        !fixture_block.contains("artifact.py"),
        "loop_impl operation intent fixtures must not use Python file-extension authority as generic target evidence"
    );
    for required_surface in [
        "src/workflow.rs",
        "loop_impl_operation_intent_fixture_language_neutral",
    ] {
        assert!(
            loop_impl.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "loop_impl operation-intent fixture, active preflight, or preflight docs must contain workflow-neutral operation-intent surface `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_invalid_edit_fixtures_are_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split("pub(crate) fn invalid_edit_arguments_project_no_progress_recovery_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn non_edit_invalid_tool_arguments_terminal_guard_fixture_passes",
            )
            .next()
        })
        .expect("invalid edit fixture block");

    for forbidden in [
        "def render",
        "test_other.py",
        "import workflow_contract",
        "class WorkflowSpec",
        "def test_more",
        "def helper",
        "def test_workflow",
        "def`/`class`/`import`",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "loop_impl invalid-edit fixtures must not use Python-shaped payload authority `{forbidden}`"
        );
    }
    for required_surface in [
        "tests/other-workflow.spec.ts",
        "workflow behavior contract",
        "loop_impl_invalid_edit_fixture_language_neutral",
    ] {
        assert!(
            loop_impl.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "loop_impl invalid-edit fixture, active preflight, or preflight docs must contain workflow-neutral invalid-edit surface `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_malformed_write_fixtures_are_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split("pub(crate) fn non_edit_invalid_tool_arguments_terminal_guard_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn malformed_apply_patch_write_recovery_surface_fixture_passes")
                .next()
        })
        .expect("non-edit invalid arguments and malformed write fixture block");

    for forbidden in [
        "other.py",
        "def render",
        "import workflow_contract",
        "from workflow import render",
        "class WorkflowSpec",
        "def test_render",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "loop_impl malformed-write fixtures must not use Python-shaped payload authority `{forbidden}`"
        );
    }
    for required_surface in [
        "tests/other-workflow.spec.ts",
        "workflow behavior contract",
        "workflow_render",
        "loop_impl_malformed_write_fixture_language_neutral",
    ] {
        assert!(
            loop_impl.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "loop_impl malformed-write fixture, active preflight, or preflight docs must contain workflow-neutral malformed-write surface `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_malformed_apply_patch_fixtures_are_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split("pub(crate) fn malformed_apply_patch_write_recovery_surface_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn failed_patch_context_mismatch_reopens_target_grounding_fixture_passes")
                .next()
        })
        .expect("malformed apply_patch recovery fixture block");

    for forbidden in [
        "def add",
        "return a + b",
        "import workflow_contract",
        "class WorkflowContract",
        "def test_render",
        "def calculate",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "loop_impl malformed apply_patch fixtures must not use Python-shaped payload authority `{forbidden}`"
        );
    }
    for required_surface in [
        "workflow_add",
        "workflow behavior contract",
        "workflow stale source draft",
        "loop_impl_malformed_apply_patch_fixture_language_neutral",
    ] {
        assert!(
            loop_impl.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "loop_impl malformed apply_patch fixture, active preflight, or preflight docs must contain workflow-neutral malformed apply_patch surface `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_singleton_write_argument_fixture_is_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split("pub(crate) fn singleton_active_target_write_arguments_repair_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn verification_repair_target_grounding_surface_keeps_read_fixture_passes")
                .next()
        })
        .expect("singleton active target write argument repair fixture block");

    for forbidden in ["import workflow_contract", "test_other.py"] {
        assert!(
            !fixture_block.contains(forbidden),
            "loop_impl singleton write argument repair fixture must not use Python-shaped authority `{forbidden}`"
        );
    }
    for required_surface in [
        "workflow behavior contract",
        "tests/other-workflow.spec.ts",
        "loop_impl_singleton_write_argument_fixture_language_neutral",
    ] {
        assert!(
            loop_impl.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "loop_impl singleton write argument fixture, active preflight, or preflight docs must contain workflow-neutral write repair surface `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_docs_budget_edit_surface_fixtures_are_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split("pub(crate) fn docs_route_supporting_context_budget_exhaustion_is_recoverable_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn canonical_shell_command_keys").next())
        .expect("docs budget and edit surface registry fixture block");

    for forbidden in ["backend/app/main.py", "source.py", "test_source.py"] {
        assert!(
            !fixture_block.contains(forbidden),
            "loop_impl docs-budget/edit-surface fixtures must not use Python-shaped path authority `{forbidden}`"
        );
    }
    for required_surface in [
        "docs/workflow-design.md",
        "src/workflow.rs",
        "tests/workflow.behavior.md",
        "loop_impl_docs_budget_edit_surface_fixture_language_neutral",
    ] {
        assert!(
            loop_impl.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "loop_impl docs-budget/edit-surface fixture, active preflight, or preflight docs must contain workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_docs_route_budget_fixture_uses_workflow_neutral_targets() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split("pub(crate) fn docs_route_supporting_context_budget_exhaustion_is_recoverable_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("pub(crate) fn edit_surface_registry_symmetry_fixture_passes").next())
        .expect("docs route supporting-context budget fixture block");

    for forbidden in ["README.md", "basic_design.md", "detail_design.md"] {
        assert!(
            !fixture_block.contains(forbidden),
            "loop_impl docs-route budget fixture must not use exact historical docs target authority `{forbidden}`"
        );
    }
    for required_surface in [
        "docs/workflow-overview.md",
        "docs/workflow-design.md",
        "docs/workflow-contract.md",
        "loop_impl_docs_route_budget_fixture_workflow_neutral",
    ] {
        assert!(
            loop_impl.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "loop_impl docs-route budget fixture, active preflight, or preflight docs must contain workflow-neutral docs target surface `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_verification_public_command_fixtures_are_domain_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split("pub(crate) fn verification_active_work_preserves_tool_surface_and_rejects_wrong_command_failed_checks")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn docs_route_rejects_completed_deliverable_regression_fixture_passes")
                .next()
        })
        .expect("verification public-command fixture block");

    for forbidden in [
        "workflow-cli src/workflow.rs 8 +",
        "workflow-cli src/workflow.rs beta 42",
        "test_calculate",
        "workflow_compute",
        "workflow_compute(1 + 2)",
        "expected: Some(\"3\"",
        "\"1 + 2\"",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "loop_impl verification/public-command fixtures must not use calculator-shaped authority `{forbidden}`"
        );
    }
    for required_surface in [
        "workflow-tool combine draft + review",
        "workflow-tool inspect draft + review",
        "workflow_behavior_verification_contract",
        "workflow_source_operation_contract",
        "workflow_process",
        "loop_impl_verification_public_command_fixture_domain_neutral",
    ] {
        assert!(
            loop_impl.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "loop_impl verification/public-command fixture, active preflight, or preflight docs must contain workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_active_authoring_docs_regression_fixtures_are_domain_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split("pub(crate) fn active_authoring_rejects_wrong_target_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn tool_result_is_progress_projection_no_content")
                .next()
        })
        .expect("active-authoring and docs completed-deliverable regression fixture block");

    for forbidden in [
        "Arcade Game",
        "README.md",
        "basic_design.md",
        "detail_design.md",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "loop_impl active-authoring/docs regression fixtures must not use domain or stale docs authority `{forbidden}`"
        );
    }
    for required_surface in [
        "docs/workflow-design.md",
        "docs/workflow-contract.md",
        "docs/completed-workflow.md",
        "loop_impl_active_authoring_docs_regression_fixture_domain_neutral",
    ] {
        assert!(
            loop_impl.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "loop_impl active-authoring/docs regression fixture, active preflight, or preflight docs must contain workflow-neutral regression surface `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_docs_existing_target_grounding_fixture_is_domain_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = loop_impl
        .split("pub(crate) fn docs_existing_target_update_keeps_exact_read_grounding_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn generated_test_authoring_keeps_recent_source_reference_read_fixture_passes")
                .next()
        })
        .expect("docs existing-target grounding fixture block");

    for forbidden in ["docs/design.md", "README.md"] {
        assert!(
            !fixture_block.contains(forbidden),
            "loop_impl docs existing-target grounding fixture must not use stale docs/root authority `{forbidden}`"
        );
    }
    for required_surface in [
        "docs/workflow-design.md",
        "docs/other-workflow.md",
        "loop_impl_docs_existing_target_grounding_fixture_domain_neutral",
    ] {
        assert!(
            loop_impl.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "loop_impl docs existing-target grounding fixture, active preflight, or preflight docs must contain workflow-neutral grounding surface `{required_surface}`"
        );
    }
}

#[test]
fn prompt_provider_replay_residual_fixtures_are_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_path = repo_root.join("src").join("agent").join("prompt.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let prompt = fs::read_to_string(prompt_path.as_std_path()).expect("read prompt module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let fixture_block = prompt
        .split("pub(crate) fn stale_inactive_authoring_replay_uses_live_builder")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn content_shape_mismatch_replay_preserves_tool_lifecycle_without_payload",
            )
            .next()
        })
        .expect("prompt provider replay residual fixture block");

    for forbidden in [
        "source.py",
        "test_source.py",
        "README.md",
        "arcade_game.py",
        "test_arcade_game.py",
        "def implementation_only",
        "wrong target rewrite",
        "C:/workspace/source.py",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "prompt provider replay residual fixtures must not use Python/game/root README authority `{forbidden}` as generic replay evidence"
        );
    }
    for required_surface in [
        "src/inactive-workflow.rs",
        "src/workflow.rs",
        "tests/workflow.behavior.md",
        "docs/workflow-notes.md",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || prompt.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "prompt provider replay residual fixture, active preflight, or preflight docs must contain workflow-neutral surface `{required_surface}`"
        );
    }
    assert!(
        prompt.contains("prompt_provider_replay_residual_fixture_workflow_neutral_fixture_passes"),
        "prompt provider replay residual fixtures must expose an executable workflow-neutral marker"
    );
    assert!(
        preflight.contains("prompt_provider_replay_residual_fixture_workflow_neutral"),
        "active preflight must execute/report the prompt provider replay residual workflow-neutral marker"
    );
    assert!(
        preflight_docs.contains("prompt_provider_replay_residual_fixture_workflow_neutral"),
        "PreflightGateSuite must document the prompt provider replay residual workflow-neutral marker"
    );
}

#[test]
fn repair_lane_target_projection_has_no_stale_required_action_outrank_shim() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let repair_lane_path = repo_root.join("src").join("agent").join("repair_lane.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let repair_lane =
        fs::read_to_string(repair_lane_path.as_std_path()).expect("read repair lane module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");

    for forbidden in [
        "typed_repair_target_outranks_required_action",
        "no_tests_ran_generated_test_target_outranks_stale_write_action",
        "let _ = (subtype, typed_target);",
    ] {
        assert!(
            !repair_lane.contains(forbidden),
            "repair lane target projection must not retain stale required-action outrank shim `{forbidden}`"
        );
    }
    for required_surface in [
        "required_target_for_subtype(state, &subtype, verification_cluster.as_ref())",
        "normalize_source_owned_required_target",
        "contract_reconciliation",
        "repair_lane_typed_target_projection_no_required_action_shim",
    ] {
        assert!(
            repair_lane.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "repair lane target projection, active preflight, or preflight docs must contain typed target authority surface `{required_surface}`"
        );
    }
}

#[test]
fn turn_decision_repair_target_uses_exact_path_authority() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let turn_decision_path = repo_root.join("src").join("agent").join("turn_decision.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let turn_decision =
        fs::read_to_string(turn_decision_path.as_std_path()).expect("read turn decision module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let target_equivalence_block = turn_decision
        .split("fn diagnostic_targets_equivalent")
        .nth(1)
        .and_then(|tail| tail.split("fn warning").next())
        .expect("turn decision diagnostic target equivalence block");

    for forbidden in [".rsplit('/')", "left_name == right_name", "basename-only"] {
        assert!(
            !target_equivalence_block.contains(forbidden),
            "turn decision repair target authority must not use basename fallback `{forbidden}`"
        );
    }
    for required_surface in [
        "diagnostic_targets_equivalent",
        "turn_decision_repair_target_exact_path_authority_fixture_passes",
        "turn_decision_repair_target_exact_path_authority",
    ] {
        assert!(
            turn_decision.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "turn decision exact-target authority, active preflight, or preflight docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn tool_orchestrator_target_matching_uses_exact_path_authority() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let tool_orchestrator_path = repo_root
        .join("src")
        .join("agent")
        .join("tool_orchestrator.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let tool_orchestrator =
        fs::read_to_string(tool_orchestrator_path.as_std_path()).expect("read tool orchestrator");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");

    for forbidden in [
        "submitted_normalized.ends_with(&format!(\"/{target}\"))",
        "normalized_path.ends_with(&format!(\"/{normalized_target}\"))",
    ] {
        assert!(
            !tool_orchestrator.contains(forbidden),
            "tool orchestrator target matching must not accept suffix-equivalent target authority `{forbidden}`"
        );
    }
    for required_surface in [
        "target_key_family_matches_exactly",
        "tool_orchestrator_target_matching_exact_path_authority_fixture_passes",
        "tool_orchestrator_target_matching_exact_path_authority",
    ] {
        assert!(
            tool_orchestrator.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "tool orchestrator exact target authority, active preflight, or preflight docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn verification_history_items_use_sequence_primary_order() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let verification_path = repo_root.join("src").join("agent").join("verification.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let verification =
        fs::read_to_string(verification_path.as_std_path()).expect("read verification module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let ordering_block = verification
        .split("fn history_item_order_key")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn history_tool_output_counts_as_repair_progress")
                .next()
        })
        .expect("verification history item ordering block");

    for forbidden in [
        "history_item_order_scalar",
        "created_at_ms.saturating_mul",
        "(history_item_order_scalar(item), item.sequence_no)",
    ] {
        assert!(
            !ordering_block.contains(forbidden),
            "verification history reconstruction must not use timestamp-primary ordering `{forbidden}`"
        );
    }
    for required_surface in [
        "verification_history_sequence_primary_order_fixture_passes",
        "verification_history_sequence_primary_order",
    ] {
        assert!(
            verification.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface),
            "verification sequence-primary authority, active preflight, or preflight docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn verification_repair_cycle_uses_history_items_not_transcript() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let verification_path = repo_root.join("src").join("agent").join("verification.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let verification =
        fs::read_to_string(verification_path.as_std_path()).expect("read verification module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle design");

    for forbidden in [
        "pub(crate) fn latest_verification_repair_cycle(\n    transcript: &Transcript",
        "for message in &transcript.messages",
        "MessagePart::ToolCall",
        "MessagePart::ToolResult",
    ] {
        assert!(
            !verification.contains(forbidden),
            "verification repair-cycle authority must not read compatibility Transcript projection `{forbidden}`"
        );
    }
    assert!(
        verification.contains("latest_verification_repair_cycle_from_history_items"),
        "verification repair-cycle reconstruction must keep canonical HistoryItem owner"
    );
    for required_surface in [
        "verification_repair_cycle_history_item_authority_fixture_passes",
        "verification_repair_cycle_history_item_authority",
    ] {
        assert!(
            verification.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "verification repair-cycle HistoryItem authority, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_generated_test_source_reference_fixture_is_domain_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let fixture_block = loop_impl
        .split("pub(crate) fn generated_test_consumed_source_reference_requires_active_target_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn singleton_missing_authoring_target_projects_create_action_fixture_passes")
                .next()
        })
        .expect("generated-test source-reference fixture block");

    for forbidden in [
        "pub fn add(left: i32, right: i32) -> i32 { left + right }",
        "add returns the sum of two integers",
        "export const add",
        "a + b",
        "left + right",
        "sum of two integers",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "loop_impl generated-test source-reference fixture must not use calculator-shaped authority `{forbidden}`"
        );
    }
    for required_surface in [
        "workflow_process",
        "workflow source reference",
        "workflow generated test contract",
        "loop_impl_generated_test_source_reference_fixture_domain_neutral",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "generated-test source-reference fixture, active preflight, or design docs must contain workflow-neutral surface `{required_surface}`"
        );
    }
}

#[test]
fn loop_impl_provider_replay_effective_surface_fixture_uses_effective_test_payload() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let fixture_block = loop_impl
        .split("pub(crate) fn provider_replay_effective_tool_surface_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn provider_replay_preserves_supporting_context_evidence_after_surface_narrowing_fixture_passes",
            )
            .next()
        })
        .expect("provider replay effective-surface fixture block");

    for forbidden in [
        r#""content":"ok""#,
        r#""content": "ok""#,
        "Wrote tests/workflow.spec.ts",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "loop_impl provider replay effective-surface fixture must not preserve placeholder accepted write payload `{forbidden}`"
        );
    }
    for required_surface in [
        "workflow-generated-test-contract",
        "workflow_replay_behavior",
        "loop_impl_provider_replay_effective_surface_fixture_effective_test_payload",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "provider replay effective-surface fixture, active preflight, or design docs must contain effective generated-test payload surface `{required_surface}`"
        );
    }
}

#[test]
fn prompt_provider_replay_inactive_filechange_uses_exact_target_identity() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_path = repo_root.join("src").join("agent").join("prompt.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let prompt = fs::read_to_string(prompt_path.as_std_path()).expect("read prompt");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let provider_replay_block = prompt
        .split("fn inactive_filechange_reference_notes_after")
        .nth(1)
        .and_then(|tail| tail.split("fn clip_reference_snapshot").next())
        .expect("provider replay inactive FileChange target identity block");

    for forbidden in [
        r#"path.ends_with(&format!("/{active_normalized}"))"#,
        r#"active_normalized.ends_with(&format!("/{path}"))"#,
        r#"normalized.ends_with(&format!("/{target_normalized}"))"#,
    ] {
        assert!(
            !provider_replay_block.contains(forbidden),
            "prompt provider replay inactive FileChange target identity must not use suffix authority `{forbidden}`"
        );
    }
    for required_surface in [
        "prompt_provider_replay_inactive_filechange_exact_target_identity",
        "provider_replay_inactive_filechange_exact_target_identity_fixture_passes",
        "exact_normalized_target_identity",
    ] {
        assert!(
            provider_replay_block.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "prompt provider replay inactive FileChange target identity, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn state_history_item_order_uses_sequence_primary_authority() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("agent").join("state.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read agent state module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let order_block = state
        .split("fn history_items_in_sequence")
        .nth(1)
        .and_then(|tail| tail.split("pub fn project_model_turn_state").next())
        .expect("state history item ordering block");

    for forbidden in [
        "created_at_ms.saturating_mul",
        "history_item_order_scalar(item), item.sequence_no",
    ] {
        assert!(
            !order_block.contains(forbidden),
            "state reducer history ordering must be sequence-primary and must not use timestamp-primary authority `{forbidden}`"
        );
    }
    for required_surface in [
        "state_history_item_sequence_primary_order",
        "state_history_item_sequence_primary_order_fixture_passes",
        "canonical_history_item_sequence_primary",
    ] {
        assert!(
            state.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "state reducer history order, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn state_structured_document_output_progress_uses_exact_target_identity() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("agent").join("state.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read agent state module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let progress_block = state
        .split("fn structured_document_summary_progress_from_history_items")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn message_user_structured_document_progress_fixture_passes")
                .next()
        })
        .expect("structured document summary progress block");

    for forbidden in [
        "path.ends_with(&output_target)",
        "summary.to_ascii_lowercase().contains(&output_target)",
    ] {
        assert!(
            !progress_block.contains(forbidden),
            "structured document output progress must use exact FileChange target identity and must not use suffix/text authority `{forbidden}`"
        );
    }
    for required_surface in [
        "state_structured_document_output_progress_exact_target_identity",
        "structured_document_output_progress_exact_target_identity_fixture_passes",
        "canonical_filechange_output_target_identity",
    ] {
        assert!(
            state.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "state structured-document progress, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn state_structured_document_docling_progress_uses_exact_target_identity() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("agent").join("state.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read agent state module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let extract_block = state
        .split("fn extract_docling_target")
        .nth(1)
        .and_then(|tail| tail.split("fn extract_failure_paths_from_text").next())
        .expect("structured document Docling target extraction block");

    for forbidden in [".file_name()", "map(str::to_string)"] {
        assert!(
            !extract_block.contains(forbidden),
            "structured document Docling progress must preserve exact normalized path identity and must not use basename-only authority `{forbidden}`"
        );
    }
    for required_surface in [
        "state_structured_document_docling_progress_exact_target_identity",
        "structured_document_docling_progress_exact_target_identity_fixture_passes",
        "canonical_docling_source_target_identity",
    ] {
        assert!(
            state.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "state structured-document Docling progress, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn verification_dotted_technology_tokens_are_not_file_targets() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let verification_path = repo_root.join("src").join("agent").join("verification.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let verification =
        fs::read_to_string(verification_path.as_std_path()).expect("read verification module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let classifier_block = verification
        .split("fn contains_explicit_file_target")
        .nth(1)
        .and_then(|tail| tail.split("#[derive(Debug, Default)]").next())
        .expect("verification explicit file target classifier block");

    assert!(
        !classifier_block.contains("candidate.contains('.')"),
        "verification requirement classifier must not treat every dotted token as an explicit file target"
    );
    for required_surface in [
        "verification_dotted_technology_token_not_file_target",
        "verification_dotted_technology_tokens_are_not_file_targets_fixture_passes",
    ] {
        assert!(
            verification.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "verification requirement classifier, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn app_resume_latest_user_message_uses_sequence_primary_order() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let run_service_path = repo_root.join("src").join("app").join("run_service.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let run_service =
        fs::read_to_string(run_service_path.as_std_path()).expect("read app run_service module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let latest_user_block = run_service
        .split("fn latest_user_message_id_from_history_items")
        .nth(1)
        .and_then(|tail| tail.split("fn build_user_thread_op").next())
        .expect("app resume latest-user selection block");

    assert!(
        !latest_user_block.contains("(item.created_at_ms, item.sequence_no)"),
        "app resume latest-user selection must not use timestamp-primary history item order"
    );
    for required_surface in [
        "app_resume_latest_user_sequence_primary_order",
        "resume_latest_user_message_uses_item_order_fixture_passes",
    ] {
        assert!(
            run_service.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "app run service, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn cli_json_history_renderer_respects_reasoning_visibility() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let render_path = repo_root.join("src").join("cli").join("render.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let render = fs::read_to_string(render_path.as_std_path()).expect("read CLI render module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let json_history_block = render
        .split("fn render_session_history_items")
        .nth(2)
        .and_then(|tail| tail.split("fn transcript_for_history_render").next())
        .expect("JSON history renderer block");

    assert!(
        !json_history_block.contains("serde_json::to_value(history_items)?"),
        "CLI JSON history renderer must not serialize raw history_items when show_reasoning=false can hide reasoning in the transcript"
    );
    for required_surface in [
        "cli_json_history_renderer_respects_reasoning_visibility",
        "cli_json_history_renderer_respects_reasoning_visibility_fixture_passes",
    ] {
        assert!(
            render.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "CLI renderer, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn config_provider_metadata_mode_default_is_openai_compatible() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let model_path = repo_root.join("src").join("config").join("model.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let model = fs::read_to_string(model_path.as_std_path()).expect("read config model module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let provider_mode_block = model
        .split("pub enum ProviderMetadataMode")
        .nth(1)
        .and_then(|tail| tail.split("pub struct FormatterRule").next())
        .expect("provider metadata mode enum block");

    assert!(
        provider_mode_block.contains("#[default]\n    LmStudioNativeRequired"),
        "ProviderMetadataMode::default must be the current LM Studio native-required mode"
    );
    assert!(
        !provider_mode_block.contains("#[default]\n    OpenAiCompatibleOnly"),
        "ProviderMetadataMode::default must not restore stale OpenAI-compatible/vLLM mode"
    );
    for required_surface in [
        "provider_metadata_mode_default_lm_studio",
        "provider_metadata_mode_default_lm_studio_fixture_passes",
    ] {
        assert!(
            model.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "config model, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn desktop_app_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let app_path = repo_root.join("src").join("desktop").join("app.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let app = fs::read_to_string(app_path.as_std_path()).expect("read desktop app module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let async_contract_fixture_block = app
        .split("fn runtime_message_async_contract_classifies_representative_backflow_sources")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn project_delete_selects_only_non_deleted_remaining_project")
                .next()
        })
        .expect("desktop app async-contract fixture block");
    let markdown_export_fixture_block = app
        .split("fn open_transcript_markdown_keeps_visible_rows_and_metadata")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn completion_notification_body_summarizes_terminal_run")
                .next()
        })
        .expect("desktop app open transcript Markdown fixture block");
    let executable_fixture_block = app
        .split("pub fn desktop_open_transcript_markdown_preserves_visible_evidence_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn full_effective_override").next())
        .expect("desktop app executable fixture block");
    let fixture_block = format!(
        "{async_contract_fixture_block}\n{markdown_export_fixture_block}\n{executable_fixture_block}"
    );

    for forbidden in [
        "http://127.0.0.1:1234",
        "http://localhost:1234",
        "qwen/example",
        "local-model",
        "context: Some(4096)",
        "max_output_tokens: Some(1024)",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "Desktop app projection fixtures must not retain stale provider profile authority `{forbidden}`"
        );
    }
    for required_surface in [
        "http://127.0.0.1:1234",
        "qwen/qwen3.6-35b-a3b",
        "ProviderMetadataMode::LmStudioNativeRequired",
        "context: Some(131072)",
        "max_output_tokens: Some(8192)",
        "desktop_app_fixture_current_provider_profile",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || app.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "Desktop app projection fixtures, active preflight, or design docs must contain current provider-profile surface `{required_surface}`"
        );
    }
}

#[test]
fn desktop_query_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let query_path = repo_root.join("src").join("desktop").join("query.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let query = fs::read_to_string(query_path.as_std_path()).expect("read desktop query module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let primary_reading_fixture_block = query
        .split("pub fn completed_desktop_transcript_primary_reading_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub fn desktop_pseudo_tool_call_closeout_evidence_preserved_fixture_passes")
                .next()
        })
        .expect("desktop query primary reading fixture block");
    let turn_order_fixture_block = query
        .split("pub(crate) fn desktop_turn_item_projection_uses_turn_local_sequence_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn session_status_is_terminal").next())
        .expect("desktop query turn order fixture block");
    let test_session_record_block = query
        .split("fn session_record(project_id: ProjectId, title: &str) -> SessionRecord")
        .nth(1)
        .and_then(|tail| tail.split("#[test]\n    fn session_selection_prefers_current_project_without_explicit_session").next())
        .expect("desktop query test session record block");
    let fixture_block = format!(
        "{primary_reading_fixture_block}\n{turn_order_fixture_block}\n{test_session_record_block}"
    );

    for forbidden in ["http://localhost:1234", "model: \"model\""] {
        assert!(
            !fixture_block.contains(forbidden),
            "Desktop query projection fixtures must not retain stale provider profile authority `{forbidden}`"
        );
    }
    for required_surface in [
        "http://127.0.0.1:1234",
        "qwen/qwen3.6-35b-a3b",
        "desktop_query_fixture_current_provider_profile",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || query.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "Desktop query projection fixtures, active preflight, or design docs must contain current provider-profile surface `{required_surface}`"
        );
    }
}

#[test]
fn desktop_state_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("desktop").join("state.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read desktop state module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let open_session_fixture_block = state
        .split("fn loaded_open_session_detail_keeps_elapsed_work_summary_title")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn cancel_request_terminalizes_busy_projection_and_row_label")
                .next()
        })
        .expect("desktop state open-session fixture block");

    for forbidden in ["http://localhost:1234", "model: \"model\""] {
        assert!(
            !open_session_fixture_block.contains(forbidden),
            "Desktop state projection fixtures must not retain stale provider profile authority `{forbidden}`"
        );
    }
    for required_surface in [
        "http://127.0.0.1:1234",
        "qwen/qwen3.6-35b-a3b",
        "desktop_state_fixture_current_provider_profile",
    ] {
        assert!(
            open_session_fixture_block.contains(required_surface)
                || state.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "Desktop state projection fixtures, active preflight, or design docs must contain current provider-profile surface `{required_surface}`"
        );
    }
}

#[test]
fn cli_renderer_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let render_path = repo_root.join("src").join("cli").join("render.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let render = fs::read_to_string(render_path.as_std_path()).expect("read CLI render module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let canonical_fixture_block = render
        .split("pub fn cli_history_renderer_uses_canonical_transcript_projection_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub fn cli_history_renderer_ignores_compatibility_transcript_fixture_passes",
            )
            .next()
        })
        .expect("CLI canonical history renderer fixture block");
    let compatibility_fixture_block = render
        .split("pub fn cli_history_renderer_ignores_compatibility_transcript_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub fn cli_json_history_renderer_respects_reasoning_visibility_fixture_passes",
            )
            .next()
        })
        .expect("CLI compatibility history renderer fixture block");
    let reasoning_fixture_block = render
        .split("pub fn cli_json_history_renderer_respects_reasoning_visibility_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn strip_reasoning").next())
        .expect("CLI JSON reasoning renderer fixture block");
    let fixture_block = format!(
        "{canonical_fixture_block}\n{compatibility_fixture_block}\n{reasoning_fixture_block}"
    );

    for forbidden in ["http://localhost:1234", "model: \"model\""] {
        assert!(
            !fixture_block.contains(forbidden),
            "CLI renderer projection fixtures must not retain stale provider profile authority `{forbidden}`"
        );
    }
    for required_surface in [
        "http://127.0.0.1:1234",
        "qwen/qwen3.6-35b-a3b",
        "cli_renderer_fixture_current_provider_profile",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || render.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "CLI renderer projection fixtures, active preflight, or design docs must contain current provider-profile surface `{required_surface}`"
        );
    }
}

#[test]
fn llm_contract_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let contract_path = repo_root.join("src").join("llm").join("contract.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let contract = fs::read_to_string(contract_path.as_std_path()).expect("read llm contract");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let fixture_block = contract
        .split("pub fn tool_call_turn_uses_configured_output_budget_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub fn chat_request_tool_choice_is_provider_neutral_typed_fixture_passes")
                .next()
        })
        .expect("llm contract provider request fixture block");

    for forbidden in ["local-tool-model", "http://localhost:8110"] {
        assert!(
            !fixture_block.contains(forbidden),
            "llm contract provider request fixtures must not retain stale provider profile authority `{forbidden}`"
        );
    }
    for required_surface in [
        "qwen/qwen3.6-35b-a3b",
        "http://127.0.0.1:1234",
        "llm_contract_fixture_current_provider_profile",
        "llm_contract_current_provider_profile_fixture_passes",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || contract.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "llm contract fixtures, active preflight, or design docs must contain current provider-profile surface `{required_surface}`"
        );
    }
}

#[test]
fn protocol_runtime_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let runtime_path = repo_root.join("src").join("protocol").join("runtime.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let runtime = fs::read_to_string(runtime_path.as_std_path()).expect("read protocol runtime");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let fixture_block = runtime
        .split("pub fn repair_target_identity_aliases_compile_exact_write_action_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("impl WorkOrder").next())
        .expect("protocol runtime control-plane fixture block");

    for forbidden in [
        "model: \"model\"",
        "http://localhost:1234",
        "context_window: 8192",
        "max_output_tokens: 1024",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "protocol runtime control-plane fixtures must not retain stale provider profile authority `{forbidden}`"
        );
    }
    for required_surface in [
        "qwen/qwen3.6-35b-a3b",
        "http://127.0.0.1:1234",
        "CURRENT_PROVIDER_CONTEXT_WINDOW",
        "CURRENT_PROVIDER_MAX_OUTPUT_TOKENS",
        "protocol_runtime_fixture_current_provider_profile",
        "repair_target_identity_aliases_compile_exact_write_action_fixture_passes",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || runtime.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "protocol runtime fixtures, active preflight, or design docs must contain current provider-profile surface `{required_surface}`"
        );
    }
}

#[test]
fn protocol_mod_projection_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let protocol_mod_path = repo_root.join("src").join("protocol").join("mod.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let protocol_mod =
        fs::read_to_string(protocol_mod_path.as_std_path()).expect("read protocol mod");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let fixture_block = protocol_mod
        .split("pub fn history_item_projection_roles_are_not_authority_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("pub fn canonical_tool_call_arguments").next())
        .expect("protocol mod projection-role fixture block");

    for forbidden in [
        "model: \"model\"",
        "model_name: \"model\"",
        "http://localhost:1234",
        "context_window: 8192",
        "max_output_tokens: 1024",
        "configured_max_output_tokens: None",
        "effective_max_output_tokens: None",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "protocol mod projection fixture must not retain stale provider profile authority `{forbidden}`"
        );
    }
    for required_surface in [
        "qwen/qwen3.6-35b-a3b",
        "http://127.0.0.1:1234",
        "CURRENT_PROTOCOL_FIXTURE_CONTEXT_WINDOW",
        "CURRENT_PROTOCOL_FIXTURE_MAX_OUTPUT_TOKENS",
        "protocol_mod_projection_fixture_current_provider_profile",
        "history_item_projection_roles_are_not_authority_fixture_passes",
    ] {
        assert!(
            fixture_block.contains(required_surface)
                || protocol_mod.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "protocol mod projection fixtures, active preflight, or design docs must contain current provider-profile surface `{required_surface}`"
        );
    }
}

#[test]
fn protocol_tool_call_arguments_do_not_fallback_to_legacy_display_projection() {
    assert!(
        protocol_tool_call_arguments_do_not_fallback_to_legacy_display_projection_fixture_passes(),
        "protocol fixture must prove display/materialized ToolCall arguments are not canonical authority"
    );

    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let protocol_mod_path = repo_root.join("src").join("protocol").join("mod.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let protocol_mod =
        fs::read_to_string(protocol_mod_path.as_std_path()).expect("read protocol mod");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let tool_call_block = protocol_mod
        .split("ToolCall {")
        .nth(1)
        .and_then(|tail| tail.split("ToolOutput {").next())
        .expect("protocol ToolCall block");
    let canonical_block = protocol_mod
        .split("pub fn canonical_tool_call_arguments")
        .nth(1)
        .and_then(|tail| {
            tail.split("#[derive(Debug, Clone, Serialize, Deserialize)]")
                .next()
        })
        .expect("canonical tool call argument selector block");

    for forbidden in [
        "Compatibility projection used by old transcript/materialized views",
        "old transcript",
        "} else {\n        arguments\n    }",
    ] {
        assert!(
            !tool_call_block.contains(forbidden) && !canonical_block.contains(forbidden),
            "protocol ToolCall argument authority must not retain legacy display fallback `{forbidden}`"
        );
    }
    for required_surface in [
        "legacy_display_arguments_not_canonical",
        "protocol_tool_call_arguments_do_not_fallback_to_legacy_display_projection_fixture_passes",
        "protocol_tool_call_typed_arguments_authority",
    ] {
        assert!(
            protocol_mod.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "protocol ToolCall typed argument authority must expose `{required_surface}`"
        );
    }
}

#[test]
fn protocol_store_latest_turn_position_uses_unified_item_stream() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let store_path = repo_root.join("src").join("protocol").join("store.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let store = fs::read_to_string(store_path.as_std_path()).expect("read protocol store");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let latest_position_block = store
        .split("fn latest_turn_position_for_session")
        .nth(1)
        .and_then(|tail| tail.split("trait ProtocolSqlExecutor").next())
        .expect("protocol store latest-turn position block");

    for forbidden in [
        ".or(query_latest_turn_id(",
        "ORDER BY rowid DESC LIMIT 1",
        "query_latest_turn_id(",
    ] {
        assert!(
            !latest_position_block.contains(forbidden),
            "protocol store latest-turn position must not retain table-precedence authority `{forbidden}`"
        );
    }
    for required_surface in [
        "query_latest_protocol_turn_id",
        "UNION ALL",
        "protocol_runtime_events",
        "protocol_history_items",
        "protocol_turn_items",
        "protocol_store_latest_turn_position_unified_item_stream",
        "protocol_store_latest_turn_position_uses_unified_item_stream_fixture_passes",
    ] {
        assert!(
            latest_position_block.contains(required_surface)
                || store.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "protocol store latest-turn position, active preflight, or design docs must contain unified item-stream surface `{required_surface}`"
        );
    }
}

#[test]
fn failure_registry_projection_sync_checks_artifact_latest_entry_parity() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let json_path = workspace_root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let markdown_path = workspace_root
        .join("docs")
        .join("testing")
        .join("FailureRegistry.md");
    let sandbox_path = workspace_root.join("project_sandbox");

    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let json_text =
        fs::read_to_string(json_path.as_std_path()).expect("read failure registry json");
    let markdown =
        fs::read_to_string(markdown_path.as_std_path()).expect("read failure registry markdown");
    let json_value: serde_json::Value =
        serde_json::from_str(&json_text).expect("parse failure registry json");
    let entries = json_value
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .expect("failure registry entries");

    for required_surface in [
        "failure_registry_artifact_ids",
        "fr22_id_from_artifact_dir_name",
        "project_sandbox_FR22_artifact_ids",
        "latest_entry_parity",
        "source_reread_artifact_ref_set_parity",
        "markdown_unique_section_ids",
        "json_markdown_section_sequence_parity",
    ] {
        assert!(
            preflight.contains(required_surface),
            "failure registry preflight must contain artifact/latest parity surface `{required_surface}`"
        );
    }
    let fixture_metadata_block = preflight
        .split("PreflightFixture {\n            fixture_id: \"fixture.harness.failure_registry_projection_sync\"")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "PreflightFixture {\n            fixture_id: \"fixture.item_lifecycle.provider_replay_call_output_symmetry\"",
            )
            .next()
        })
        .expect("Failure Registry projection sync default fixture metadata block");
    assert!(
        fixture_metadata_block.contains("source_reread_artifact_ref_set_parity"),
        "Failure Registry preflight fixture metadata must expose source-reread artifact ref set parity"
    );
    assert!(
        !fixture_metadata_block.contains("artifact_registry_id_parity"),
        "Failure Registry preflight fixture metadata must not expose broad artifact registry id parity"
    );

    let mut json_ids = BTreeSet::new();
    let mut json_id_sequence = Vec::new();
    let mut json_source_reread_claim_ids = BTreeSet::new();
    let mut json_source_reread_ref_ids = BTreeSet::new();
    let mut json_status_by_id = std::collections::BTreeMap::new();
    for entry in entries {
        let id = entry
            .get("id")
            .and_then(serde_json::Value::as_str)
            .expect("entry id");
        if !id.starts_with("FR22-") {
            continue;
        }
        let status = entry
            .get("status")
            .and_then(serde_json::Value::as_str)
            .expect("entry status");
        if entry
            .get("direct_cause")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|cause| {
                cause.contains("No-grep")
                    || cause.contains("source reread")
                    || cause.contains("source-reread")
            })
        {
            json_source_reread_claim_ids.insert(id.to_string());
        }
        let expected_id_slug = id.to_ascii_lowercase();
        if entry
            .get("artifact_refs")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|refs| {
                refs.iter().any(|value| {
                    value.as_str().is_some_and(|artifact_ref| {
                        let artifact_ref = artifact_ref.to_ascii_lowercase();
                        artifact_ref.contains(&expected_id_slug)
                            && artifact_ref.ends_with("source-reread-evidence.txt")
                    })
                })
            })
        {
            json_source_reread_ref_ids.insert(id.to_string());
        }
        json_ids.insert(id.to_string());
        assert!(
            json_status_by_id
                .insert(id.to_string(), status.to_string())
                .is_none(),
            "Failure Registry JSON must not contain duplicate FR22 id `{id}`"
        );
        json_id_sequence.push(id.to_string());
    }

    let mut markdown_status_by_id = std::collections::BTreeMap::new();
    let mut markdown_id_sequence = Vec::new();
    let mut markdown_seen_ids = BTreeSet::new();
    let mut active_id: Option<String> = None;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(id) = trimmed.strip_prefix("## ") {
            if id.starts_with("FR22-") {
                assert!(
                    markdown_seen_ids.insert(id.to_string()),
                    "Failure Registry Markdown must not contain duplicate FR22 section `{id}`"
                );
                markdown_id_sequence.push(id.to_string());
                active_id = Some(id.to_string());
            } else {
                active_id = None;
            }
            continue;
        }
        let Some(id) = active_id.as_ref() else {
            continue;
        };
        if let Some(rest) = trimmed.strip_prefix("- `status`: `") {
            let (status, _) = rest.split_once('`').expect("markdown status terminator");
            markdown_status_by_id.insert(id.clone(), status.to_string());
            active_id = None;
        }
    }
    let markdown_ids = markdown_status_by_id
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();

    let mut artifact_ids = BTreeSet::new();
    for entry in fs::read_dir(sandbox_path.as_std_path()).expect("read project sandbox") {
        let entry = entry.expect("sandbox entry");
        if !entry.file_type().expect("sandbox file type").is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let parts = name.split('-').collect::<Vec<_>>();
        if parts.len() < 5 || parts[0] != "fr22" {
            continue;
        }
        if entry.path().join("source-reread-evidence.txt").is_file() {
            artifact_ids.insert(format!(
                "FR22-{}-{}-{}-{}",
                parts[1], parts[2], parts[3], parts[4]
            ));
        }
    }

    assert_eq!(
        markdown_status_by_id, json_status_by_id,
        "Failure Registry Markdown and JSON entries/statuses must be identical"
    );
    assert!(
        json_source_reread_claim_ids.is_subset(&artifact_ids),
        "Failure Registry No-grep/source-reread FR22 ids must have project_sandbox source-reread artifact ids"
    );
    assert_eq!(
        artifact_ids, json_source_reread_ref_ids,
        "Failure Registry source-reread artifact_refs must exactly match project_sandbox source-reread artifact ids"
    );
    assert_eq!(
        json_ids.iter().next_back(),
        markdown_ids.iter().next_back(),
        "latest FR22 JSON and Markdown entries must match"
    );
    assert_eq!(
        markdown_id_sequence, json_id_sequence,
        "Failure Registry Markdown FR22 section order must match canonical JSON entry order"
    );
}

#[test]
fn preflight_default_fixture_required_refs_are_unique() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");

    let mut current_fixture: Option<String> = None;
    let mut in_fixture = false;
    let mut awaiting_fixture_id_literal = false;
    let mut in_required_refs = false;
    let mut refs = Vec::new();
    let mut checked_fixture_count = 0usize;

    for line in preflight.lines() {
        let trimmed = line.trim();
        if trimmed == "PreflightFixture {" {
            in_fixture = true;
            current_fixture = None;
            awaiting_fixture_id_literal = false;
            continue;
        }
        if in_fixture && trimmed.starts_with("fixture_id:") {
            current_fixture = trimmed
                .split('"')
                .nth(1)
                .map(std::string::ToString::to_string);
            awaiting_fixture_id_literal = current_fixture.is_none();
            continue;
        }
        if awaiting_fixture_id_literal && trimmed.starts_with('"') {
            current_fixture = trimmed
                .split('"')
                .nth(1)
                .map(std::string::ToString::to_string);
            awaiting_fixture_id_literal = false;
            continue;
        }
        if in_fixture && trimmed == "required_refs: vec![" {
            in_required_refs = true;
            refs.clear();
            continue;
        }
        if in_required_refs && trimmed == "]," {
            let fixture_id = current_fixture
                .as_deref()
                .expect("PreflightFixture required_refs has fixture_id");
            let mut seen = BTreeSet::new();
            for required_ref in &refs {
                assert!(
                    seen.insert(required_ref),
                    "PreflightFixture `{fixture_id}` required_refs must not duplicate `{required_ref}`"
                );
            }
            checked_fixture_count += 1;
            in_required_refs = false;
            continue;
        }
        if in_required_refs && trimmed.starts_with('"') && trimmed.contains(".to_string()") {
            let required_ref = trimmed
                .split('"')
                .nth(1)
                .expect("required ref string literal");
            refs.push(required_ref);
        }
        if in_fixture && trimmed == "}," && !in_required_refs {
            in_fixture = false;
        }
    }

    assert!(
        checked_fixture_count > 0,
        "module guard must inspect default PreflightFixture required_refs"
    );
}

#[test]
fn preflight_verification_stable_surface_fixture_is_language_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let fixture_block = preflight
        .split(
            "PreflightFixture {\n            fixture_id: \"fixture.tool_lifecycle.verification_stable_tool_surface\"",
        )
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "PreflightFixture {\n            fixture_id: \"fixture.tool_lifecycle.authoring_stable_tool_surface\"",
            )
            .next()
        })
        .expect("verification stable tool surface fixture metadata block");

    assert!(
        fixture_block.contains("verification_command_encoding_alias"),
        "verification stable tool surface metadata must expose a language-neutral command encoding alias"
    );
    for forbidden in [
        "utf8_py_compile_command_alias",
        "python_command_as_gate_primary_key",
    ] {
        assert!(
            !fixture_block.contains(forbidden),
            "verification stable tool surface metadata must not expose language-specific alias `{forbidden}`"
        );
    }
    for docs in [
        preflight_docs.as_str(),
        runtime_contracts.as_str(),
        detailed_design.as_str(),
        item_lifecycle.as_str(),
    ] {
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                docs,
                "verification_command_encoding_alias",
            ),
            "design/testing docs must name the language-neutral verification command encoding alias"
        );
    }
}

#[test]
fn preflight_diagnostics_match_fixture_owner_authority() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let diagnostic_block = preflight
        .split("repair_target_identity_aliases_compile_exact_write_action_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("if matches!(gate.family, PreflightGateFamily::ProtocolItemLifecycle)")
                .next()
        })
        .expect("repair target alias preflight diagnostic block");

    assert!(
        diagnostic_block.contains("repair-target")
            || diagnostic_block.contains("repair target")
            || diagnostic_block.contains("action authority"),
        "repair-target alias preflight diagnostic must describe the repair-target/action-authority invariant it evaluates"
    );
    for forbidden in [
        "stale provider profile",
        "provider profile authority",
        "current closed-network LM Studio provider profile",
    ] {
        assert!(
            !diagnostic_block.contains(forbidden),
            "repair-target alias preflight diagnostic must not use stale provider-profile owner wording `{forbidden}`"
        );
    }
}

#[test]
fn preflight_protocol_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let persistence_block = preflight
        .split("fn protocol_persistence_unit_of_work_fixture_passes()")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn protocol_item_lifecycle_fixture_passes()")
                .next()
        })
        .expect("protocol persistence preflight fixture block");
    let item_lifecycle_block = preflight
        .split("fn protocol_item_lifecycle_fixture_passes()")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn state_reducer_runtime_feedback_fixture_passes()")
                .next()
        })
        .expect("protocol item lifecycle preflight fixture block");
    let combined = format!("{persistence_block}\n{item_lifecycle_block}");

    for forbidden in [
        "\"model\".to_string()",
        "\"local-model\".to_string()",
        "http://localhost:1234",
        "context_window: 8192",
        "max_output_tokens: 1024",
    ] {
        assert!(
            !combined.contains(forbidden),
            "active preflight protocol fixtures must not retain stale provider-profile authority `{forbidden}`"
        );
    }
    for required in [
        "PREFLIGHT_FIXTURE_MODEL",
        "PREFLIGHT_FIXTURE_BASE_URL",
        "PREFLIGHT_FIXTURE_CONTEXT_WINDOW",
        "PREFLIGHT_FIXTURE_MAX_OUTPUT_TOKENS",
    ] {
        assert!(
            combined.contains(required),
            "active preflight protocol fixtures must use current provider-profile constant `{required}`"
        );
    }
    for required in ["qwen/qwen3.6-35b-a3b", "http://127.0.0.1:1234", "131_072"] {
        assert!(
            preflight.contains(required),
            "active preflight provider-profile constants must define current provider-profile authority `{required}`"
        );
    }
    for required in [
        "preflight_protocol_fixture_current_provider_profile",
        "current closed-network provider profile",
    ] {
        assert!(
            preflight.contains(required)
                || preflight_docs.contains(required)
                || runtime_contracts.contains(required)
                || detailed_design.contains(required)
                || item_lifecycle.contains(required),
            "active preflight protocol fixture current-provider-profile contract must expose `{required}`"
        );
    }
}

#[test]
fn preflight_prompt_replay_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let internal_control_block = preflight
        .split("fn prompt_replay_internal_control_items_are_not_provider_visible()")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn provider_replay_call_output_symmetry_fixture_passes()")
                .next()
        })
        .expect("prompt replay internal-control fixture block");
    let provider_replay_block = preflight
        .split("fn provider_replay_call_output_symmetry_fixture_passes()")
        .nth(1)
        .and_then(|tail| tail.split("fn write_schema_stays_provider_owned()").next())
        .expect("provider replay call/output fixture block");
    let combined = format!("{internal_control_block}\n{provider_replay_block}");

    for forbidden in [
        "\"local\".to_string()",
        "http://localhost:1234",
        "unfinished.py",
    ] {
        assert!(
            !combined.contains(forbidden),
            "active preflight prompt/provider replay fixtures must not retain stale provider or language-shaped fixture authority `{forbidden}`"
        );
    }
    for required in [
        "PREFLIGHT_FIXTURE_MODEL",
        "PREFLIGHT_FIXTURE_BASE_URL",
        "docs/unfinished-workflow.md",
    ] {
        assert!(
            combined.contains(required),
            "active preflight prompt/provider replay fixtures must use current provider-profile or workflow-neutral target surface `{required}`"
        );
    }
    for required in [
        "preflight_prompt_replay_fixture_current_provider_profile",
        "current closed-network provider profile",
    ] {
        assert!(
            preflight.contains(required)
                || preflight_docs.contains(required)
                || runtime_contracts.contains(required)
                || detailed_design.contains(required)
                || item_lifecycle.contains(required),
            "active preflight prompt/provider replay fixture current-provider-profile contract must expose `{required}`"
        );
    }
}

#[test]
fn session_transcript_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let transcript_path = repo_root.join("src").join("session").join("transcript.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");

    let transcript =
        fs::read_to_string(transcript_path.as_std_path()).expect("read session transcript");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let fixture_block = transcript
        .split("pub(crate) fn transcript_from_history_items_uses_item_sequence_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn transcript_from_history_items_current_provider_profile_fixture_passes")
                .next()
        })
        .expect("transcript sequence fixture block");

    for forbidden in ["model: \"model\"", "http://localhost:1234"] {
        assert!(
            !fixture_block.contains(forbidden),
            "session transcript projection fixture must not retain stale provider profile authority `{forbidden}`"
        );
    }
    for required_surface in [
        "qwen/qwen3.6-35b-a3b",
        "http://127.0.0.1:1234",
        "session_transcript_fixture_current_provider_profile",
        "transcript_from_history_items_current_provider_profile_fixture_passes",
    ] {
        assert!(
            transcript.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "session transcript fixture, active preflight, or design docs must contain current provider-profile surface `{required_surface}`"
        );
    }
}

#[test]
fn turn_decision_repair_required_active_work_rejects_shell_only_surface() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let turn_decision_path = repo_root.join("src").join("agent").join("turn_decision.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let turn_decision =
        fs::read_to_string(turn_decision_path.as_std_path()).expect("read turn decision module");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let fixture_block = turn_decision
        .split(
            "pub(crate) fn repair_required_active_work_rejects_shell_only_surface_fixture_passes",
        )
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn unclassified_repair_fails_closed_before_dispatch_fixture_passes",
            )
            .next()
        })
        .expect("repair-required shell-only fixture block");

    assert!(
        fixture_block.contains("diagnostic.allowed_tools == vec![\"shell\".to_string()]")
            && fixture_block.contains("repair_required_active_work_without_edit_surface")
            && fixture_block.contains("TurnDecisionWarningSeverity::Error")
            && !fixture_block.contains(
                "diagnostic.allowed_tools == vec![\"shell\".to_string()]\n        && diagnostic\n            .warnings\n            .iter()\n            .all(|warning| warning.severity != TurnDecisionWarningSeverity::Error)"
            ),
        "repair-required active work must classify shell-only verification surface as an Error diagnostic instead of accepting it as a passing shell rerun"
    );
    for required_surface in [
        "turn_decision_repair_required_edit_surface_required",
        "repair_required_active_work_rejects_shell_only_surface_fixture_passes",
        "repair_required_active_work_without_edit_surface",
    ] {
        assert!(
            turn_decision.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "turn decision, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn runtime_event_publisher_tolerates_observer_absence() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let event_bus_path = repo_root.join("src").join("runtime").join("event_bus.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");

    let event_bus = fs::read_to_string(event_bus_path.as_std_path()).expect("read event bus");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let publish_block = event_bus
        .split("impl RunEventPublisher")
        .nth(1)
        .and_then(|tail| tail.split("impl RunEventSubscriber").next())
        .expect("RunEventPublisher impl block");

    assert!(
        !publish_block.contains("map_err(|error| RuntimeError::Message(format!(\"failed to publish run event: {error}\")))"),
        "RunEventPublisher must not make observer absence a runtime failure by mapping broadcast send errors directly into RuntimeError"
    );
    for required_surface in [
        "run_event_publisher_tolerates_observer_absence_fixture_passes",
        "runtime_event_publisher_observer_absence_best_effort",
        "run_event_projection_observer_absence_not_control_plane_failure",
    ] {
        assert!(
            event_bus.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "runtime event bus, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn harness_recorder_is_harness_only_under_protocol_recording_sink() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let runtime_writer_path = repo_root
        .join("src")
        .join("harness")
        .join("runtime_writer.rs");
    let run_service_path = repo_root.join("src").join("app").join("run_service.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");

    let runtime_writer =
        fs::read_to_string(runtime_writer_path.as_std_path()).expect("read runtime writer");
    let run_service = fs::read_to_string(run_service_path.as_std_path()).expect("read run service");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");

    assert!(
        !runtime_writer.contains("SqliteProtocolEventStore")
            && !runtime_writer.contains("protocol_event_store")
            && !runtime_writer.contains("record_protocol_projection"),
        "NativeHarnessRecorder must not own protocol persistence; protocol recording belongs to ProtocolRecordingSink"
    );
    assert!(
        runtime_writer.contains("Self::start_harness_only")
            && run_service.contains("NativeHarnessRecorder::start_harness_only")
            && run_service.contains("let mut sink = ProtocolRecordingSink::new")
            && run_service.contains("&mut harness_sink"),
        "normal runtime recording must compose ProtocolRecordingSink over harness-only recording"
    );
    for required_surface in [
        "native_harness_recorder_is_harness_only_fixture_passes",
        "harness_recorder_protocol_first_sink_composition",
        "native_harness_recorder_harness_only_protocol_sink_first",
    ] {
        assert!(
            runtime_writer.contains(required_surface)
                || run_service.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "harness runtime writer, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn manual_st_closeout_repair_targets_preserve_exact_identity() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let manual_st_path = repo_root.join("src").join("harness").join("manual_st.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");

    let manual_st = fs::read_to_string(manual_st_path.as_std_path()).expect("read manual_st");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let target_mapping_block = manual_st
        .split("fn expected_artifact_for_closeout_target")
        .nth(1)
        .and_then(|tail| tail.split("fn closeout_targets_match").next())
        .expect("expected artifact target mapping block");

    assert!(
        !target_mapping_block.contains("ends_with(&format!(\"/{expected}\"))")
            && !target_mapping_block
                .contains("closeout_file_name(&normalized) == closeout_file_name(&expected)")
            && !target_mapping_block
                .contains("or_else(|| closeout_file_name(&normalized).map(str::to_string))"),
        "manual ST closeout repair-target mapping must not use suffix, basename, or synthesized basename identity"
    );
    for required_surface in [
        "manual_st_closeout_repair_targets_preserve_exact_identity_fixture_passes",
        "manual_st_closeout_exact_target_identity",
    ] {
        assert!(
            manual_st.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "manual ST closeout, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn manual_st_verification_commands_are_generic_public_commands() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let manual_st_path = repo_root.join("src").join("harness").join("manual_st.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");

    let manual_st = fs::read_to_string(manual_st_path.as_std_path()).expect("read manual_st");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let command_like_block = manual_st
        .split("fn route_verification_command_like")
        .nth(1)
        .and_then(|tail| tail.split("fn verification_commands_for_stage").next())
        .expect("route verification command predicate block");

    assert!(
        !command_like_block.contains("value.contains(\"python \")")
            && !command_like_block.contains("value.starts_with(\"cargo \")")
            && !command_like_block.contains("value.starts_with(\"uv \")"),
        "manual ST verification command parsing must not be a Python/Cargo/Uv prefix whitelist"
    );
    for required_surface in [
        "manual_st_verification_commands_are_generic_public_commands_fixture_passes",
        "manual_st_generic_verification_command_contract",
    ] {
        assert!(
            manual_st.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "manual ST parser, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn manual_st_provider_retry_exhausted_timeout_classification_has_owner_and_evidence() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let manual_st_path = repo_root.join("src").join("harness").join("manual_st.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");

    let manual_st = fs::read_to_string(manual_st_path.as_std_path()).expect("read manual_st");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let timeout_block = manual_st
        .split("fn timeout_classification_value")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn write_running_timeout_classification_for_progress")
                .next()
        })
        .expect("timeout classification block");

    assert!(
        timeout_block.contains("|| provider_stream_retry_exhausted"),
        "provider retry exhaustion must carry terminal reason evidence refs"
    );
    assert!(
        timeout_block.contains("Some(\"provider_stream_retry_exhausted\")"),
        "provider retry exhaustion must be a first-class primary timeout owner"
    );
    for required_surface in [
        "manual_st_provider_retry_exhausted_timeout_classification_fixture_passes",
        "provider_stream_retry_exhausted_timeout_owner",
    ] {
        assert!(
            manual_st.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "manual ST timeout classification, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn manual_st_closeout_and_route_fixtures_are_workflow_neutral_and_current_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let manual_st_path = repo_root.join("src").join("harness").join("manual_st.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");

    let manual_st = fs::read_to_string(manual_st_path.as_std_path()).expect("read manual_st");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");

    let closeout_fixture_block = manual_st
        .split("pub fn final_assistant_open_obligation_not_clean_closeout_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub fn closeout_artifact_roles_use_language_adapter_fixture_passes")
                .next()
        })
        .expect("manual ST closeout fixture block");
    for stale_surface in [
        "calculator.py",
        "test_calculator.py",
        "widget.py",
        "test_widget.py",
        "tool.py",
        "space_invader.py",
        "test_space_invader.py",
        "python -m unittest",
        "python -X utf8",
    ] {
        assert!(
            !closeout_fixture_block.contains(stale_surface),
            "manual ST closeout/route fixture block must not use legacy domain/language authority `{stale_surface}`"
        );
    }

    let terminal_fixture_block = manual_st
        .split("pub fn closeout_continuation_budget_is_scoped_by_failure_signature_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("#[derive(Debug, Clone, Copy, PartialEq, Eq")
                .next()
        })
        .expect("manual ST terminal continuation fixture block");
    for stale_surface in [
        "calculator.py",
        "test_calculator.py",
        "widget.py",
        "test_widget.py",
        "space_invader.py",
        "test_space_invader.py",
        "python -m unittest",
        "python -m py_compile",
    ] {
        assert!(
            !terminal_fixture_block.contains(stale_surface),
            "manual ST terminal fixture block must not use legacy domain/language authority `{stale_surface}`"
        );
    }

    let route_progress_block = manual_st
        .split("pub fn route_result_progress_fields_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub fn route_inflight_timeout_classification_owner_fixture_passes")
                .next()
        })
        .expect("manual ST route progress fixture block");
    for stale_profile in ["local-model", "http://localhost:1234"] {
        assert!(
            !route_progress_block.contains(stale_profile),
            "manual ST route progress fixtures must use current provider-profile authority, not `{stale_profile}`"
        );
    }

    for required_surface in [
        "manual_st_closeout_and_route_fixtures_are_workflow_neutral_and_current_profile_fixture_passes",
        "manual_st_closeout_route_fixture_workflow_neutral_current_profile",
    ] {
        assert!(
            manual_st.contains(required_surface)
                || preflight.contains(required_surface)
                || preflight_docs.contains(required_surface)
                || runtime_contracts.contains(required_surface)
                || detailed_design.contains(required_surface)
                || item_lifecycle.contains(required_surface),
            "manual ST fixture authority, active preflight, or design docs must contain `{required_surface}`"
        );
    }
}

#[test]
fn kanban_projects_latest_closed_fr_registry_entries() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let registry_path = workspace_root
        .join("docs")
        .join("testing")
        .join("failure-registry.json");
    let kanban_path = workspace_root.join("Kanban.md");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");

    let registry = fs::read_to_string(registry_path.as_std_path()).expect("read registry json");
    let kanban = fs::read_to_string(kanban_path.as_std_path()).expect("read Kanban");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let latest_closed = latest_closed_fr22_registry_ids(&registry, 5);

    assert_eq!(
        latest_closed.len(),
        5,
        "Failure Registry must expose enough latest closed FR22 entries to audit Kanban tracking projection"
    );
    for id in latest_closed {
        let checked_projection = format!("- [x] {id} ");
        let unchecked_projection = format!("- [ ] {id} ");
        assert!(
            kanban.contains(&checked_projection),
            "Kanban task ledger must project latest closed registry entry `{id}` as a checked task"
        );
        assert!(
            !kanban.contains(&unchecked_projection),
            "Kanban task ledger must not leave latest closed registry entry `{id}` as open work"
        );
    }

    for docs_surface in [
        &preflight_docs,
        &runtime_contracts,
        &detailed_design,
        &item_lifecycle,
    ] {
        assert!(
            docs_surface.contains("kanban_latest_closed_fr_task_projection"),
            "docs/design surfaces must describe Kanban latest closed FR projection authority"
        );
    }
}

#[test]
fn public_command_source_matching_uses_exact_target_identity() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let public_command_path = repo_root
        .join("src")
        .join("agent")
        .join("public_command_contract.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");
    let public_command =
        fs::read_to_string(public_command_path.as_std_path()).expect("read public command source");
    let preflight_docs =
        fs::read_to_string(preflight_docs_path.as_std_path()).expect("read preflight docs");
    let runtime_contracts =
        fs::read_to_string(runtime_contracts_path.as_std_path()).expect("read runtime contracts");
    let detailed_design =
        fs::read_to_string(detailed_design_path.as_std_path()).expect("read detailed design");
    let item_lifecycle =
        fs::read_to_string(item_lifecycle_path.as_std_path()).expect("read item lifecycle");
    let source_match_block = public_command
        .split("fn public_command_subject_matches_source")
        .nth(1)
        .and_then(|tail| {
            tail.split("fn output_observation_alternatives_from_context")
                .next()
        })
        .expect("public command subject/source matching block");

    assert!(
        moyai::agent::public_command_contract::public_command_source_match_exact_target_identity_fixture_passes(),
        "public command source relevance must reject same-stem sibling targets and accept only exact normalized target identity"
    );
    assert!(
        !public_command.contains("fn public_command_subject_stem")
            && !source_match_block.contains("public_command_subject_stem")
            && !source_match_block.contains(".file_stem()"),
        "public command source relevance must not use stem/basename authority"
    );

    for docs_surface in [
        &preflight_docs,
        &runtime_contracts,
        &detailed_design,
        &item_lifecycle,
    ] {
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                docs_surface,
                "public_command_source_match_exact_target_identity",
            ),
            "docs/design surfaces must project public command exact source identity marker"
        );
    }
}

#[test]
fn tui_config_global_save_avoids_pre_remove_destructive_window() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let config_editor_path = repo_root.join("src").join("tui").join("config_editor.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");

    let config_editor =
        fs::read_to_string(config_editor_path.as_std_path()).expect("read TUI config editor");
    assert!(
        !config_editor.contains("remove_file(path)"),
        "TUI global config save must not pre-remove the existing destination before temp-file commit"
    );
    assert!(
        config_editor.contains("persist_config_tempfile")
            && config_editor.contains("NamedTempFile")
            && config_editor.contains(".persist(path"),
        "TUI global config save must use an explicit temp-file persist helper for destination commit"
    );

    for docs_path in [
        preflight_docs_path,
        runtime_contracts_path,
        detailed_design_path,
        item_lifecycle_path,
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs.contains("tui_config_global_save_atomic_commit"),
            "docs/design surfaces must describe TUI global config save atomic commit authority"
        );
    }
}

#[test]
fn tui_query_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let query_path = repo_root.join("src").join("tui").join("query.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");

    let query = fs::read_to_string(query_path.as_std_path()).expect("read TUI query source");
    for stale_profile in ["\"local\"", "http://localhost:1234"] {
        assert!(
            !query.contains(stale_profile),
            "TUI query fixtures must use current provider-profile authority, not `{stale_profile}`"
        );
    }
    for required_profile in [
        CURRENT_PROVIDER_PROFILE_PROVIDER,
        CURRENT_PROVIDER_PROFILE_MODEL,
        CURRENT_PROVIDER_PROFILE_BASE_URL,
    ] {
        assert!(
            query.contains(required_profile),
            "TUI query fixtures must expose current provider profile `{required_profile}`"
        );
    }

    for docs_path in [
        preflight_docs_path,
        runtime_contracts_path,
        detailed_design_path,
        item_lifecycle_path,
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "tui_query_fixture_current_provider_profile",
            ),
            "docs/design surfaces must describe TUI query current provider profile fixture authority"
        );
    }
}

#[test]
fn cli_entrypoint_artifact_outputs_use_atomic_commit() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let main_path = repo_root.join("src").join("main.rs");
    let preflight_docs_path = workspace_root
        .join("docs")
        .join("testing")
        .join("PreflightGateSuite.md");
    let runtime_contracts_path = workspace_root
        .join("docs")
        .join("design")
        .join("runtime-contracts.md");
    let detailed_design_path = workspace_root
        .join("docs")
        .join("design")
        .join("detailed-design.md");
    let item_lifecycle_path = workspace_root
        .join("docs")
        .join("design")
        .join("itemlifecycle-detail-design.md");

    let main_source = fs::read_to_string(main_path.as_std_path()).expect("read CLI main source");
    assert!(
        !main_source.contains("std::fs::write("),
        "CLI entrypoint artifact outputs must not directly truncate/write destination paths"
    );
    assert!(
        main_source.contains("write_cli_artifact_atomic")
            && main_source.contains("NamedTempFile")
            && main_source.contains(".persist(path"),
        "CLI entrypoint artifact outputs must use a temp-file persist helper"
    );

    for docs_path in [
        preflight_docs_path,
        runtime_contracts_path,
        detailed_design_path,
        item_lifecycle_path,
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "cli_entrypoint_artifact_atomic_commit",
            ),
            "docs/design surfaces must describe CLI entrypoint artifact atomic commit authority"
        );
    }
}

#[test]
fn staged_task_docs_output_target_matching_uses_exact_identity() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let staged_docs_path = repo_root
        .join("src")
        .join("agent")
        .join("staged_task_docs.rs");
    let staged_docs =
        fs::read_to_string(staged_docs_path.as_std_path()).expect("read staged docs source");

    assert!(
        !staged_docs.contains("ends_with(&format!(\"/{normalized_required}\")")
            && !staged_docs.contains("ends_with(&format!(\"/{normalized_target}\")"),
        "staged documentation output target identity must not admit suffix-equivalent paths"
    );
    assert!(
        staged_docs.contains("normalized_target == normalized_required"),
        "staged documentation output target matching must preserve exact normalized identity"
    );

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "staged_docs_output_exact_target_identity",
            ),
            "docs/design surfaces must describe staged docs output exact target identity"
        );
    }
}

#[test]
fn compaction_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let compaction_path = repo_root.join("src").join("agent").join("compaction.rs");
    let compaction =
        fs::read_to_string(compaction_path.as_std_path()).expect("read compaction source");

    for stale_profile in ["\"test-model\"", "http://localhost:1234"] {
        assert!(
            !compaction.contains(stale_profile),
            "compaction fixtures must use current provider-profile authority, not `{stale_profile}`"
        );
    }
    assert!(
        compaction.contains(CURRENT_PROVIDER_PROFILE_MODEL)
            && compaction.contains(CURRENT_PROVIDER_PROFILE_BASE_URL)
            && compaction.contains("COMPACTION_FIXTURE_MODEL")
            && compaction.contains("COMPACTION_FIXTURE_BASE_URL"),
        "compaction fixtures must expose current provider profile constants"
    );

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "compaction_fixture_current_provider_profile",
            ),
            "docs/design surfaces must describe compaction current provider profile fixture authority"
        );
    }
}

#[test]
fn state_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state source");

    for stale_profile in [
        "model: \"local\".to_string()",
        "base_url: \"http://localhost:1234\".to_string()",
    ] {
        assert!(
            !state.contains(stale_profile),
            "state fixtures must use current provider-profile authority, not `{stale_profile}`"
        );
    }
    assert!(
        state.contains(CURRENT_PROVIDER_PROFILE_MODEL)
            && state.contains(CURRENT_PROVIDER_PROFILE_BASE_URL)
            && state.contains("STATE_FIXTURE_MODEL")
            && state.contains("STATE_FIXTURE_BASE_URL"),
        "state fixtures must expose current provider profile constants"
    );

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs.contains("state_fixture_current_provider_profile"),
            "docs/design surfaces must describe state current provider profile fixture authority"
        );
    }
}

#[test]
fn state_verification_diagnostic_label_fixture_is_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state source");
    let fixture_block = state
        .split("pub(crate) fn verification_failure_diagnostic_labels_do_not_become_repair_targets_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn synthetic_tool_feedback_preserves_real_verification_cluster_fixture_passes",
            )
            .next()
        })
        .expect("state verification diagnostic-label fixture block");

    assert!(
        fixture_block.contains("active_targets")
            && fixture_block.contains("contains(\"workflow_contract.required_transition\")"),
        "state diagnostic-label fixture must still prove diagnostic labels are not promoted to repair targets"
    );
    for stale_surface in [
        "REQ-4",
        "test_update_applies_transition_req4",
        "score increments",
        "observed: Some(\"0\"",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "state diagnostic-label fixture must not retain stale test/requirement/domain authority `{stale_surface}`"
        );
    }
    for required_surface in [
        "workflow_contract.required_transition",
        "workflow_transition_assertion_label",
        "src/workflow.rs",
        "tests/workflow.behavior.md",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "state diagnostic-label fixture must expose workflow-neutral authority `{required_surface}`"
        );
    }

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "state_verification_diagnostic_label_fixture_workflow_neutral",
            ),
            "docs/design surfaces must describe state verification diagnostic-label workflow-neutral fixture authority"
        );
    }
}

#[test]
fn state_generated_test_repair_label_fixtures_are_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("agent").join("state.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state source");
    let fixture_block = state
        .split("pub(crate) fn source_owned_repair_active_work_excludes_generated_test_evidence_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn generated_test_local_binding_contradiction_active_work_fixture_passes",
            )
            .next()
        })
        .expect("state generated-test/source-owned repair fixture block");

    for stale_surface in [
        "test_cli_incomplete_binary",
        "test_cli_unknown_function",
        "test_generated_helper_executes",
        "test_public_output",
        "test_main_guard",
        "\"API-5\"",
        "\"BEH-5\"",
    ] {
        assert!(
            !fixture_block.contains(stale_surface),
            "state generated-test/source-owned repair fixtures must not retain stale label or numbered requirement authority `{stale_surface}`"
        );
    }
    for required_surface in [
        "workflow_public_command_incomplete_invocation",
        "workflow_generated_helper_executes",
        "workflow_public_output",
        "workflow_generated_reflection_subject",
        "workflow_contract.timeout_bound",
        "workflow_contract.public_behavior",
    ] {
        assert!(
            fixture_block.contains(required_surface),
            "state generated-test/source-owned repair fixtures must expose workflow-neutral label/contract authority `{required_surface}`"
        );
    }

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "state_generated_test_repair_label_fixture_workflow_neutral",
            ),
            "docs/design surfaces must describe state generated-test repair-label workflow-neutral fixture authority"
        );
    }
}

#[test]
fn contract_reconciliation_owner_classification_ignores_raw_observed_exception_text() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let contract_path = repo_root
        .join("src")
        .join("agent")
        .join("contract_reconciliation.rs");
    let source = fs::read_to_string(contract_path.as_std_path())
        .expect("read contract reconciliation source");

    for raw_authority in [
        ".chain(evidence.observed.iter())",
        ".chain(evidence.exception.iter())",
        ".chain(cluster.primary_failure.iter())",
    ] {
        assert!(
            !source.contains(raw_authority),
            "contract reconciliation owner classification must not use raw observed/exception/primary failure text authority `{raw_authority}`"
        );
    }
    for typed_authority in [
        "evidence.subtype.as_deref()",
        "evidence.evidence_markers",
        "evidence.source_refs",
        "evidence.test_refs",
    ] {
        assert!(
            source.contains(typed_authority),
            "contract reconciliation must keep typed evidence authority `{typed_authority}`"
        );
    }

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "contract_reconciliation_typed_evidence_marker_authority",
            ),
            "docs/design surfaces must describe contract reconciliation typed evidence marker authority"
        );
    }
}

#[test]
fn prompt_content_shape_adapter_fixture_is_workflow_neutral() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_path = repo_root.join("src").join("agent").join("prompt.rs");
    let prompt = fs::read_to_string(prompt_path.as_std_path()).expect("read prompt source");
    let adapter_fixture_block = prompt
        .split("pub(crate) fn prompt_content_shape_projection_uses_adapter_contract_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn python_source_content_shape_repair_projection_carries_positive_contract")
                .next()
        })
        .expect("prompt content-shape adapter fixture block");

    for forbidden in ["test_widget.py", "widget.test.ts", "widget"] {
        assert!(
            !adapter_fixture_block.contains(forbidden),
            "generic prompt content-shape adapter fixture must not use widget-domain surface `{forbidden}` as contract authority"
        );
    }
    for required in ["tests/test_workflow.py", "tests/workflow.test.ts"] {
        assert!(
            adapter_fixture_block.contains(required),
            "generic prompt content-shape adapter fixture must use workflow-neutral target `{required}`"
        );
    }

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "prompt_content_shape_adapter_fixture_workflow_neutral",
            ),
            "docs/design surfaces must describe prompt content-shape adapter workflow-neutral fixture authority"
        );
    }
}

#[test]
fn prompt_workspace_root_fixture_uses_invariant_key() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let prompt_path = repo_root.join("src").join("agent").join("prompt.rs");
    let prompt = fs::read_to_string(prompt_path.as_std_path()).expect("read prompt source");
    let workspace_fixture_block = prompt
        .split("pub(crate) fn prompt_projection_workspace_root_uses_typed_runtime_input_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn prompt_messages_contain_compaction_replay_reminder").next())
        .expect("prompt workspace-root fixture block");

    for forbidden in ["moyai-fr92", "fr92"] {
        assert!(
            !workspace_fixture_block.contains(forbidden),
            "generic prompt workspace-root fixture must not use FR-id fixture key `{forbidden}`"
        );
    }
    for required in ["moyai-typed-runtime-root-", "moyai-legacy-transcript-root-"] {
        assert!(
            workspace_fixture_block.contains(required),
            "generic prompt workspace-root fixture must use invariant fixture key `{required}`"
        );
    }

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "prompt_workspace_root_fixture_invariant_key",
            ),
            "docs/design surfaces must describe prompt workspace-root fixture invariant key authority"
        );
    }
}

#[test]
fn loop_terminal_accounting_fixture_uses_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let terminal_accounting_block = loop_impl
        .split("pub(crate) fn terminal_token_accounting_sequence_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("async fn interrupt_turn").next())
        .expect("loop terminal token accounting fixture block");

    for stale_profile in [
        "model: \"model\".to_string()",
        "base_url: \"http://localhost:1234\".to_string()",
        "AssistantMessageMeta {\n                            model: \"model\".to_string()",
        "\"http://localhost:1234\"",
    ] {
        assert!(
            !terminal_accounting_block.contains(stale_profile),
            "loop terminal accounting fixture must not use stale provider profile surface `{stale_profile}`"
        );
    }
    for required in [
        "LOOP_FIXTURE_MODEL.to_string()",
        "LOOP_FIXTURE_BASE_URL.to_string()",
        "LOOP_FIXTURE_MODEL",
        "LOOP_FIXTURE_BASE_URL",
    ] {
        assert!(
            terminal_accounting_block.contains(required),
            "loop terminal accounting fixture must use current provider constant `{required}`"
        );
    }

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "loop_terminal_accounting_fixture_current_provider_profile",
            ),
            "docs/design surfaces must describe loop terminal accounting current provider profile authority"
        );
    }
}

#[test]
fn loop_request_diagnostics_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let current_request_diagnostics_block = loop_impl
        .split("pub(crate) fn request_diagnostics_stream_retry_policy_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn request_diagnostics_missing_model_capabilities_remain_absent_fixture_passes",
            )
            .next()
        })
        .expect("current loop request diagnostics fixture block");

    for stale_profile in [
        "\"local-model\".to_string()",
        "\"http://localhost:8110\".to_string()",
    ] {
        assert!(
            !current_request_diagnostics_block.contains(stale_profile),
            "current loop request diagnostics fixtures must not use stale provider profile surface `{stale_profile}`"
        );
    }
    for required in [
        "LOOP_FIXTURE_MODEL.to_string()",
        "LOOP_FIXTURE_BASE_URL.to_string()",
    ] {
        assert!(
            current_request_diagnostics_block.contains(required),
            "current loop request diagnostics fixtures must use current provider constant `{required}`"
        );
    }

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "loop_request_diagnostics_fixture_current_provider_profile",
            ),
            "docs/design surfaces must describe loop request diagnostics current provider profile authority"
        );
    }
}

#[test]
fn loop_request_diagnostics_parallel_fixture_uses_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let parallel_diagnostics_block = loop_impl
        .split("pub(crate) fn request_diagnostics_parallel_tool_calls_scope_matches_chat_request_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn provider_messages_for_dispatch_control").next())
        .expect("loop request diagnostics parallel-tool-call fixture block");

    for stale_profile in [
        "\"local-model\".to_string()",
        "\"http://localhost:8110\".to_string()",
    ] {
        assert!(
            !parallel_diagnostics_block.contains(stale_profile),
            "loop request diagnostics parallel fixture must not use stale provider profile surface `{stale_profile}`"
        );
    }
    for required in [
        "LOOP_FIXTURE_MODEL.to_string()",
        "LOOP_FIXTURE_BASE_URL.to_string()",
    ] {
        assert!(
            parallel_diagnostics_block.contains(required),
            "loop request diagnostics parallel fixture must use current provider constant `{required}`"
        );
    }

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "loop_request_diagnostics_parallel_fixture_current_provider_profile",
            ),
            "docs/design surfaces must describe loop request diagnostics parallel fixture current provider profile authority"
        );
    }
}

#[test]
fn loop_consumed_image_request_diagnostics_fixture_uses_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let consumed_image_diagnostics_block = loop_impl
        .split("pub(crate) fn provider_chat_request_omits_consumed_images_fixture_passes")
        .nth(1)
        .and_then(|tail| tail.split("fn sandbox_profile_for_access_mode").next())
        .expect("loop consumed-image request diagnostics fixture block");

    for stale_profile in [
        "\"local-model\".to_string()",
        "\"http://localhost:1234\".to_string()",
    ] {
        assert!(
            !consumed_image_diagnostics_block.contains(stale_profile),
            "loop consumed-image request diagnostics fixture must not use stale provider profile surface `{stale_profile}`"
        );
    }
    for required in [
        "LOOP_FIXTURE_MODEL.to_string()",
        "LOOP_FIXTURE_BASE_URL.to_string()",
    ] {
        assert!(
            consumed_image_diagnostics_block.contains(required),
            "loop consumed-image request diagnostics fixture must use current provider constant `{required}`"
        );
    }

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "loop_consumed_image_request_diagnostics_fixture_current_provider_profile",
            ),
            "docs/design surfaces must describe loop consumed-image request diagnostics current provider profile authority"
        );
    }
}

#[test]
fn loop_language_neutral_fixture_helper_uses_invariant_key() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");

    assert!(
        !loop_impl.contains("loop_impl_fr38_language_neutral_runtime_fixture_refs"),
        "loop language-neutral runtime fixture helper must not use historical FR id as primary key"
    );
    assert!(
        loop_impl.contains("loop_impl_language_neutral_runtime_fixture_refs"),
        "loop language-neutral runtime fixture helper must use invariant helper vocabulary"
    );

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "loop_language_neutral_fixture_helper_invariant_key",
            ),
            "docs/design surfaces must describe loop language-neutral helper invariant-key authority"
        );
    }
}

#[test]
fn loop_repair_grounding_fixtures_use_language_neutral_failure_labels() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let repair_grounding_block = loop_impl
        .split("pub(crate) fn failed_patch_context_mismatch_reopens_target_grounding_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn docs_route_supporting_context_budget_exhaustion_is_recoverable_fixture_passes",
            )
            .next()
        })
        .expect("loop repair grounding fixture block");

    for stale_label in [
        "\"test_workflow\"",
        "\"test_public_stdout\"",
        "self.assertIn",
    ] {
        assert!(
            !repair_grounding_block.contains(stale_label),
            "loop repair grounding fixtures must not use framework-specific label/call-site authority `{stale_label}`"
        );
    }
    for required in [
        "workflow_source_parse_contract",
        "workflow_public_output_contract",
    ] {
        assert!(
            repair_grounding_block.contains(required),
            "loop repair grounding fixtures must use language-neutral evidence `{required}`"
        );
    }
    assert!(
        repair_grounding_block.contains("public_output_contains(stdout")
            && repair_grounding_block.contains("expected token"),
        "loop repair grounding fixtures must keep the public-output call-site as language-neutral typed evidence"
    );

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "loop_repair_grounding_fixture_language_neutral_failure_labels",
            ),
            "docs/design surfaces must describe loop repair grounding language-neutral failure label authority"
        );
    }
}

#[test]
fn loop_runtime_owned_verification_fixtures_use_language_neutral_labels() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let runtime_verification_block = loop_impl
        .split(
            "pub(crate) fn repair_active_shell_probe_uses_repair_target_authority_fixture_passes",
        )
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn active_authoring_rejects_wrong_target_fixture_passes")
                .next()
        })
        .expect("loop runtime-owned verification fixture block");

    for stale_label in [
        "\"test_workflow_cli\"",
        "\"test_workflow_verification\"",
        "\"test_workflow\"",
        "\"test_workflow_cli_contract\"",
        "\"test_workflow_file_contract\"",
    ] {
        assert!(
            !runtime_verification_block.contains(stale_label),
            "loop runtime-owned verification fixtures must not use framework-specific failing label authority `{stale_label}`"
        );
    }
    for required in [
        "workflow_public_output_contract",
        "workflow_behavior_verification_contract",
        "workflow_repair_behavior_contract",
        "workflow_cli_contract",
        "workflow_file_output_contract",
    ] {
        assert!(
            runtime_verification_block.contains(required),
            "loop runtime-owned verification fixtures must use language-neutral evidence `{required}`"
        );
    }

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "loop_runtime_owned_verification_fixture_language_neutral_labels",
            ),
            "docs/design surfaces must describe loop runtime-owned verification language-neutral label authority"
        );
    }
}

#[test]
fn loop_source_owned_repair_fixture_uses_language_neutral_labels() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let source_owned_repair_block = loop_impl
        .split("pub(crate) fn verification_repair_rejects_non_exact_write_target_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn docs_route_rejects_completed_deliverable_regression_fixture_passes",
            )
            .next()
        })
        .expect("loop source-owned repair fixture block");

    assert!(
        !source_owned_repair_block.contains("\"test_workflow_verification\""),
        "loop source-owned repair fixtures must not use test-method-shaped failing label authority"
    );
    assert!(
        source_owned_repair_block.contains("workflow_source_operation_contract"),
        "loop source-owned repair fixtures must use workflow-neutral source-operation contract evidence"
    );

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "loop_source_owned_repair_fixture_language_neutral_labels",
            ),
            "docs/design surfaces must describe loop source-owned repair language-neutral label authority"
        );
    }
}

#[test]
fn loop_control_envelope_projection_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let control_projection_block = loop_impl
        .split("pub(crate) fn mixed_target_invalid_edit_recovery_projects_into_control_envelope_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn open_obligation_final_message_recovery_persists_across_no_progress_tool_fixture_passes",
            )
            .next()
        })
        .expect("loop control-envelope projection fixture block");

    for stale_profile in [
        "model: \"model\".to_string()",
        "base_url: \"http://localhost:1234\".to_string()",
    ] {
        assert!(
            !control_projection_block.contains(stale_profile),
            "loop control-envelope projection fixtures must not use stale provider profile `{stale_profile}`"
        );
    }
    for current_profile in [
        "model: LOOP_FIXTURE_MODEL.to_string()",
        "base_url: LOOP_FIXTURE_BASE_URL.to_string()",
    ] {
        assert!(
            control_projection_block.contains(current_profile),
            "loop control-envelope projection fixtures must use current provider profile constant `{current_profile}`"
        );
    }

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "loop_control_envelope_projection_fixture_current_provider_profile",
            ),
            "docs/design surfaces must describe loop control-envelope projection current provider profile authority"
        );
    }
}

#[test]
fn loop_provider_replay_fixtures_use_language_neutral_command_labels() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let loop_impl_path = repo_root.join("src").join("agent").join("loop_impl.rs");
    let loop_impl = fs::read_to_string(loop_impl_path.as_std_path()).expect("read loop impl");
    let provider_replay_block = loop_impl
        .split("pub(crate) fn provider_replay_effective_tool_surface_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn provider_replay_omits_prior_assistant_text_when_open_obligations_fixture_passes")
                .next()
        })
        .expect("loop provider replay fixture block");

    assert!(
        !provider_replay_block.contains("test_workflow"),
        "loop provider replay fixtures must not use test-method-shaped command authority"
    );
    assert!(
        provider_replay_block.contains("workflow_replay_verification_contract"),
        "loop provider replay fixtures must use workflow-neutral verification command evidence"
    );

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "loop_provider_replay_fixture_language_neutral_command_labels",
            ),
            "docs/design surfaces must describe loop provider replay language-neutral command authority"
        );
    }
}

#[test]
fn lifecycle_kernel_fixtures_use_current_provider_profile() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let lifecycle_path = repo_root
        .join("src")
        .join("agent")
        .join("lifecycle_kernel.rs");
    let lifecycle =
        fs::read_to_string(lifecycle_path.as_std_path()).expect("read lifecycle kernel");
    let fixture_block = lifecycle
        .split("fn edit_only_repair_fixture_envelope")
        .nth(1)
        .and_then(|tail| tail.split("#[cfg(test)]").next())
        .expect("lifecycle kernel fixture envelope block");

    for stale_profile in [
        "\"test-provider\"",
        "\"test-model\"",
        "\"http://localhost\"",
    ] {
        assert!(
            !fixture_block.contains(stale_profile),
            "lifecycle kernel fixture envelope must not use stale provider profile `{stale_profile}`"
        );
    }
    for current_profile in [
        "LIFECYCLE_FIXTURE_PROVIDER",
        "LIFECYCLE_FIXTURE_MODEL",
        "LIFECYCLE_FIXTURE_BASE_URL",
    ] {
        assert!(
            fixture_block.contains(current_profile),
            "lifecycle kernel fixture envelope must use current provider profile constant `{current_profile}`"
        );
    }

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "lifecycle_kernel_fixture_current_provider_profile",
            ),
            "docs/design surfaces must describe lifecycle kernel fixture current provider profile authority"
        );
    }
}

#[test]
fn structured_document_summary_skips_generated_dependency_targets() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let state_path = repo_root.join("src").join("agent").join("state.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let state = fs::read_to_string(state_path.as_std_path()).expect("read state reducer");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let collection_block = state
        .split("fn collect_structured_document_targets_recursive")
        .nth(1)
        .and_then(|tail| tail.split("fn explicit_batch_size").next())
        .expect("structured document collection block");

    assert!(
        collection_block
            .contains("structured_document_path_is_generated_or_dependency(relative.as_path())"),
        "structured document target collection must reuse generated/dependency path exclusion"
    );
    assert!(
        state.contains(
            "structured_document_summary_skips_generated_dependency_targets_fixture_passes"
        ),
        "state reducer must expose the structured document generated/dependency exclusion fixture"
    );
    assert!(
        preflight.contains("state_structured_document_summary_generated_dependency_exclusion"),
        "active preflight must include the structured document generated/dependency exclusion gate"
    );
    assert!(
        moyai::harness::preflight::structured_document_summary_generated_dependency_exclusion_fixture_passes(),
        "structured document generated/dependency exclusion preflight wrapper must execute the state reducer fixture"
    );

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "state_structured_document_summary_generated_dependency_exclusion",
            ),
            "docs/design surfaces must describe structured document generated/dependency target exclusion"
        );
    }
}

#[test]
fn protocol_store_latest_turn_position_resists_timestamp_drift() {
    let repo_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = repo_root.parent().expect("workspace root");
    let store_path = repo_root.join("src").join("protocol").join("store.rs");
    let migration_path = repo_root.join("src").join("storage").join("migration.rs");
    let preflight_path = repo_root.join("src").join("harness").join("preflight.rs");
    let store = fs::read_to_string(store_path.as_std_path()).expect("read protocol store");
    let migration =
        fs::read_to_string(migration_path.as_std_path()).expect("read migration runner");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let latest_turn_block = store
        .split("fn query_latest_protocol_turn_id")
        .nth(1)
        .and_then(|tail| tail.split("trait ProtocolSqlExecutor").next())
        .expect("protocol latest-turn query block");

    assert!(
        !latest_turn_block.contains("latest_observed_at_ms DESC"),
        "protocol latest-turn selection must not use wall-clock timestamp as primary continuation authority"
    );
    assert!(
        latest_turn_block.contains("protocol_item_append_order"),
        "protocol latest-turn selection must use event-sourced protocol append order"
    );
    assert!(
        moyai::harness::preflight::protocol_store_latest_turn_position_resists_timestamp_drift_fixture_passes(),
        "protocol store timestamp-drift fixture must choose the latest appended protocol turn"
    );
    assert!(
        preflight.contains("protocol_store_latest_turn_position_event_sourced_order"),
        "active preflight must include protocol store event-sourced latest-turn ordering"
    );
    assert!(
        migration.contains("V20_PROTOCOL_ITEM_APPEND_ORDER")
            && migration.contains("V20__protocol_item_append_order.sql")
            && migration.contains("connection.execute_batch(V20_PROTOCOL_ITEM_APPEND_ORDER)?"),
        "storage migration runner must apply protocol append-order migration"
    );

    for docs_path in [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ] {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "protocol_store_latest_turn_position_event_sourced_order",
            ),
            "docs/design surfaces must describe protocol store event-sourced latest-turn ordering"
        );
    }
}

#[test]
fn desktop_markdown_exports_use_atomic_artifact_commit() {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().expect("workspace root");
    let desktop_app_path = manifest_dir.join("src").join("desktop").join("app.rs");
    let preflight_path = manifest_dir
        .join("src")
        .join("harness")
        .join("preflight.rs");
    let docs_paths = [
        workspace_root
            .join("docs")
            .join("testing")
            .join("PreflightGateSuite.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("runtime-contracts.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("detailed-design.md"),
        workspace_root
            .join("docs")
            .join("design")
            .join("itemlifecycle-detail-design.md"),
    ];
    let desktop_app =
        fs::read_to_string(desktop_app_path.as_std_path()).expect("read desktop app source");
    let preflight = fs::read_to_string(preflight_path.as_std_path()).expect("read preflight");
    let transcript_export_block = desktop_app
        .split("pub(crate) fn export_open_transcript_markdown_auto")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) fn export_selected_history_markdown_to_path")
                .next()
        })
        .expect("desktop open transcript export block");
    let history_export_block = desktop_app
        .split("fn spawn_history_markdown_export")
        .nth(1)
        .and_then(|tail| tail.split("fn spawn_session_load").next())
        .expect("desktop history export block");

    for (name, block) in [
        ("open transcript", transcript_export_block),
        ("canonical history", history_export_block),
    ] {
        assert!(
            block.contains("write_markdown_export_atomic("),
            "desktop {name} Markdown export must use Desktop-owned atomic artifact commit helper"
        );
        assert!(
            !block.contains("std::fs::write("),
            "desktop {name} Markdown export must not directly write GUI evidence artifacts"
        );
    }
    assert!(
        desktop_app.contains("fn write_markdown_export_atomic("),
        "desktop app must define a same-directory atomic Markdown export commit helper"
    );
    assert!(
        preflight.contains("desktop_markdown_export_atomic_commit"),
        "active preflight must carry Desktop Markdown export atomic commit marker"
    );

    for docs_path in docs_paths {
        let docs = fs::read_to_string(docs_path.as_std_path()).expect("read docs/design surface");
        assert!(
            docs_contains_or_item_lifecycle_current_authority(
                &docs,
                "desktop_markdown_export_atomic_commit",
            ),
            "docs/design surfaces must describe Desktop Markdown export atomic commit authority"
        );
    }
}

fn latest_closed_fr22_registry_ids(registry: &str, limit: usize) -> Vec<String> {
    let mut current_id: Option<String> = None;
    let mut closed_ids = Vec::new();

    for line in registry.lines() {
        let trimmed = line.trim();
        if let Some(id) = json_string_field(trimmed, "id") {
            current_id = id.starts_with("FR22-").then_some(id);
            continue;
        }
        if let Some(status) = json_string_field(trimmed, "status") {
            if status == "root_fix_verified_rerun_skipped_per_user_instruction" {
                if let Some(id) = current_id.take() {
                    closed_ids.push(id);
                }
            }
        }
    }

    let start = closed_ids.len().saturating_sub(limit);
    closed_ids.into_iter().skip(start).collect()
}

fn json_string_field(line: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\": \"");
    let start = line.find(&needle)? + needle.len();
    let tail = &line[start..];
    let end = tail.find('"')?;
    Some(tail[..end].to_string())
}

#[test]
fn desktop_query_todo_status_projection_uses_typed_enum() {
    let desktop_query = fs::read_to_string("src/desktop/query.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/desktop/query.rs"))
        .expect("desktop query source should be readable");
    let preflight = fs::read_to_string("../docs/testing/PreflightGateSuite.md")
        .or_else(|_| fs::read_to_string("docs/testing/PreflightGateSuite.md"))
        .expect("preflight docs should be readable");
    let runtime_contracts = fs::read_to_string("../docs/design/runtime-contracts.md")
        .or_else(|_| fs::read_to_string("docs/design/runtime-contracts.md"))
        .expect("runtime contracts should be readable");
    let detailed_design = fs::read_to_string("../docs/design/detailed-design.md")
        .or_else(|_| fs::read_to_string("docs/design/detailed-design.md"))
        .expect("detailed design should be readable");

    assert!(
        !desktop_query.contains("format!(\"{:?}\", todo.status)"),
        "Desktop todo projection must not use Debug-string todo.status authority"
    );
    assert!(
        desktop_query.contains("matches!(todo.status, TodoStatus::Completed)"),
        "completed todo counting should match the typed TodoStatus enum"
    );
    assert!(
        desktop_query.contains("matches!(todo.status, TodoStatus::Blocked)"),
        "blocked todo counting should match the typed TodoStatus enum"
    );
    assert!(
        desktop_query.contains("fn todo_status_label(status: TodoStatus)"),
        "todo status labels should accept typed TodoStatus"
    );
    assert!(
        !desktop_query.contains("fn todo_status_label(status: &str)"),
        "todo status labels must not accept Debug-string status"
    );
    assert!(preflight.contains("desktop_query_todo_status_typed_projection"));
    assert!(runtime_contracts.contains("desktop_query_todo_status_typed_projection"));
    assert!(detailed_design.contains("desktop_query_todo_status_typed_projection"));
}

#[test]
fn desktop_web_access_mode_projection_uses_typed_enum() {
    let desktop_web_model = fs::read_to_string("src/desktop/web_model.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/desktop/web_model.rs"))
        .expect("desktop web model source should be readable");
    let preflight = fs::read_to_string("../docs/testing/PreflightGateSuite.md")
        .or_else(|_| fs::read_to_string("docs/testing/PreflightGateSuite.md"))
        .expect("preflight docs should be readable");
    let runtime_contracts = fs::read_to_string("../docs/design/runtime-contracts.md")
        .or_else(|_| fs::read_to_string("docs/design/runtime-contracts.md"))
        .expect("runtime contracts should be readable");
    let detailed_design = fs::read_to_string("../docs/design/detailed-design.md")
        .or_else(|_| fs::read_to_string("docs/design/detailed-design.md"))
        .expect("detailed design should be readable");

    assert!(
        !desktop_web_model.contains("format!(\n            \"{:?}\",\n            state\n                .provider_config\n                .effective_config\n                .permissions\n                .access_mode"),
        "Desktop web access label must not use Debug-string access_mode authority"
    );
    assert!(
        desktop_web_model.contains("fn access_mode_key(access: AccessMode)"),
        "Desktop web access label should accept typed AccessMode"
    );
    assert!(
        desktop_web_model.contains("AccessMode::FullAccess => \"full_access\""),
        "Desktop web access label should match typed AccessMode variants"
    );
    assert!(
        desktop_web_model.contains("desktop_web_access_mode_typed_projection_fixture_passes"),
        "Desktop web access projection should have executable fixture coverage"
    );
    assert!(preflight.contains("desktop_web_access_mode_typed_projection"));
    assert!(runtime_contracts.contains("desktop_web_access_mode_typed_projection"));
    assert!(detailed_design.contains("desktop_web_access_mode_typed_projection"));
}

#[test]
fn desktop_file_change_action_projection_uses_typed_change_kind() {
    let desktop_artifact_projection = fs::read_to_string("src/desktop/artifact_projection.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/desktop/artifact_projection.rs"))
        .expect("desktop artifact projection source should be readable");
    let desktop_models = fs::read_to_string("src/desktop/models.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/desktop/models.rs"))
        .expect("desktop models source should be readable");
    let preflight = fs::read_to_string("../docs/testing/PreflightGateSuite.md")
        .or_else(|_| fs::read_to_string("docs/testing/PreflightGateSuite.md"))
        .expect("preflight docs should be readable");
    let runtime_contracts = fs::read_to_string("../docs/design/runtime-contracts.md")
        .or_else(|_| fs::read_to_string("docs/design/runtime-contracts.md"))
        .expect("runtime contracts should be readable");
    let detailed_design = fs::read_to_string("../docs/design/detailed-design.md")
        .or_else(|_| fs::read_to_string("docs/design/detailed-design.md"))
        .expect("detailed design should be readable");

    assert!(
        desktop_models.contains("pub kind: ChangeKind"),
        "DesktopFileChangeRow must preserve typed ChangeKind authority"
    );
    assert!(
        desktop_artifact_projection.contains("fn merged_file_change_kind("),
        "file-change dedupe precedence should merge typed ChangeKind values"
    );
    assert!(
        !desktop_artifact_projection
            .contains("fn merged_file_change_action(existing: &str, incoming: &str)"),
        "file-change dedupe precedence must not merge display action labels"
    );
    assert!(
        !desktop_artifact_projection.contains("row.action == \"追加\""),
        "file-change summary counts must not use visible action labels as lifecycle authority"
    );
    assert!(
        desktop_artifact_projection
            .contains("desktop_file_change_action_typed_projection_fixture_passes"),
        "Desktop file-change action projection should have executable fixture coverage"
    );
    assert!(preflight.contains("desktop_file_change_action_typed_projection"));
    assert!(runtime_contracts.contains("desktop_file_change_action_typed_projection"));
    assert!(detailed_design.contains("desktop_file_change_action_typed_projection"));
}

#[test]
fn desktop_session_row_status_projection_uses_typed_status() {
    let desktop_models = fs::read_to_string("src/desktop/models.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/desktop/models.rs"))
        .expect("desktop models source should be readable");
    let preflight = fs::read_to_string("../docs/testing/PreflightGateSuite.md")
        .or_else(|_| fs::read_to_string("docs/testing/PreflightGateSuite.md"))
        .expect("preflight docs should be readable");
    let runtime_contracts = fs::read_to_string("../docs/design/runtime-contracts.md")
        .or_else(|_| fs::read_to_string("docs/design/runtime-contracts.md"))
        .expect("runtime contracts should be readable");
    let detailed_design = fs::read_to_string("../docs/design/detailed-design.md")
        .or_else(|_| fs::read_to_string("docs/design/detailed-design.md"))
        .expect("detailed design should be readable");

    assert!(
        desktop_models.contains("pub status_kind: SessionStatus"),
        "DesktopSessionRow must preserve typed SessionStatus authority"
    );
    assert!(
        desktop_models.contains("let status = self.status_kind;"),
        "title-preserving session row updates should read typed SessionStatus"
    );
    assert!(
        !desktop_models
            .contains("session_status_from_key(&self.status).unwrap_or(SessionStatus::Running)"),
        "DesktopSessionRow must not recover lifecycle status from visible status text"
    );
    assert!(
        desktop_models.contains("desktop_session_row_status_typed_projection_fixture_passes"),
        "Desktop session row status projection should have executable fixture coverage"
    );
    assert!(preflight.contains("desktop_session_row_status_typed_projection"));
    assert!(runtime_contracts.contains("desktop_session_row_status_typed_projection"));
    assert!(detailed_design.contains("desktop_session_row_status_typed_projection"));
}

#[test]
fn desktop_transcript_row_kind_projection_uses_typed_enum() {
    let desktop_models = fs::read_to_string("src/desktop/models.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/desktop/models.rs"))
        .expect("desktop models source should be readable");
    let desktop_open_session = fs::read_to_string("src/desktop/open_session.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/desktop/open_session.rs"))
        .expect("desktop open_session source should be readable");
    let desktop_query = fs::read_to_string("src/desktop/query.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/desktop/query.rs"))
        .expect("desktop query source should be readable");
    let preflight = fs::read_to_string("../docs/testing/PreflightGateSuite.md")
        .or_else(|_| fs::read_to_string("docs/testing/PreflightGateSuite.md"))
        .expect("preflight docs should be readable");
    let runtime_contracts = fs::read_to_string("../docs/design/runtime-contracts.md")
        .or_else(|_| fs::read_to_string("docs/design/runtime-contracts.md"))
        .expect("runtime contracts should be readable");
    let detailed_design = fs::read_to_string("../docs/design/detailed-design.md")
        .or_else(|_| fs::read_to_string("docs/design/detailed-design.md"))
        .expect("detailed design should be readable");

    assert!(
        desktop_models.contains("pub enum DesktopTranscriptRowKind"),
        "Desktop transcript rows must preserve typed row kind authority"
    );
    assert!(
        desktop_models.contains("pub row_kind: DesktopTranscriptRowKind"),
        "DesktopTranscriptRow must carry typed row kind authority next to the visible key"
    );
    assert!(
        desktop_open_session.contains("row.row_kind == DesktopTranscriptRowKind::FileChanges"),
        "stored/live transcript preservation must read typed row kind, not visible kind text"
    );
    assert!(
        desktop_query.contains("desktop_transcript_row_kind_typed_projection_fixture_passes"),
        "Desktop transcript row kind projection should have executable fixture coverage"
    );
    assert!(preflight.contains("desktop_transcript_row_kind_typed_projection"));
    assert!(runtime_contracts.contains("desktop_transcript_row_kind_typed_projection"));
    assert!(detailed_design.contains("desktop_transcript_row_kind_typed_projection"));
}

#[test]
fn desktop_preferences_save_uses_atomic_tempfile_commit() {
    let desktop_preferences = fs::read_to_string("src/desktop/preferences.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/desktop/preferences.rs"))
        .expect("desktop preferences source should be readable");
    let preflight = fs::read_to_string("../docs/testing/PreflightGateSuite.md")
        .or_else(|_| fs::read_to_string("docs/testing/PreflightGateSuite.md"))
        .expect("preflight docs should be readable");
    let runtime_contracts = fs::read_to_string("../docs/design/runtime-contracts.md")
        .or_else(|_| fs::read_to_string("docs/design/runtime-contracts.md"))
        .expect("runtime contracts should be readable");
    let detailed_design = fs::read_to_string("../docs/design/detailed-design.md")
        .or_else(|_| fs::read_to_string("docs/design/detailed-design.md"))
        .expect("detailed design should be readable");

    assert!(
        desktop_preferences.contains("NamedTempFile::new_in"),
        "Desktop preferences save should use a unique same-directory temporary file"
    );
    assert!(
        desktop_preferences.contains("sync_all()"),
        "Desktop preferences save should fsync the temporary file before commit"
    );
    assert!(
        desktop_preferences.contains(".persist(path.as_std_path())"),
        "Desktop preferences save should persist the temporary file as a single commit"
    );
    assert!(
        !desktop_preferences.contains("path.with_extension(\"tmp\")"),
        "Desktop preferences save must not use a fixed .tmp path as commit authority"
    );
    assert!(
        !desktop_preferences.contains("remove_file(&path)"),
        "Desktop preferences save must not delete the destination before commit"
    );
    assert!(
        desktop_preferences.contains("desktop_preferences_save_atomic_commit_fixture_passes"),
        "Desktop preferences save should have executable atomic-commit fixture coverage"
    );
    assert!(preflight.contains("desktop_preferences_atomic_commit"));
    assert!(runtime_contracts.contains("desktop_preferences_atomic_commit"));
    assert!(detailed_design.contains("desktop_preferences_atomic_commit"));
}

#[test]
fn app_initial_turn_context_uses_typed_route_key() {
    let app_run_service = fs::read_to_string("src/app/run_service.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/app/run_service.rs"))
        .expect("app run_service source should be readable");
    let session_state = fs::read_to_string("src/session/state.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/session/state.rs"))
        .expect("session state source should be readable");
    let preflight = fs::read_to_string("../docs/testing/PreflightGateSuite.md")
        .or_else(|_| fs::read_to_string("docs/testing/PreflightGateSuite.md"))
        .expect("preflight docs should be readable");
    let runtime_contracts = fs::read_to_string("../docs/design/runtime-contracts.md")
        .or_else(|_| fs::read_to_string("docs/design/runtime-contracts.md"))
        .expect("runtime contracts should be readable");
    let detailed_design = fs::read_to_string("../docs/design/detailed-design.md")
        .or_else(|_| fs::read_to_string("docs/design/detailed-design.md"))
        .expect("detailed design should be readable");

    assert!(
        session_state.contains("impl TaskRoute") && session_state.contains("pub fn key(self)"),
        "TaskRoute should expose a stable typed key projection"
    );
    assert!(
        app_run_service.contains("active_work_kind: Some(state.route.key().to_string())"),
        "initial TurnContext active_work_kind should use TaskRoute::key"
    );
    assert!(
        !app_run_service.contains("format!(\"{:?}\", state.route)"),
        "initial TurnContext must not use Rust Debug route spelling as lifecycle authority"
    );
    assert!(
        app_run_service.contains("app_initial_turn_route_key_projection_fixture_passes"),
        "app initial turn route projection should have executable fixture coverage"
    );
    assert!(preflight.contains("app_initial_turn_route_key_projection"));
    assert!(runtime_contracts.contains("app_initial_turn_route_key_projection"));
    assert!(detailed_design.contains("app_initial_turn_route_key_projection"));
}

#[test]
fn app_default_desktop_workspace_creation_errors_are_not_silenced() {
    let app_bootstrap = fs::read_to_string("src/app/bootstrap.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/app/bootstrap.rs"))
        .expect("app bootstrap source should be readable");
    let preflight = fs::read_to_string("../docs/testing/PreflightGateSuite.md")
        .or_else(|_| fs::read_to_string("docs/testing/PreflightGateSuite.md"))
        .expect("preflight docs should be readable");
    let runtime_contracts = fs::read_to_string("../docs/design/runtime-contracts.md")
        .or_else(|_| fs::read_to_string("docs/design/runtime-contracts.md"))
        .expect("runtime contracts should be readable");
    let detailed_design = fs::read_to_string("../docs/design/detailed-design.md")
        .or_else(|_| fs::read_to_string("docs/design/detailed-design.md"))
        .expect("detailed design should be readable");

    assert!(
        app_bootstrap.contains("default_desktop_workspace_directory()?"),
        "Desktop default workspace selection should propagate creation errors"
    );
    assert!(
        app_bootstrap.contains(
            "fn default_desktop_workspace_directory() -> Result<Option<Utf8PathBuf>, AppBootstrapError>"
        ),
        "default desktop workspace helper should expose bootstrap errors"
    );
    assert!(
        app_bootstrap.contains("std::fs::create_dir_all(path.as_std_path())?"),
        "Desktop default workspace directory creation should be fallible evidence"
    );
    assert!(
        !app_bootstrap.contains("let _ = std::fs::create_dir_all"),
        "Desktop default workspace creation must not discard filesystem errors"
    );
    assert!(
        app_bootstrap.contains("app_default_desktop_workspace_creation_fixture_passes"),
        "app default Desktop workspace creation should have executable fixture coverage"
    );
    assert!(preflight.contains("app_default_desktop_workspace_creation_error_propagation"));
    assert!(runtime_contracts.contains("app_default_desktop_workspace_creation_error_propagation"));
    assert!(detailed_design.contains("app_default_desktop_workspace_creation_error_propagation"));
}

#[test]
fn protocol_store_single_item_appends_use_atomic_append_order_commit() {
    let protocol_store = fs::read_to_string("src/protocol/store.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/protocol/store.rs"))
        .expect("protocol store source should be readable");
    let preflight = fs::read_to_string("../docs/testing/PreflightGateSuite.md")
        .or_else(|_| fs::read_to_string("docs/testing/PreflightGateSuite.md"))
        .expect("preflight docs should be readable");
    let runtime_contracts = fs::read_to_string("../docs/design/runtime-contracts.md")
        .or_else(|_| fs::read_to_string("docs/design/runtime-contracts.md"))
        .expect("runtime contracts should be readable");
    let detailed_design = fs::read_to_string("../docs/design/detailed-design.md")
        .or_else(|_| fs::read_to_string("docs/design/detailed-design.md"))
        .expect("detailed design should be readable");
    let item_lifecycle = fs::read_to_string("../docs/design/itemlifecycle-detail-design.md")
        .or_else(|_| fs::read_to_string("docs/design/itemlifecycle-detail-design.md"))
        .expect("item lifecycle detail design should be readable");

    assert!(
        protocol_store.contains(
            "pub(crate) fn protocol_store_single_item_append_order_atomic_commit_fixture_passes() -> bool"
        ),
        "protocol store single-item append/order commit invariant should have executable fixture coverage"
    );
    assert!(
        protocol_store.contains(
            "let mut connection = self.connection.lock().expect(\"sqlite mutex poisoned\");\n        let transaction = connection.transaction()?;\n        insert_runtime_event(&transaction, event)?;\n        transaction.commit()?"
        ),
        "append_runtime_event should commit runtime event and append-order authority in one transaction"
    );
    assert!(
        protocol_store.contains(
            "let mut connection = self.connection.lock().expect(\"sqlite mutex poisoned\");\n        let transaction = connection.transaction()?;\n        insert_history_item(&transaction, item)?;\n        transaction.commit()?"
        ),
        "append_history_item should commit history item and append-order authority in one transaction"
    );
    assert!(
        protocol_store.contains(
            "let mut connection = self.connection.lock().expect(\"sqlite mutex poisoned\");\n        let transaction = connection.transaction()?;\n        insert_turn_item(&transaction, item)?;\n        transaction.commit()?"
        ),
        "append_turn_item should commit turn item and append-order authority in one transaction"
    );
    assert!(preflight.contains("protocol_store_single_item_append_order_atomic_commit"));
    assert!(runtime_contracts.contains("protocol_store_single_item_append_order_atomic_commit"));
    assert!(detailed_design.contains("protocol_store_single_item_append_order_atomic_commit"));
    assert!(docs_contains_or_item_lifecycle_current_authority(
        &item_lifecycle,
        "protocol_store_single_item_append_order_atomic_commit"
    ));
}

#[test]
fn desktop_web_visibility_uses_typed_projection() {
    let web_types = fs::read_to_string("ui/desktop-web/src/types.ts")
        .or_else(|_| fs::read_to_string("moyAI/ui/desktop-web/src/types.ts"))
        .expect("Desktop Web TypeScript types should be readable");
    let web_render = fs::read_to_string("ui/desktop-web/src/render.ts")
        .or_else(|_| fs::read_to_string("moyAI/ui/desktop-web/src/render.ts"))
        .expect("Desktop Web render source should be readable");
    let desktop_web_model = fs::read_to_string("src/desktop/web_model.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/desktop/web_model.rs"))
        .expect("Desktop web model source should be readable");
    let desktop_models = fs::read_to_string("src/desktop/models.rs")
        .or_else(|_| fs::read_to_string("moyAI/src/desktop/models.rs"))
        .expect("Desktop models source should be readable");
    let preflight = fs::read_to_string("../docs/testing/PreflightGateSuite.md")
        .or_else(|_| fs::read_to_string("docs/testing/PreflightGateSuite.md"))
        .expect("preflight docs should be readable");
    let runtime_contracts = fs::read_to_string("../docs/design/runtime-contracts.md")
        .or_else(|_| fs::read_to_string("docs/design/runtime-contracts.md"))
        .expect("runtime contracts should be readable");
    let detailed_design = fs::read_to_string("../docs/design/detailed-design.md")
        .or_else(|_| fs::read_to_string("docs/design/detailed-design.md"))
        .expect("detailed design should be readable");
    let item_lifecycle = fs::read_to_string("../docs/design/itemlifecycle-detail-design.md")
        .or_else(|_| fs::read_to_string("docs/design/itemlifecycle-detail-design.md"))
        .expect("item lifecycle detail design should be readable");

    assert!(
        web_types.contains("thread_empty: boolean")
            && web_types.contains("artifact_preview_available: boolean")
            && desktop_web_model.contains("pub thread_empty: bool")
            && desktop_web_model.contains("pub artifact_preview_available: bool")
            && desktop_models.contains("pub thread_empty: bool")
            && desktop_models.contains("pub artifact_preview_available: bool"),
        "Desktop Web visibility should be projected as typed state fields across Rust and TypeScript DTOs"
    );
    assert!(
        web_render.contains("state.thread_empty")
            && web_render.contains("state.artifact_preview_available"),
        "Desktop Web render should consume typed visibility projection fields"
    );
    assert!(
        !web_render.contains("title.includes(\"チャット\")")
            && !web_render.contains("body.includes(\"下の入力欄\")")
            && !web_render.contains("artifact_preview_text.trim()")
            && !web_render.contains("artifact_preview_text.includes(\"選択されていません\")"),
        "Desktop Web render must not use localized display strings as visibility authority"
    );
    assert!(
        desktop_web_model.contains("desktop_gui_typed_visibility_projection_fixture_passes"),
        "Desktop Web visibility projection should have executable fixture coverage"
    );
    assert!(preflight.contains("desktop_gui_typed_visibility_projection"));
    assert!(runtime_contracts.contains("desktop_gui_typed_visibility_projection"));
    assert!(detailed_design.contains("desktop_gui_typed_visibility_projection"));
    assert!(docs_contains_or_item_lifecycle_current_authority(
        &item_lifecycle,
        "desktop_gui_typed_visibility_projection"
    ));
}

fn build_control_envelope(allowed_tools: Vec<ToolName>) -> moyai::protocol::TurnControlEnvelope {
    build_control_envelope_with_choice(allowed_tools, ToolChoice::Required)
}

#[test]
fn cli_human_renderer_uses_typed_lifecycle_projection() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root.parent().expect("crate has repository parent");
    let render = std::fs::read_to_string(root.join("src/cli/render.rs"))
        .expect("cli renderer source is readable");
    let session_model = std::fs::read_to_string(root.join("src/session/model.rs"))
        .expect("session model source is readable");
    let session_state = std::fs::read_to_string(root.join("src/session/state.rs"))
        .expect("session state source is readable");
    let preflight = std::fs::read_to_string(root.join("src/harness/preflight.rs"))
        .expect("preflight source is readable");
    let preflight_doc =
        std::fs::read_to_string(repo_root.join("docs/testing/PreflightGateSuite.md"))
            .expect("preflight doc is readable");
    let runtime_contracts =
        std::fs::read_to_string(repo_root.join("docs/design/runtime-contracts.md"))
            .expect("runtime contracts doc is readable");
    let detailed_design = std::fs::read_to_string(repo_root.join("docs/design/detailed-design.md"))
        .expect("detailed design doc is readable");
    let item_lifecycle =
        std::fs::read_to_string(repo_root.join("docs/design/itemlifecycle-detail-design.md"))
            .expect("item lifecycle design doc is readable");

    assert!(
        session_model.contains("impl SessionStatus")
            && session_model.contains("pub fn key(self) -> &'static str")
            && session_model.contains("Self::AwaitingUser => \"awaiting_user\"")
            && session_model.contains("impl MessageRole")
            && session_model.contains("Self::Assistant => \"assistant\"")
            && session_model.contains("impl PartKind")
            && session_model.contains("Self::RequestDiagnostics => \"request_diagnostics\""),
        "CLI human renderer lifecycle enums should expose stable typed keys"
    );
    assert!(
        session_state.contains("impl ProcessPhase")
            && session_state.contains("Self::Verify => \"verify\""),
        "ProcessPhase should expose a stable typed key for CLI state projection"
    );
    assert!(
        render.contains("state.route.key()")
            && render.contains("state.process_phase.key()")
            && render.contains("summary.status.key()")
            && render.contains("session.status.key()")
            && render.contains("message.record.role.key()")
            && render.contains("part.kind.key()")
            && render.contains("serde_json::to_string(part)"),
        "CLI human renderer should project visible lifecycle state through typed keys and canonical payload serialization"
    );
    assert!(
        !render.contains("route={:?}")
            && !render.contains("phase={:?}")
            && !render.contains("status={:?}")
            && !render.contains("{:?}:")
            && !render.contains("  {:?}"),
        "CLI human renderer must not use Rust Debug strings as visible lifecycle authority"
    );
    assert!(
        render.contains("cli_human_renderer_typed_lifecycle_projection_fixture_passes"),
        "CLI renderer should expose executable fixture coverage for typed lifecycle projection"
    );
    assert!(preflight.contains("cli_human_renderer_typed_lifecycle_projection"));
    assert!(preflight_doc.contains("cli_human_renderer_typed_lifecycle_projection"));
    assert!(runtime_contracts.contains("cli_human_renderer_typed_lifecycle_projection"));
    assert!(detailed_design.contains("cli_human_renderer_typed_lifecycle_projection"));
    assert!(docs_contains_or_item_lifecycle_current_authority(
        &item_lifecycle,
        "cli_human_renderer_typed_lifecycle_projection"
    ));
}

#[test]
fn edit_file_change_feedback_all_kinds_evidence_only() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root.parent().expect("crate has repository parent");
    let change_tracker = std::fs::read_to_string(root.join("src/edit/change_tracker.rs"))
        .expect("edit change tracker source is readable");
    let preflight = std::fs::read_to_string(root.join("src/harness/preflight.rs"))
        .expect("preflight source is readable");
    let preflight_doc =
        std::fs::read_to_string(repo_root.join("docs/testing/PreflightGateSuite.md"))
            .expect("preflight doc is readable");
    let runtime_contracts =
        std::fs::read_to_string(repo_root.join("docs/design/runtime-contracts.md"))
            .expect("runtime contracts doc is readable");
    let detailed_design = std::fs::read_to_string(repo_root.join("docs/design/detailed-design.md"))
        .expect("detailed design doc is readable");
    let item_lifecycle =
        std::fs::read_to_string(repo_root.join("docs/design/itemlifecycle-detail-design.md"))
            .expect("item lifecycle design doc is readable");

    assert!(
        change_tracker.contains("ChangeKind::Delete")
            && change_tracker.contains("The file removal is established for this session")
            && change_tracker.contains("ChangeKind::Move")
            && change_tracker.contains("The file move is established for this session"),
        "Delete/Move successful FileChange feedback should project completed evidence, not a next edit instruction"
    );
    assert!(
        change_tracker.contains("ChangeKind::Delete,")
            && change_tracker.contains("ChangeKind::Move,")
            && change_tracker.contains("expected_guidance_count == 4"),
        "successful_file_change_tool_feedback_is_evidence_only should cover every ChangeKind"
    );
    assert!(change_tracker.contains("edit_file_change_feedback_all_kinds_evidence_only"));
    assert!(preflight.contains("edit_file_change_feedback_all_kinds_evidence_only"));
    assert!(preflight_doc.contains("edit_file_change_feedback_all_kinds_evidence_only"));
    assert!(runtime_contracts.contains("edit_file_change_feedback_all_kinds_evidence_only"));
    assert!(detailed_design.contains("edit_file_change_feedback_all_kinds_evidence_only"));
    assert!(item_lifecycle.contains("edit_file_change_feedback_all_kinds_evidence_only"));
}

#[test]
fn llm_provider_policy_upgrades_existing_no_tool_prompt_for_tool_requests() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root.parent().expect("crate has repository parent");
    let llm_contract = std::fs::read_to_string(root.join("src/llm/contract.rs"))
        .expect("llm contract source is readable");
    let preflight = std::fs::read_to_string(root.join("src/harness/preflight.rs"))
        .expect("preflight source is readable");
    let preflight_doc =
        std::fs::read_to_string(repo_root.join("docs/testing/PreflightGateSuite.md"))
            .expect("preflight doc is readable");
    let runtime_contracts =
        std::fs::read_to_string(repo_root.join("docs/design/runtime-contracts.md"))
            .expect("runtime contracts doc is readable");
    let detailed_design = std::fs::read_to_string(repo_root.join("docs/design/detailed-design.md"))
        .expect("detailed design doc is readable");
    let item_lifecycle =
        std::fs::read_to_string(repo_root.join("docs/design/itemlifecycle-detail-design.md"))
            .expect("item lifecycle design doc is readable");

    assert!(
        moyai::llm::contract::provider_policy_tool_lifecycle_upgrade_fixture_passes(),
        "OpenAI-compatible tool-enabled provider prompt rendering should upgrade an existing no-tool provider-policy prompt with the Tool Lifecycle Policy"
    );
    assert!(llm_contract.contains("provider_policy_tool_lifecycle_upgrade_fixture_passes"));
    assert!(llm_contract.contains("OPENAI_COMPATIBLE_ONLY_TOOL_LIFECYCLE_POLICY"));
    assert!(preflight.contains("llm_provider_policy_tool_lifecycle_upgrade"));
    assert!(preflight_doc.contains("llm_provider_policy_tool_lifecycle_upgrade"));
    assert!(runtime_contracts.contains("llm_provider_policy_tool_lifecycle_upgrade"));
    assert!(detailed_design.contains("llm_provider_policy_tool_lifecycle_upgrade"));
    assert!(item_lifecycle.contains("llm_provider_policy_tool_lifecycle_upgrade"));
}

#[test]
fn model_probe_rejects_extra_tool_arguments() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root.parent().expect("crate has repository parent");
    let model_probe = std::fs::read_to_string(root.join("src/llm/model_probe.rs"))
        .expect("model probe source is readable");
    let preflight = std::fs::read_to_string(root.join("src/harness/preflight.rs"))
        .expect("preflight source is readable");
    let preflight_doc =
        std::fs::read_to_string(repo_root.join("docs/testing/PreflightGateSuite.md"))
            .expect("preflight doc is readable");
    let runtime_contracts =
        std::fs::read_to_string(repo_root.join("docs/design/runtime-contracts.md"))
            .expect("runtime contracts doc is readable");
    let detailed_design = std::fs::read_to_string(repo_root.join("docs/design/detailed-design.md"))
        .expect("detailed design doc is readable");
    let item_lifecycle =
        std::fs::read_to_string(repo_root.join("docs/design/itemlifecycle-detail-design.md"))
            .expect("item lifecycle design doc is readable");

    assert!(
        moyai::llm::model_probe::model_probe_rejects_extra_tool_arguments_fixture_passes(),
        "model availability tool-call probe should reject schema-violating extra function.arguments properties"
    );
    assert!(model_probe.contains("model_probe_rejects_extra_tool_arguments_fixture_passes"));
    assert!(model_probe.contains("additionalProperties"));
    assert!(preflight.contains("model_probe_typed_arguments_schema_validation"));
    assert!(preflight_doc.contains("model_probe_typed_arguments_schema_validation"));
    assert!(runtime_contracts.contains("model_probe_typed_arguments_schema_validation"));
    assert!(detailed_design.contains("model_probe_typed_arguments_schema_validation"));
    assert!(item_lifecycle.contains("model_probe_typed_arguments_schema_validation"));
}

#[test]
fn mcp_tools_list_rejects_malformed_tool_descriptors() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root.parent().expect("crate has repository parent");
    let mcp = std::fs::read_to_string(root.join("src/mcp/mod.rs")).expect("mcp source is readable");
    let preflight = std::fs::read_to_string(root.join("src/harness/preflight.rs"))
        .expect("preflight source is readable");
    let preflight_doc =
        std::fs::read_to_string(repo_root.join("docs/testing/PreflightGateSuite.md"))
            .expect("preflight doc is readable");
    let runtime_contracts =
        std::fs::read_to_string(repo_root.join("docs/design/runtime-contracts.md"))
            .expect("runtime contracts doc is readable");
    let detailed_design = std::fs::read_to_string(repo_root.join("docs/design/detailed-design.md"))
        .expect("detailed design doc is readable");
    let item_lifecycle =
        std::fs::read_to_string(repo_root.join("docs/design/itemlifecycle-detail-design.md"))
            .expect("item lifecycle design doc is readable");

    assert!(
        moyai::mcp::mcp_tools_list_rejects_malformed_tool_descriptors_fixture_passes(),
        "MCP tools/list parsing should reject malformed descriptor entries instead of silently dropping them from model-visible tool surface metadata"
    );
    assert!(mcp.contains("mcp_tools_list_rejects_malformed_tool_descriptors_fixture_passes"));
    assert!(mcp.contains("mcp_tools_list_descriptor_schema_validation"));
    assert!(preflight.contains("mcp_tools_list_descriptor_schema_validation"));
    assert!(preflight_doc.contains("mcp_tools_list_descriptor_schema_validation"));
    assert!(runtime_contracts.contains("mcp_tools_list_descriptor_schema_validation"));
    assert!(detailed_design.contains("mcp_tools_list_descriptor_schema_validation"));
    assert!(docs_contains_or_item_lifecycle_current_authority(
        &item_lifecycle,
        "mcp_tools_list_descriptor_schema_validation"
    ));
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
        provider: CURRENT_PROVIDER_PROFILE_PROVIDER.to_string(),
        model: CURRENT_PROVIDER_PROFILE_MODEL.to_string(),
        base_url: CURRENT_PROVIDER_PROFILE_BASE_URL.to_string(),
        access_mode: AccessMode::AutoReview,
        sandbox: moyai::protocol::SandboxProfile::WorkspaceWrite,
        shell_family: ShellFamily::PowerShell,
        model_capabilities: ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
            parallel_tool_calls: false,
            context_window: CURRENT_PROVIDER_PROFILE_CONTEXT_WINDOW,
            max_output_tokens: CURRENT_PROVIDER_PROFILE_MAX_OUTPUT_TOKENS,
        },
        route: TaskRoute::Code,
        process_phase: moyai::session::ProcessPhase::Author,
        active_contract: active_contract.clone(),
        allowed_tools: allowed_tools.clone(),
        tool_choice,
        images: Vec::new(),
        output_contract: moyai::protocol::OutputContract {
            final_answer_required: false,
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
