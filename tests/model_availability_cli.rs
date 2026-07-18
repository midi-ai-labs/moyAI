use assert_cmd::Command;
use serde_json::Value;

#[test]
fn model_availability_cli_writes_fail_closed_report() {
    let temp = tempfile::tempdir().expect("tempdir");
    let output = temp.path().join("model_availability.json");

    Command::cargo_bin("moyai")
        .expect("binary")
        .env("MOYAI_CONFIG_PATH", temp.path().join("config.toml"))
        .args([
            "model",
            "availability",
            "--dir",
            temp.path().to_str().expect("utf8"),
            "--base-url",
            "http://127.0.0.1:9",
            "--model",
            "missing-model",
            "--output",
            output.to_str().expect("utf8"),
        ])
        .assert()
        .failure();

    let report: Value = serde_json::from_slice(&std::fs::read(&output).expect("report"))
        .expect("model availability report json");
    assert_eq!(report["gate"], "model_availability");
    assert_eq!(report["status"], "fail");
    assert_eq!(
        report["generated_by"],
        "moyai_model_availability_v4_catalog_and_load_state"
    );
    assert_eq!(report["model"], "missing-model");
    assert_eq!(report["base_url"], "http://127.0.0.1:9");
    assert_eq!(report["v1_present"], false);
    assert_eq!(report["native_present"], false);
    assert_eq!(
        report["provider_metadata_mode"],
        "lm_studio_native_required"
    );
    assert!(report.get("tool_call_probe_passed").is_none());
    assert!(report.get("tool_call_probes").is_none());
    assert!(report["openai_error"].is_null());
    assert!(report["native_error"].is_string());
    assert!(
        report["readiness_detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("not registered"))
    );
}
