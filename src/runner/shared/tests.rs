use super::*;

#[tokio::test]
async fn received_activity_keeps_live_run_state_and_stop_checks_exact_attempt() {
    use crate::runner::operations::RunnerOperation;
    use crate::runner::{Execution, LocalRunState};
    use crate::runtime::{OwnedTaskHandle, RunControl};
    use tokio_util::sync::CancellationToken;

    let (_temp, settings, journal_path) = fixture();
    let root = journal_path.parent().unwrap();
    let paths = crate::storage::StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/db.sqlite3"),
        truncation_dir: root.join("data/output"),
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let process =
        crate::app::AppBootstrap::create_process_runtime(crate::storage::StoreBundle::new(sqlite))
            .await
            .unwrap();
    let host = RunnerHost::from_process(process).unwrap();
    let mut journal = Journal::open(&journal_path, &settings).unwrap();
    let mut entry = journal
        .intent(assignment(), settings.environments[0].clone())
        .unwrap();
    journal.executing(&mut entry).unwrap();
    let control = RunControl::new();
    let worker_control = control.token();
    let worker = OwnedTaskHandle::new(
        1,
        tokio::spawn(async move {
            worker_control.cancelled().await;
        }),
    );
    host.inner.state.lock().unwrap().runs.insert(
        entry.run_id,
        Execution {
            managed_scope_id: entry.run_id,
            request: LocalRunRequest {
                directory: root.into(),
                prompt: "Task".into(),
                session_id: None,
                title: None,
                single_agent: true,
            },
            session_id: Some(crate::session::SessionId::new()),
            control: control.clone(),
            process_lifetime: CancellationToken::new(),
            processes_drained: false,
            service: None,
            worker: Some(worker),
            result: None,
            response: None,
            shared: true,
            resource: None,
        },
    );
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
        settings,
        client: SharedClient::for_test(client),
        journal,
        checkpoint_cursor: String::new(),
        commands: None,
        external: None,
        local_project_id: None,
    };
    controller.project(false, None);
    let projected = host
        .inner
        .state
        .lock()
        .unwrap()
        .shared_projection
        .clone()
        .unwrap();
    assert!(!projected.connected);
    assert_eq!(projected.attempts.len(), 1);
    assert_eq!(projected.attempts[0].project_id, "project");
    assert_eq!(
        projected.attempts[0].local_state,
        Some(LocalRunState::Running)
    );
    for (generation, run_id) in [(2, entry.run_id), (1, ulid::Ulid::new())] {
        assert!(
            controller
                .operator_request(RunnerOperation::StopShared {
                    attempt_id: entry.assignment.attempt_id.clone(),
                    generation,
                    run_id,
                })
                .await
                .is_err()
        );
        assert!(!control.is_cancelled());
    }
    controller
        .stop_fenced_executions(Some(&[entry.assignment.clone()]))
        .await
        .unwrap();
    assert!(
        !control.is_cancelled(),
        "an unfenced delivery keeps its exact worker"
    );
    let mut fenced = entry.assignment.clone();
    fenced.stop_requested = true;
    controller
        .stop_fenced_executions(Some(&[fenced]))
        .await
        .unwrap();
    assert!(
        control.is_cancelled(),
        "the Hub fence stops the live worker"
    );
    controller
        .operator_request(RunnerOperation::StopShared {
            attempt_id: entry.assignment.attempt_id.clone(),
            generation: entry.assignment.generation,
            run_id: entry.run_id,
        })
        .await
        .unwrap();
    assert!(control.is_cancelled());
    host.begin_shutdown().unwrap();
    host.wait_stopped().await;
}

#[tokio::test]
async fn unavailable_hub_stops_a_live_worker_without_reexecuting_an_unknown_result() {
    use crate::runner::Execution;
    use crate::runtime::{OwnedTaskHandle, RunControl};
    use tokio_util::sync::CancellationToken;

    let (_temp, settings, journal_path) = fixture();
    let root = journal_path.parent().unwrap();
    let paths = crate::storage::StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/db.sqlite3"),
        truncation_dir: root.join("data/output"),
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let process =
        crate::app::AppBootstrap::create_process_runtime(crate::storage::StoreBundle::new(sqlite))
            .await
            .unwrap();
    let host = RunnerHost::from_process(process).unwrap();
    let mut journal = Journal::open(&journal_path, &settings).unwrap();
    let mut entry = journal
        .intent(assignment(), settings.environments[0].clone())
        .unwrap();
    journal.executing(&mut entry).unwrap();
    let control = RunControl::new();
    let lifetime = CancellationToken::new();
    let token = control.token();
    let worker = OwnedTaskHandle::new(1, tokio::spawn(async move { token.cancelled().await }));
    host.inner.state.lock().unwrap().runs.insert(
        entry.run_id,
        Execution {
            managed_scope_id: entry.run_id,
            request: LocalRunRequest {
                directory: root.into(),
                prompt: "Work during Hub outage".into(),
                session_id: None,
                title: None,
                single_agent: true,
            },
            session_id: Some(crate::session::SessionId::new()),
            control: control.clone(),
            process_lifetime: lifetime.clone(),
            processes_drained: false,
            service: None,
            worker: Some(worker),
            result: None,
            response: None,
            shared: true,
            resource: None,
        },
    );
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
        settings,
        client: SharedClient::for_test(client),
        journal,
        checkpoint_cursor: String::new(),
        commands: None,
        external: None,
        local_project_id: None,
    };
    controller.stop_fenced_executions(None).await.unwrap();
    assert!(control.is_cancelled());
    assert!(lifetime.is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while host
            .inner
            .state
            .lock()
            .unwrap()
            .runs
            .get(&entry.run_id)
            .unwrap()
            .active()
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    controller.collect_outcome(&mut entry).unwrap();
    assert_eq!(entry.phase, Phase::Uncertain);
    assert!(matches!(
        entry.report.as_ref().map(|report| &report.outcome),
        Some(ReportOutcome::Uncertain { .. })
    ));
    host.begin_shutdown().unwrap();
    host.wait_stopped().await;
}

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
            local_project_id: None,
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
            conversation_id: "job".into(),
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
            continued_from: None,
            state: JobState::Assigned,
            awaiting_child_id: None,
            revision: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
        },
        child_result: None,
        allowed_child_environments: vec![],
        allowed_child_candidates: vec![],
        retained_services: vec![],
        required_runner_capabilities: vec![],
    }
}

#[test]
fn assignment_capability_requirement_defaults_for_legacy_hub_and_rejects_unknown_features() {
    let mut legacy = serde_json::to_value(assignment()).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("required_runner_capabilities");
    let parsed: Assignment = serde_json::from_value(legacy).unwrap();
    assert!(parsed.required_runner_capabilities.is_empty());
    assert!(parsed.require_supported_capabilities().is_ok());
    let mut current = parsed.clone();
    current.required_runner_capabilities =
        vec![super::protocol::MULTI_DEVICE_SESSION_CAPABILITY.into()];
    assert!(current.require_supported_capabilities().is_ok());
    current.required_runner_capabilities = vec!["newer_protocol_v2".into()];
    assert!(current.require_supported_capabilities().is_err());
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
fn journal_path_recovers_only_an_empty_uninitialized_legacy_file() {
    let (_temp, settings, fixture_path) = fixture();
    let data = fixture_path.parent().unwrap();
    let legacy = data.join("runner-shared.sqlite3");
    let db = rusqlite::Connection::open(&legacy).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
    drop(db);
    assert_eq!(Journal::path_for(data, &settings).unwrap(), legacy);
    let journal = Journal::open(&legacy, &settings).unwrap();
    assert!(journal.active().unwrap().is_empty());
    drop(journal);

    let (_unknown_temp, settings, fixture_path) = fixture();
    let data = fixture_path.parent().unwrap();
    let legacy = data.join("runner-shared.sqlite3");
    let db = rusqlite::Connection::open(&legacy).unwrap();
    db.execute_batch(
        "CREATE TABLE unknown_evidence(value TEXT); INSERT INTO unknown_evidence VALUES('retain');",
    )
    .unwrap();
    drop(db);
    assert!(Journal::path_for(data, &settings).is_err());
    let db = rusqlite::Connection::open(&legacy).unwrap();
    let evidence: String = db
        .query_row("SELECT value FROM unknown_evidence", [], |row| row.get(0))
        .unwrap();
    assert_eq!(evidence, "retain");
}

#[test]
fn new_hub_journal_keeps_old_unknown_work_separate_without_settlement() {
    let (_temp, mut settings, fixture_path) = fixture();
    let data = fixture_path.parent().unwrap();
    let old_path = Journal::path_for(data, &settings).unwrap();
    let mut old = Journal::open(&old_path, &settings).unwrap();
    let mut entry = old
        .intent(assignment(), settings.environments[0].clone())
        .unwrap();
    old.executing(&mut entry).unwrap();
    old.uncertain(&mut entry, "previous execution remains unconfirmed")
        .unwrap();
    drop(old);
    settings.hub_id = "replacement-hub".into();
    settings.device_id = "new-device".into();
    let new_path = Journal::path_for(data, &settings).unwrap();
    assert_ne!(old_path, new_path);
    let new = Journal::open(&new_path, &settings).unwrap();
    assert!(new.active().unwrap().is_empty());
    drop(new);
    settings.hub_id = "hub".into();
    settings.device_id = "device".into();
    assert_eq!(Journal::path_for(data, &settings).unwrap(), old_path);
    let old = Journal::open(&old_path, &settings).unwrap();
    let preserved = old.get("attempt").unwrap().unwrap();
    assert_eq!(preserved.phase, Phase::Uncertain);
    assert_eq!(preserved.run_id, entry.run_id);
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
fn retained_service_receipt_survives_handoff_and_requires_stopped_ack() {
    let (_temp, settings, path) = fixture();
    let mut journal = Journal::open(&path, &settings).unwrap();
    let mut entry = journal
        .intent(assignment(), settings.environments[0].clone())
        .unwrap();
    journal.executing(&mut entry).unwrap();
    let service = crate::tool::shell::RetainedService {
        service_id: ulid::Ulid::new(),
        expires_at_ms: super::super::operations::now_ms() + 60_000,
        retain_after_turn: false,
    };
    let checkpoint = json!({"checkpoint_id":"exact-child"});
    let report = Report::for_assignment(
        &entry.assignment,
        "outcome",
        ReportOutcome::YieldToChild {
            checkpoint: checkpoint.clone(),
            child_environment_id: "solver".into(),
            child_title: "Verify".into(),
            child_input: json!({"version":1,"prompt":"verify"}),
            resources_released: true,
        },
    );
    journal
        .outcome_with_retention(&mut entry, report.clone(), Some(checkpoint), Some(service))
        .unwrap();
    drop(journal);

    let mut journal = Journal::open(&path, &settings).unwrap();
    let mut entry = journal.get("attempt").unwrap().unwrap();
    assert_eq!(entry.retained_service, Some(service));
    assert_eq!(entry.report, Some(report));
    assert_eq!(journal.retained_services().unwrap().len(), 1);
    journal.retention_reported(&mut entry).unwrap();
    journal.settled(&mut entry).unwrap();
    assert!(journal.active().unwrap().is_empty());
    assert_eq!(journal.retained_services().unwrap().len(), 1);
    journal.service_uncertain(&mut entry).unwrap();
    assert_eq!(journal.retained_services().unwrap().len(), 1);
    journal.service_stopped(&mut entry).unwrap();
    assert!(journal.retained_services().unwrap().is_empty());
    assert!(journal.service_stopped(&mut entry).is_ok());
}

#[test]
fn completed_turn_keeps_only_an_explicit_finite_preview_receipt() {
    let (_temp, settings, path) = fixture();
    let mut journal = Journal::open(&path, &settings).unwrap();
    let mut entry = journal
        .intent(assignment(), settings.environments[0].clone())
        .unwrap();
    journal.executing(&mut entry).unwrap();
    let mut service = crate::tool::shell::RetainedService {
        service_id: ulid::Ulid::new(),
        expires_at_ms: super::super::operations::now_ms() + 60_000,
        retain_after_turn: false,
    };
    let completed = Report::for_assignment(
        &entry.assignment,
        "outcome",
        ReportOutcome::Finished {
            success: true,
            result: json!({"text":"Preview is running"}),
            resources_released: true,
        },
    );
    assert!(
        journal
            .outcome_with_retention(&mut entry, completed.clone(), None, Some(service))
            .is_err()
    );
    service.retain_after_turn = true;
    journal
        .outcome_with_retention(&mut entry, completed, None, Some(service))
        .unwrap();
    journal.retention_reported(&mut entry).unwrap();
    journal.settled(&mut entry).unwrap();
    assert!(journal.active().unwrap().is_empty());
    assert_eq!(journal.retained_services().unwrap().len(), 1);
}

#[tokio::test]
async fn shared_completed_turn_keeps_its_preview_process_until_explicit_stop() {
    use crate::runner::Execution;
    use crate::runtime::{OwnedTaskHandle, RunControl};
    use tokio_util::sync::CancellationToken;

    let (_temp, settings, journal_path) = fixture();
    let root = journal_path.parent().unwrap();
    let paths = crate::storage::StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/db.sqlite3"),
        truncation_dir: root.join("data/output"),
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let process =
        crate::app::AppBootstrap::create_process_runtime(crate::storage::StoreBundle::new(sqlite))
            .await
            .unwrap();
    let host = RunnerHost::from_process(process).unwrap();
    let mut journal = Journal::open(&journal_path, &settings).unwrap();
    let mut entry = journal
        .intent(assignment(), settings.environments[0].clone())
        .unwrap();
    journal.executing(&mut entry).unwrap();
    let session_id = crate::session::SessionId::new();
    let lifetime = CancellationToken::new();
    let shells = host
        .inner
        .process
        .managed_shells()
        .with_lifetime(lifetime.clone(), entry.run_id);
    let preview = shells.start_preview_for_test(session_id).await;
    let worker = OwnedTaskHandle::new(1, tokio::spawn(async {}));
    while !worker.is_finished() {
        tokio::task::yield_now().await;
    }
    let summary = crate::session::RunSummary::from_terminal(
        session_id,
        crate::protocol::TurnId::new(),
        crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Completed,
            final_response_id: None,
            tool_call_count: 1,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        },
    );
    host.inner.state.lock().unwrap().runs.insert(
        entry.run_id,
        Execution {
            managed_scope_id: entry.run_id,
            request: LocalRunRequest {
                directory: root.into(),
                prompt: "Start preview".into(),
                session_id: Some(session_id),
                title: None,
                single_agent: true,
            },
            session_id: Some(session_id),
            control: RunControl::new(),
            process_lifetime: lifetime.clone(),
            processes_drained: false,
            service: None,
            worker: Some(worker),
            result: Some(Ok(super::super::ExecutionOutcome::Completed(summary))),
            response: None,
            shared: true,
            resource: None,
        },
    );
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
        settings,
        client: SharedClient::for_test(client),
        journal,
        checkpoint_cursor: String::new(),
        commands: None,
        external: None,
        local_project_id: None,
    };
    controller.collect_outcome(&mut entry).unwrap();
    assert!(!lifetime.is_cancelled());
    assert!(shells.retained_service_live(preview));
    assert_eq!(entry.retained_service, Some(preview));
    assert!(matches!(
        entry.report.as_ref().map(|report| &report.outcome),
        Some(ReportOutcome::Finished { success: true, .. })
    ));
    assert!(shells.cancel_retained_service(preview.service_id));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while shells.has_local_work_in_scope(session_id, entry.run_id) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        shells.retained_service_state(preview),
        crate::tool::shell::RetainedServiceState::Stopped
    );
    use crate::runner::operations::{ReconciliationEvidence, RunnerOperation};
    let reconciliation = |evidence| RunnerOperation::ReconcileRetainedService {
        service_id: preview.service_id.to_string(),
        attempt_id: entry.assignment.attempt_id.clone(),
        generation: entry.assignment.generation,
        reason: "Preview stopped".into(),
        evidence,
    };
    assert!(
        controller
            .operator_request(reconciliation(
                ReconciliationEvidence::OperatorConfirmedStopped {
                    effects_reviewed: true,
                    processes_stopped: true,
                }
            ))
            .await
            .is_err(),
        "Known stopped process must wait for exact worker drain"
    );
    assert!(
        controller
            .operator_request(reconciliation(ReconciliationEvidence::ProcessDrain))
            .await
            .is_ok()
    );
    host.begin_shutdown().unwrap();
    host.wait_stopped().await;
}

#[tokio::test]
async fn hub_stop_fence_drains_a_completed_turns_preview_before_reporting_failure() {
    use crate::runner::Execution;
    use crate::runtime::{OwnedTaskHandle, RunControl};
    use tokio_util::sync::CancellationToken;

    let (_temp, settings, journal_path) = fixture();
    let root = journal_path.parent().unwrap();
    let paths = crate::storage::StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/db.sqlite3"),
        truncation_dir: root.join("data/output"),
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let process =
        crate::app::AppBootstrap::create_process_runtime(crate::storage::StoreBundle::new(sqlite))
            .await
            .unwrap();
    let host = RunnerHost::from_process(process).unwrap();
    let mut journal = Journal::open(&journal_path, &settings).unwrap();
    let mut entry = journal
        .intent(assignment(), settings.environments[0].clone())
        .unwrap();
    journal.executing(&mut entry).unwrap();
    let session_id = crate::session::SessionId::new();
    let lifetime = CancellationToken::new();
    let shells = host
        .inner
        .process
        .managed_shells()
        .with_lifetime(lifetime.clone(), entry.run_id);
    let preview = shells.start_preview_for_test(session_id).await;
    let worker = OwnedTaskHandle::new(1, tokio::spawn(async {}));
    while !worker.is_finished() {
        tokio::task::yield_now().await;
    }
    let control = RunControl::new();
    let summary = crate::session::RunSummary::from_terminal(
        session_id,
        crate::protocol::TurnId::new(),
        crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Completed,
            final_response_id: None,
            tool_call_count: 1,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        },
    );
    host.inner.state.lock().unwrap().runs.insert(
        entry.run_id,
        Execution {
            managed_scope_id: entry.run_id,
            request: LocalRunRequest {
                directory: root.into(),
                prompt: "Start preview".into(),
                session_id: Some(session_id),
                title: None,
                single_agent: true,
            },
            session_id: Some(session_id),
            control: control.clone(),
            process_lifetime: lifetime.clone(),
            processes_drained: false,
            service: None,
            worker: Some(worker),
            result: Some(Ok(super::super::ExecutionOutcome::Completed(summary))),
            response: None,
            shared: true,
            resource: None,
        },
    );
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
        settings,
        client: SharedClient::for_test(client),
        journal,
        checkpoint_cursor: String::new(),
        commands: None,
        external: None,
        local_project_id: None,
    };
    let mut fenced = entry.assignment.clone();
    fenced.stop_requested = true;
    controller
        .stop_fenced_executions(Some(&[fenced]))
        .await
        .unwrap();
    assert!(control.is_cancelled());
    assert!(lifetime.is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while shells.has_local_work_in_scope(session_id, entry.run_id) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        shells.retained_service_state(preview),
        crate::tool::shell::RetainedServiceState::Stopped
    );
    controller.collect_outcome(&mut entry).unwrap();
    assert_eq!(entry.phase, Phase::ReportPending);
    assert!(entry.retained_service.is_none());
    assert!(controller.journal.retained_services().unwrap().is_empty());
    assert!(matches!(
        entry.report.as_ref().map(|report| &report.outcome),
        Some(ReportOutcome::Finished {
            success: false,
            resources_released: true,
            ..
        })
    ));
    host.begin_shutdown().unwrap();
    host.wait_stopped().await;
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
