use assert_cmd::Command;
use serde_json::Value;

#[test]
fn model_availability_cli_writes_fail_closed_report() {
    let temp = tempfile::tempdir().expect("tempdir");
    let output = temp.path().join("model_availability.json");

    Command::cargo_bin("moyai")
        .expect("binary")
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
    assert_eq!(report["generated_by"], "moyai_model_availability_v1");
    assert_eq!(report["model"], "missing-model");
    assert_eq!(report["base_url"], "http://127.0.0.1:9");
    assert_eq!(report["v1_present"], false);
    assert_eq!(report["native_present"], false);
    assert!(report["openai_error"].is_string());
    assert!(report["native_error"].is_string());
}
