use super::*;

#[tokio::test]
async fn endpoint_change_shutdown_rechecks_the_worker_journal_after_a_stale_idle_observation() {
    use crate::runner::operations::{ProvisionMode, RunnerOperation};
    for case in [
        "empty",
        "claimed_after_observation",
        "unknown",
        "resumed",
        "wrong_consent",
    ] {
        let (_temp, settings, journal_path) = fixture();
        let root = journal_path.parent().unwrap();
        let paths = crate::storage::StoragePaths {
            data_dir: root.join("data"),
            database_path: root.join("data/db.sqlite3"),
            truncation_dir: root.join("data/output"),
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
        sqlite.migrate().unwrap();
        let process = crate::app::AppBootstrap::create_process_runtime(
            crate::storage::StoreBundle::new(sqlite),
        )
        .await
        .unwrap();
        let host = RunnerHost::from_process(process).unwrap();
        let binding = format!("hub|device|https://old.example:9471|{}", "a".repeat(64));
        {
            let mut store = host.inner.operations.lock().unwrap();
            let mut next = store.installed.clone();
            next.mode = ProvisionMode::Paused;
            next.desktop_binding = Some(binding.clone());
            store.update(next).unwrap();
        }
        let journal = Journal::open(&journal_path, &settings).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let mut ca = rcgen::CertificateParams::default();
        ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let client = crate::device_network::DeviceClient::new(
            &crate::device_network::SharedHubConfig {
                hub_url: "https://127.0.0.1:1".into(),
                ca_certificate_pem: ca.self_signed(&key).unwrap().pem(),
            },
            None,
            "device".into(),
        )
        .unwrap();
        let mut controller = Controller {
            host: host.clone(),
            settings: settings.clone(),
            client: SharedClient::for_test(client),
            journal,
            checkpoint_cursor: String::new(),
            commands: None,
            external: None,
            local_human: None,
        };
        assert!(controller.journal.active().unwrap().is_empty());
        // This is the race a frontend status-read then ordinary Shutdown misses.
        if matches!(case, "claimed_after_observation" | "unknown") {
            let mut entry = controller
                .journal
                .intent(assignment(), settings.environments[0].clone())
                .unwrap();
            if case == "unknown" {
                controller.journal.executing(&mut entry).unwrap();
                controller
                    .journal
                    .uncertain(&mut entry, "uncertain execution")
                    .unwrap();
            }
        }
        if case == "resumed" {
            let mut store = host.inner.operations.lock().unwrap();
            let mut next = store.installed.clone();
            next.mode = ProvisionMode::Available;
            store.update(next).unwrap();
        }
        let result = controller
            .operator_request(RunnerOperation::QuiescentShutdown {
                expected_desktop_binding: if case == "wrong_consent" {
                    "other".into()
                } else {
                    binding
                },
            })
            .await;
        assert_eq!(result.is_ok(), case == "empty", "{case}");
        assert_eq!(controller.closing(), case == "empty", "{case}");
        if case == "empty" {
            assert!(host.operate(RunnerOperation::Resume).await.is_err());
            assert_eq!(
                host.inner.operations.lock().unwrap().installed.mode,
                ProvisionMode::Paused
            );
        }
        if matches!(case, "claimed_after_observation" | "unknown") {
            assert_eq!(controller.journal.active().unwrap().len(), 1);
        }
        host.begin_shutdown().unwrap();
        host.wait_stopped().await;
    }
}

#[test]
fn approval_consume_wire_distinguishes_answers_reconfirmation_and_invalid_responses() {
    use super::protocol::ApprovalConsumeResult;
    assert!(
        serde_json::from_value::<Option<ApprovalConsumeResult>>(json!(null))
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        serde_json::from_value::<ApprovalConsumeResult>(
            json!({"approval_id":"approval","decision":"approve"})
        )
        .unwrap(),
        ApprovalConsumeResult::Answer { .. }
    ));
    assert!(matches!(
        serde_json::from_value::<ApprovalConsumeResult>(
            json!({"approval_id":"approval","reconfirmation_required":true})
        )
        .unwrap(),
        ApprovalConsumeResult::ReconfirmationRequired {
            reconfirmation_required: true,
            ..
        }
    ));
    for invalid in [
        json!({"approval_id":"approval","decision":"approve","reconfirmation_required":true}),
        json!({"approval_id":"approval"}),
        json!({"error":"conflict"}),
        json!({"approval_id":"approval","decision":"invalidated"}),
        json!({"approval_id":"approval","reconfirmation_required":"true"}),
    ] {
        assert!(
            serde_json::from_value::<ApprovalConsumeResult>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
}

pub(super) fn fixture() -> (tempfile::TempDir, SharedSettings, camino::Utf8PathBuf) {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../project_sandbox/runner-shared-tests");
    std::fs::create_dir_all(&base).unwrap();
    let temp = tempfile::tempdir_in(base).unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(temp.path().to_owned()).unwrap();
    let settings = SharedSettings {
        version: 1,
        hub_id: "hub".into(),
        device_id: "device".into(),
        resource_scope: ResourceScope::WorkspaceIsolation { confirmed: true },
        environments: vec![EnvironmentMapping {
            environment_id: "environment".into(),
            directory: root.clone(),
            access_mode: crate::config::AccessMode::Default,
            allowed_child_environments: vec![],
        }],
    };
    (temp, settings, root.join("journal.sqlite3"))
}

pub(super) fn assignment() -> Assignment {
    Assignment {
        attempt_id: "attempt".into(),
        generation: 1,
        authority_generation: 1,
        runner_id: "device".into(),
        stop_requested: false,
        job: Job {
            id: "job".into(),
            root_id: "job".into(),
            parent_id: None,
            project_id: "project".into(),
            environment_id: "environment".into(),
            requestor_id: "user".into(),
            assignee_id: "user".into(),
            title: "Task".into(),
            input: json!({"version":1,"prompt":"Task"}),
            checkpoint: None,
            result: None,
            state: JobState::Assigned,
            awaiting_child_id: None,
            revision: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
        },
        child_result: None,
        allowed_child_environments: vec![],
    }
}

#[test]
fn shared_input_cannot_override_local_authority() {
    let valid: SharedInput = serde_json::from_value(json!({"version":1,"prompt":"Solve"})).unwrap();
    valid.validate().unwrap();
    for key in ["directory", "access_mode", "model", "session_id"] {
        let mut value = serde_json::to_value(&valid).unwrap();
        value[key] = json!("untrusted override");
        assert!(serde_json::from_value::<SharedInput>(value).is_err());
    }
    assert!(
        SharedInput {
            version: 3,
            prompt: "Task".into(),
            input_refs: vec![],
        }
        .validate()
        .is_err()
    );
    assert!(
        SharedInput {
            version: 1,
            prompt: " ".into(),
            input_refs: vec![],
        }
        .validate()
        .is_err()
    );
}

#[test]
fn journal_reopen_distinguishes_unexecuted_intent_from_possible_effects() {
    let (_temp, settings, path) = fixture();
    let mut journal = Journal::open(&path, &settings).unwrap();
    let mut entry = journal
        .intent(assignment(), settings.environments[0].clone())
        .unwrap();
    let run_id = entry.run_id;
    assert!(Journal::open(&path, &settings).is_err());
    drop(journal);
    let mut journal = Journal::open(&path, &settings).unwrap();
    entry = journal.get("attempt").unwrap().unwrap();
    assert_eq!(entry.phase, Phase::Intent);
    assert_eq!(entry.run_id, run_id);
    journal.executing(&mut entry).unwrap();
    drop(journal);
    let mut journal = Journal::open(&path, &settings).unwrap();
    entry = journal.get("attempt").unwrap().unwrap();
    assert_eq!(entry.phase, Phase::Executing);
    assert!(journal.executing(&mut entry).is_err());
    journal.uncertain(&mut entry, "crash boundary").unwrap();
    drop(journal);
    let mut journal = Journal::open(&path, &settings).unwrap();
    let mut entry = journal.get("attempt").unwrap().unwrap();
    assert_eq!(entry.phase, Phase::Uncertain);
    assert!(journal.executing(&mut entry).is_err());
    assert!(journal.settled(&mut entry).is_err());
    assert_eq!(journal.active().unwrap().len(), 1);
}

#[test]
fn journal_keeps_exact_report_for_lost_ack_and_binds_hub_identity() {
    let (_temp, mut settings, path) = fixture();
    let mut journal = Journal::open(&path, &settings).unwrap();
    let mut entry = journal
        .intent(assignment(), settings.environments[0].clone())
        .unwrap();
    journal.executing(&mut entry).unwrap();
    let report = Report::for_assignment(
        &entry.assignment,
        "outcome",
        ReportOutcome::Finished {
            success: true,
            result: json!({"text":"done"}),
            resources_released: true,
        },
    );
    journal.outcome(&mut entry, report.clone()).unwrap();
    drop(journal);
    let mut journal = Journal::open(&path, &settings).unwrap();
    entry = journal.get("attempt").unwrap().unwrap();
    assert_eq!(entry.report, Some(report));
    assert_eq!(entry.phase, Phase::ReportPending);
    journal.settled(&mut entry).unwrap();
    assert!(journal.active().unwrap().is_empty());
    assert_eq!(
        journal
            .intent(assignment(), settings.environments[0].clone())
            .unwrap()
            .run_id,
        entry.run_id
    );
    drop(journal);
    settings.hub_id = "different-hub".into();
    assert!(Journal::open(&path, &settings).is_err());
}

#[test]
fn persisted_attempt_cannot_move_to_another_environment_authority() {
    let (_temp, settings, path) = fixture();
    let mut journal = Journal::open(&path, &settings).unwrap();
    journal
        .intent(assignment(), settings.environments[0].clone())
        .unwrap();
    let mut changed = settings.environments[0].clone();
    changed.access_mode = crate::config::AccessMode::FullAccess;
    assert!(journal.intent(assignment(), changed).is_err());
    let mut changed = assignment();
    changed.generation += 1;
    assert!(
        journal
            .intent(changed, settings.environments[0].clone())
            .is_err()
    );
}

#[test]
fn rejected_child_handoff_retains_original_for_uncertain_ack_reconciliation() {
    let (_temp, settings, path) = fixture();
    let mut journal = Journal::open(&path, &settings).unwrap();
    let mut entry = journal
        .intent(assignment(), settings.environments[0].clone())
        .unwrap();
    journal.executing(&mut entry).unwrap();
    let original = Report::for_assignment(
        &entry.assignment,
        "outcome",
        ReportOutcome::YieldToChild {
            checkpoint: json!({"receipt":"checkpoint"}),
            child_environment_id: "solver".into(),
            child_title: "Child".into(),
            child_input: json!({"version":1,"prompt":"Solve"}),
            resources_released: true,
        },
    );
    journal.outcome(&mut entry, original.clone()).unwrap();
    let fallback = Report::for_assignment(
        &entry.assignment,
        "yield_rejected",
        ReportOutcome::Finished {
            success: false,
            result: json!({"error":"handoff rejected"}),
            resources_released: true,
        },
    );
    journal
        .retain_rejected_yield_fallback(&mut entry, fallback.clone())
        .unwrap();
    drop(journal);
    let journal = Journal::open(&path, &settings).unwrap();
    let reopened = journal.get("attempt").unwrap().unwrap();
    assert_eq!(reopened.phase, Phase::ReportPending);
    assert_eq!(reopened.report, Some(original));
    assert_eq!(reopened.fallback_report, Some(fallback));
}

#[test]
fn checkpoint_settlement_delivery_is_bounded_and_acknowledgements_survive_reopen() {
    let (_temp, settings, path) = fixture();
    let mut journal = Journal::open(&path, &settings).unwrap();
    for index in 0..18 {
        let mut assignment = assignment();
        assignment.attempt_id = format!("attempt-{index:02}");
        let mut entry = journal
            .intent(assignment, settings.environments[0].clone())
            .unwrap();
        journal.executing(&mut entry).unwrap();
        let report = Report::for_assignment(
            &entry.assignment,
            "outcome",
            ReportOutcome::YieldToChild {
                checkpoint: json!({"receipt":index}),
                child_environment_id: "solver".into(),
                child_title: "Child".into(),
                child_input: json!({"version":1,"prompt":"Solve"}),
                resources_released: true,
            },
        );
        journal.outcome(&mut entry, report).unwrap();
        assert!(
            journal
                .acknowledge_checkpoint_settlement(&mut entry)
                .is_err()
        );
        journal.settled(&mut entry).unwrap();
    }
    let first = journal.unsettled_checkpoints("").unwrap();
    assert_eq!(first.len(), 1);
    let tail = journal
        .unsettled_checkpoints(&first.last().unwrap().assignment.attempt_id)
        .unwrap();
    assert_eq!(tail.len(), 1);
    assert_ne!(
        tail[0].assignment.attempt_id,
        first[0].assignment.attempt_id
    );
    let mut acknowledged = first[0].clone();
    journal
        .acknowledge_checkpoint_settlement(&mut acknowledged)
        .unwrap();
    drop(journal);
    // Older task journals retained this receipt only inside the Yield report.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE runner_attempts SET entry_json=json_remove(entry_json,'$.local_checkpoint')",
            [],
        )
        .unwrap();
    let journal = Journal::open(&path, &settings).unwrap();
    assert!(
        journal.active().unwrap().is_empty(),
        "Pending local delivery must not hold a Hub execution slot"
    );
    let next = journal.unsettled_checkpoints("").unwrap();
    assert_eq!(next.len(), 1);
    assert!(
        next.iter()
            .all(|entry| entry.assignment.attempt_id != acknowledged.assignment.attempt_id)
    );
    assert_eq!(
        journal
            .get(&acknowledged.assignment.attempt_id)
            .unwrap()
            .unwrap()
            .report,
        acknowledged.report
    );
}
