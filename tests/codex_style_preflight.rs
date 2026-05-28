use assert_cmd::Command;
use serde_json::Value;

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
