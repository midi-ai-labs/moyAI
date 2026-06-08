use assert_cmd::Command;
use camino::Utf8PathBuf;
use moyai::harness::preflight::{PreflightResultStatus, run_artifact_replay_preflight};
use serde_json::Value;

fn write_valid_route_artifacts(root: &std::path::Path) {
    let required = [
        "route_manifest.json",
        "case_progress.json",
        "verification_command_log.json",
        "workspace_diff_manifest.json",
        "result.json",
        "preflight_report.json",
        "timeout_classification.json",
    ];
    let route_root = root.to_string_lossy();
    std::fs::write(
        root.join("route_manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "route_id": "required_core_route_a",
            "route_type": "required_core",
            "route_level_verdict": "not_run",
            "fixture_version": "manual_st_route_runner.v1",
            "case_ids": ["case1", "case3"],
            "evidence_artifacts": required,
            "build_identifier": "moyai-test",
            "model_id": "qwen/qwen3.6-35b-a3b",
            "provider_base_url": "http://127.0.0.1:1234",
            "workspace_path": route_root,
            "start_time": "1",
            "end_time": "1"
        }))
        .expect("route manifest"),
    )
    .expect("route manifest");
    std::fs::write(
        root.join("case_progress.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "route_id": "required_core_route_a",
            "route_type": "required_core",
            "route_level_verdict": "not_run",
            "progress_status": "route_artifact_written",
            "evidence_artifact_schema_version": "manual_st.case_progress.v1",
            "last_progress_at": "1"
        }))
        .expect("case progress"),
    )
    .expect("case progress");
    std::fs::write(
        root.join("verification_command_log.json"),
        serde_json::to_vec_pretty(&serde_json::json!({ "commands": [] }))
            .expect("verification log"),
    )
    .expect("verification log");
    std::fs::write(
        root.join("workspace_diff_manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "expected_artifacts": [],
            "actual_added_files": [],
            "actual_modified_files": [],
            "actual_deleted_files": [],
            "unexpected_outside_workspace_access_or_change": false,
            "fixture_input_mutation": false,
            "verdict": "blocked",
            "diagnostics": ["workspace was not executed"]
        }))
        .expect("workspace diff"),
    )
    .expect("workspace diff");
    std::fs::write(
        root.join("result.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "route_id": "required_core_route_a",
            "route_type": "required_core",
            "route_root": route_root,
            "case_ids": ["case1", "case3"],
            "model_id": "qwen/qwen3.6-35b-a3b",
            "provider_base_url": "http://127.0.0.1:1234",
            "build_identifier": "moyai-test",
            "expected_artifacts": [],
            "route_level_verdict": "not_run",
            "session_ids": [],
            "case_results": [],
            "started_at": "1",
            "completed_at": "1",
            "stop_reason": "dry_run requested; no live LLM route was executed"
        }))
        .expect("result"),
    )
    .expect("result");
    std::fs::write(
        root.join("preflight_report.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "status": "pass",
            "generated_by": "codex_style_preflight_v2",
            "results": [
                {
                    "fixture_id": "fixture.manual_st.route_evidence_schema",
                    "status": "pass"
                }
            ]
        }))
        .expect("preflight"),
    )
    .expect("preflight");
    std::fs::write(
        root.join("timeout_classification.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "classified_terminal_before_timeout": true,
            "evidence_refs": [],
            "outer_timeout": false,
            "primary_timeout_owner": null,
            "provider_stream_retry_exhausted": false,
            "provider_stream_stall": false,
            "provider_transport_stream_error": false,
            "repeated_no_progress_repair": false,
            "semantic_no_progress_terminal_guard": false,
            "tool_or_environment_stall": false,
            "verification_non_convergence": false
        }))
        .expect("timeout"),
    )
    .expect("timeout");
}

#[test]
fn preflight_run_cli_writes_codex_style_report() {
    let temp = tempfile::tempdir().expect("tempdir");
    let output = temp.path().join("preflight_report.json");

    Command::cargo_bin("moyai")
        .expect("binary")
        .args(["preflight", "run", "--output"])
        .arg(&output)
        .assert()
        .success();

    let report: Value = serde_json::from_slice(&std::fs::read(&output).expect("report"))
        .expect("preflight report json");
    assert_eq!(report["status"], "pass");
    assert_eq!(report["generated_by"], "codex_style_preflight_v2");
    assert!(
        report["results"]
            .as_array()
            .expect("results")
            .iter()
            .all(|result| result["fixture_id"].is_string())
    );
}

#[test]
fn preflight_artifact_cli_requires_route_level_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");

    Command::cargo_bin("moyai")
        .expect("binary")
        .args([
            "preflight",
            "artifact",
            "--artifact-root",
            temp.path().to_str().expect("utf8"),
        ])
        .assert()
        .failure();

    write_valid_route_artifacts(temp.path());

    Command::cargo_bin("moyai")
        .expect("binary")
        .args([
            "preflight",
            "artifact",
            "--artifact-root",
            temp.path().to_str().expect("utf8"),
        ])
        .assert()
        .success();
}

#[test]
fn preflight_artifact_rejects_empty_route_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");

    for artifact in [
        "route_manifest.json",
        "case_progress.json",
        "verification_command_log.json",
        "workspace_diff_manifest.json",
        "result.json",
        "preflight_report.json",
        "timeout_classification.json",
    ] {
        std::fs::write(temp.path().join(artifact), "{}").expect("artifact");
    }

    let artifact_root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8");
    let report = run_artifact_replay_preflight(&artifact_root, Vec::new()).expect("preflight");
    assert_eq!(report.status, PreflightResultStatus::Fail);
    assert!(
        report.results[0].diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("malformed route evidence artifacts")
                && diagnostic.contains("route_manifest.json.route_id")
                && diagnostic.contains("preflight_report.json.generated_by")
        }),
        "diagnostics should name malformed typed route evidence: {:?}",
        report.results[0].diagnostics
    );
}
