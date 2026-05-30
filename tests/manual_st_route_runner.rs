use assert_cmd::Command;
use moyai::harness::manual_st::{
    ManualStRouteKind, closeout_continuation_budget_blocks_same_workspace_stall_fixture_passes,
    closeout_continuation_budget_is_scoped_by_failure_signature_fixture_passes,
    closeout_continuation_is_text_only_fixture_passes,
    completed_expected_artifact_clears_stale_authoring_obligation_fixture_passes,
    final_assistant_open_obligation_continuation_hook_fixture_passes,
    final_assistant_open_obligation_not_clean_closeout_fixture_passes,
    latest_verification_result_drives_closeout_fixture_passes,
    manual_st_default_output_root_uses_workspace_sandbox_fixture_passes,
    manual_st_route_omits_provider_defaults_without_explicit_override_fixture_passes,
    manual_st_route_plan,
    open_obligation_continuation_expected_inventory_is_non_authoring_fixture_passes,
    post_repair_route_verification_clears_stale_repair_fixture_passes,
    route_case_progress_phase_boundaries_fixture_passes,
    route_inflight_case_progress_artifact_fixture_passes,
    route_owned_command_timeout_fixture_passes,
    route_terminal_verdict_rematerializes_from_case_results_fixture_passes,
    route_verification_waits_for_authored_artifacts_fixture_passes,
    run_error_closeout_replaces_stale_continuation_evidence_fixture_passes,
    run_error_open_obligation_uses_closeout_continuation_budget_fixture_passes,
    satisfied_docs_repair_does_not_reopen_route_closeout_fixture_passes,
    stage_scoped_verification_commands_are_spec_owned_fixture_passes,
    successful_closeout_continuation_rematerializes_case_verdict_fixture_passes,
    terminalized_session_continuation_ledger_bounds_same_stage_recovery_fixture_passes,
    verification_evidence_after_content_change_invalidated_fixture_passes,
    verification_failed_closeout_builds_repair_hook_prompt_fixture_passes,
    verification_failure_labels_do_not_become_authoring_obligations_fixture_passes,
    verification_failure_preserves_closeout_evidence_fixture_passes,
    vision_prompt_uses_labeled_attachment_fixture_passes,
};
use serde_json::Value;

#[test]
fn manual_st_route_plan_owns_required_core_route_shape() {
    let plan = manual_st_route_plan(ManualStRouteKind::RequiredCore);

    assert_eq!(plan.route_id, "required_core_route_a");
    assert_eq!(plan.route_type, "required_core");
    assert_eq!(plan.case_ids, vec!["case1", "case3"]);
    assert_eq!(
        plan.required_artifacts,
        vec![
            "route_manifest.json",
            "case_progress.json",
            "verification_command_log.json",
            "workspace_diff_manifest.json",
            "result.json",
            "preflight_report.json",
            "timeout_classification.json"
        ]
    );
}

#[test]
fn manual_st_route_plan_supports_targeted_case1_run() {
    let plan = manual_st_route_plan(ManualStRouteKind::TargetedCoreCase1);

    assert_eq!(plan.route_id, "targeted_core_case1");
    assert_eq!(plan.route_type, "targeted_support");
    assert_eq!(plan.case_ids, vec!["case1"]);
    assert!(
        plan.required_artifacts
            .contains(&"route_manifest.json".to_string())
    );
}

#[test]
fn final_assistant_with_open_obligation_is_route_failure_evidence() {
    assert!(final_assistant_open_obligation_not_clean_closeout_fixture_passes());
}

#[test]
fn final_assistant_with_open_obligation_builds_continuation_hook_prompt() {
    assert!(final_assistant_open_obligation_continuation_hook_fixture_passes());
}

#[test]
fn open_obligation_continuation_expected_inventory_is_not_authoring_scope() {
    assert!(open_obligation_continuation_expected_inventory_is_non_authoring_fixture_passes());
}

#[test]
fn route_verification_waits_for_artifact_authoring_closeout() {
    assert!(route_verification_waits_for_authored_artifacts_fixture_passes());
}

#[test]
fn post_repair_route_verification_clears_stale_repair_authority() {
    assert!(post_repair_route_verification_clears_stale_repair_fixture_passes());
}

#[test]
fn closeout_continuation_does_not_reattach_original_images() {
    assert!(closeout_continuation_is_text_only_fixture_passes());
}

#[test]
fn vision_manual_st_prompt_uses_labeled_attachment_not_workspace_filename() {
    assert!(vision_prompt_uses_labeled_attachment_fixture_passes());
}

#[test]
fn closeout_uses_latest_verification_result_per_command() {
    assert!(latest_verification_result_drives_closeout_fixture_passes());
}

#[test]
fn verification_pass_before_content_change_is_not_closeout_evidence() {
    assert!(verification_evidence_after_content_change_invalidated_fixture_passes());
}

#[test]
fn route_verification_failure_keeps_closeout_evidence() {
    assert!(verification_failure_preserves_closeout_evidence_fixture_passes());
}

#[test]
fn run_error_recomputes_current_closeout_evidence() {
    assert!(run_error_closeout_replaces_stale_continuation_evidence_fixture_passes());
}

#[test]
fn run_error_open_obligation_consumes_closeout_continuation_budget() {
    assert!(run_error_open_obligation_uses_closeout_continuation_budget_fixture_passes());
}

#[test]
fn completed_expected_artifact_clears_stale_authoring_obligation() {
    assert!(completed_expected_artifact_clears_stale_authoring_obligation_fixture_passes());
}

#[test]
fn satisfied_docs_repair_does_not_reopen_route_closeout() {
    assert!(satisfied_docs_repair_does_not_reopen_route_closeout_fixture_passes());
}

#[test]
fn route_inflight_case_progress_is_root_artifact() {
    assert!(route_inflight_case_progress_artifact_fixture_passes());
}

#[test]
fn route_case_progress_records_phase_boundaries() {
    assert!(route_case_progress_phase_boundaries_fixture_passes());
}

#[test]
fn successful_closeout_continuation_rematerializes_case_verdict() {
    assert!(successful_closeout_continuation_rematerializes_case_verdict_fixture_passes());
}

#[test]
fn route_terminal_verdict_rematerializes_from_case_results() {
    assert!(route_terminal_verdict_rematerializes_from_case_results_fixture_passes());
}

#[test]
fn failed_verification_closeout_builds_repair_hook_prompt() {
    assert!(verification_failed_closeout_builds_repair_hook_prompt_fixture_passes());
}

#[test]
fn closeout_continuation_budget_is_failure_signature_scoped() {
    assert!(closeout_continuation_budget_is_scoped_by_failure_signature_fixture_passes());
}

#[test]
fn closeout_continuation_budget_blocks_same_workspace_stall() {
    assert!(closeout_continuation_budget_blocks_same_workspace_stall_fixture_passes());
}

#[test]
fn terminalized_session_continuation_ledger_bounds_same_stage_recovery() {
    assert!(terminalized_session_continuation_ledger_bounds_same_stage_recovery_fixture_passes());
}

#[test]
fn verification_failure_labels_do_not_become_authoring_obligations() {
    assert!(verification_failure_labels_do_not_become_authoring_obligations_fixture_passes());
}

#[test]
fn manual_st_default_output_root_is_outside_moyai_git_root() {
    assert!(manual_st_default_output_root_uses_workspace_sandbox_fixture_passes());
}

#[test]
fn manual_st_route_inherits_provider_config_without_explicit_override() {
    assert!(manual_st_route_omits_provider_defaults_without_explicit_override_fixture_passes());
}

#[test]
fn route_owned_commands_are_bounded_by_harness_wait_policy() {
    assert!(route_owned_command_timeout_fixture_passes());
}

#[test]
fn stage_scoped_verification_commands_come_from_spec_contract() {
    assert!(stage_scoped_verification_commands_are_spec_owned_fixture_passes());
}

#[test]
fn manual_st_route_cli_dry_run_writes_route_owned_artifacts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let preflight = temp.path().join("preflight_report.json");
    let output = temp.path().join("route");
    std::fs::write(
        &preflight,
        r#"{"status":"pass","generated_by":"test","results":[]}"#,
    )
    .expect("preflight");

    Command::cargo_bin("moyai")
        .expect("binary")
        .args([
            "manual-st",
            "route",
            "--route",
            "required-core",
            "--preflight-report",
            preflight.to_str().expect("utf8"),
            "--output-root",
            output.to_str().expect("utf8"),
            "--dry-run",
        ])
        .assert()
        .success();

    for artifact in [
        "route_manifest.json",
        "case_progress.json",
        "verification_command_log.json",
        "workspace_diff_manifest.json",
        "result.json",
        "preflight_report.json",
        "timeout_classification.json",
    ] {
        assert!(
            output.join(artifact).exists(),
            "missing route artifact {artifact}"
        );
    }

    let manifest: Value =
        serde_json::from_slice(&std::fs::read(output.join("route_manifest.json")).expect("read"))
            .expect("manifest json");
    assert_eq!(manifest["route_type"], "required_core");
    assert_eq!(manifest["case_ids"], serde_json::json!(["case1", "case3"]));
    assert_eq!(manifest["route_level_verdict"], "not_run");
    assert_eq!(manifest["fixture_version"], "manual_st_route_runner.v1");

    let progress: Value =
        serde_json::from_slice(&std::fs::read(output.join("case_progress.json")).expect("read"))
            .expect("case progress json");
    assert_eq!(progress["route_level_verdict"], "not_run");
    assert_eq!(
        progress["evidence_artifact_schema_version"],
        "manual_st.case_progress.v1"
    );
}

#[test]
fn manual_st_route_cli_timeout_materializes_route_fail_artifacts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let preflight = temp.path().join("preflight_report.json");
    let output = temp.path().join("route");
    std::fs::write(
        &preflight,
        r#"{"status":"pass","generated_by":"test","results":[]}"#,
    )
    .expect("preflight");

    Command::cargo_bin("moyai")
        .expect("binary")
        .args([
            "manual-st",
            "route",
            "--route",
            "required-core",
            "--preflight-report",
            preflight.to_str().expect("utf8"),
            "--output-root",
            output.to_str().expect("utf8"),
            "--max-turn-seconds",
            "0",
        ])
        .assert()
        .failure();

    for artifact in [
        "route_manifest.json",
        "case_progress.json",
        "verification_command_log.json",
        "workspace_diff_manifest.json",
        "result.json",
        "preflight_report.json",
        "timeout_classification.json",
    ] {
        assert!(
            output.join(artifact).exists(),
            "missing route artifact {artifact}"
        );
    }

    let manifest: Value =
        serde_json::from_slice(&std::fs::read(output.join("route_manifest.json")).expect("read"))
            .expect("manifest json");
    assert_eq!(manifest["route_level_verdict"], "fail");

    let result: Value =
        serde_json::from_slice(&std::fs::read(output.join("result.json")).expect("read"))
            .expect("result json");
    assert_eq!(result["case_results"][0]["timeout_observed"], true);

    let progress: Value =
        serde_json::from_slice(&std::fs::read(output.join("case_progress.json")).expect("read"))
            .expect("case progress json");
    assert_eq!(progress["route_level_verdict"], "fail");
    assert_eq!(progress["progress_status"], "route_terminalized");

    let timeout: Value = serde_json::from_slice(
        &std::fs::read(output.join("timeout_classification.json")).expect("read"),
    )
    .expect("timeout json");
    assert_eq!(timeout["outer_timeout"], true);
    assert_eq!(timeout["classified_terminal_before_timeout"], false);
}

#[test]
fn manual_st_route_cli_refuses_non_green_preflight() {
    let temp = tempfile::tempdir().expect("tempdir");
    let preflight = temp.path().join("preflight_report.json");
    std::fs::write(
        &preflight,
        r#"{"status":"fail","generated_by":"test","results":[]}"#,
    )
    .expect("preflight");

    Command::cargo_bin("moyai")
        .expect("binary")
        .args([
            "manual-st",
            "route",
            "--route",
            "required-core",
            "--preflight-report",
            preflight.to_str().expect("utf8"),
            "--dry-run",
        ])
        .assert()
        .failure();
}
