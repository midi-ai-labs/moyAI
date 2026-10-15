use super::*;
use crate::config::AccessMode;
use crate::protocol::{UserInputItem, UserTurn};
use crate::session::{ProjectId, ProjectRepository, SessionRepository, SessionSettingsPatch};
use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
use camino::Utf8PathBuf;

#[tokio::test]
async fn remote_artifact_outgoing_cache_is_terminal_exact_and_immutable_after_reopen() {
    use crate::remote_agent::artifacts::{
        ArtifactBundle, ArtifactFile, ArtifactManifest, RemoteInputFile,
    };
    let (_temp, store, draft) = fixture().await;
    let session = store.session_repo().create_session(draft).await.unwrap();
    let jobs = store.remote_job_store();
    let mut row = device_reference(session.id, TurnId::new(), "artifact-cache");
    row.job_id = Some(Ulid::new().to_string());
    jobs.accept_device_reference(&row).unwrap();
    let files = vec![ArtifactFile {
        path: "result.txt".into(),
        kind: crate::session::ChangeKind::Add,
        from_path: None,
        base_sha256: None,
        sha256: Some(format!("{:x}", Sha256::digest(b"v1"))),
        byte_length: 2,
    }];
    let version = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(row.job_id.as_ref().unwrap(), &files)).unwrap())
    );
    let bundle = ArtifactBundle {
        manifest: ArtifactManifest {
            job_id: row.job_id.clone().unwrap(),
            version: version.clone(),
            files,
        },
        files: vec![RemoteInputFile {
            path: "result.txt".into(),
            sha256: format!("{:x}", Sha256::digest(b"v1")),
            text: "v1".into(),
        }],
    };
    assert!(
        jobs.cache_artifacts(row.id, &bundle).is_err(),
        "running job cannot freeze an output version"
    );
    row.state = "completed".into();
    jobs.update_device_reference(&row).unwrap();
    jobs.cache_artifacts(row.id, &bundle).unwrap();
    jobs.cache_artifacts(row.id, &bundle).unwrap();
    let mut changed = bundle.clone();
    changed.files[0].text = "v2".into();
    changed.files[0].sha256 = format!("{:x}", Sha256::digest(b"v2"));
    changed.manifest.files[0].sha256 = Some(changed.files[0].sha256.clone());
    changed.manifest.version = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(&changed.manifest.job_id, &changed.manifest.files)).unwrap()
        )
    );
    assert!(jobs.cache_artifacts(row.id, &changed).is_err());
    let reopened = SqliteStore::open(store.paths()).unwrap();
    reopened.migrate().unwrap();
    let read = reopened
        .remote_job_store()
        .cached_artifacts(row.id, row.job_id.as_deref().unwrap(), Some(&version))
        .unwrap()
        .unwrap();
    assert_eq!(read.files[0].text, "v1");
    assert!(
        jobs.cached_artifacts(row.id, "different-job", None)
            .unwrap()
            .is_none()
    );
    assert!(
        jobs.cached_artifacts(Ulid::new(), row.job_id.as_deref().unwrap(), None)
            .unwrap()
            .is_none()
    );
    assert!(
        jobs.cached_artifacts(
            row.id,
            row.job_id.as_deref().unwrap(),
            Some(&"0".repeat(64))
        )
        .is_err()
    );
}

#[tokio::test]
async fn remote_network_receipt_migration_preserves_jobs_and_retries_exact_delivery_after_reopen() {
    let (_temp, store, draft) = fixture().await;
    let jobs = store.remote_job_store();
    let profile = Ulid::new();
    let (legacy, _) = jobs
        .accept(
            "principal",
            profile,
            &request("legacy-receipt"),
            "{}",
            &draft,
        )
        .unwrap();
    {
        let connection = jobs.connection.lock().unwrap();
        connection.execute_batch("DROP TABLE remote_network_receipts; DELETE FROM moyai_schema_migrations WHERE version=64;").unwrap();
        crate::storage::migration::run_to_current(&connection).unwrap();
    }
    assert_eq!(
        jobs.get("principal", legacy.id)
            .unwrap()
            .unwrap()
            .session_id,
        legacy.session_id
    );
    assert!(jobs.pending_network_receipts(8).unwrap().is_empty());
    let (first, created) = jobs
        .accept_with_network(
            "principal",
            profile,
            &request("new-receipt"),
            "{}",
            &draft,
            Some("durable-grant-one"),
        )
        .unwrap();
    assert!(created);
    let replay = jobs
        .accept_with_network(
            "principal",
            profile,
            &request("new-receipt"),
            "{}",
            &draft,
            Some("durable-grant-one"),
        )
        .unwrap();
    assert!(!replay.1);
    assert_eq!(replay.0.id, first.id);
    let pending = jobs.pending_network_receipts(8).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0.id, first.id);
    assert_eq!(pending[0].1, "durable-grant-one");
    jobs.network_receipt_attempt(first.id, "wrong-grant", true)
        .unwrap();
    assert_eq!(jobs.pending_network_receipts(8).unwrap().len(), 1);
    jobs.network_receipt_attempt(first.id, "durable-grant-one", false)
        .unwrap();
    let reopened = SqliteStore::open(store.paths()).unwrap();
    reopened.migrate().unwrap();
    assert_eq!(
        reopened
            .remote_job_store()
            .pending_network_receipts(8)
            .unwrap()[0]
            .0
            .id,
        first.id
    );
    assert!(
        jobs.connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE remote_network_receipts SET grant_id='wrong' WHERE job_id=?1",
                params![first.id.to_string()]
            )
            .is_err()
    );
    jobs.network_receipt_attempt(first.id, "durable-grant-one", true)
        .unwrap();
    jobs.network_receipt_attempt(first.id, "durable-grant-one", false)
        .unwrap();
    assert!(jobs.pending_network_receipts(8).unwrap().is_empty());
    assert_eq!(
        jobs.get("principal", first.id)
            .unwrap()
            .unwrap()
            .admitted_turn_id,
        None
    );
}

#[tokio::test]
async fn remote_network_receipt_failure_rolls_back_job_and_session_creation() {
    let (_temp, store, draft) = fixture().await;
    let jobs = store.remote_job_store();
    let profile = Ulid::new();
    let before = jobs.recent_all(64).unwrap().len();
    assert!(
        jobs.accept_with_network(
            "principal",
            profile,
            &request("invalid-receipt"),
            "{}",
            &draft,
            Some("invalid grant")
        )
        .is_err()
    );
    assert_eq!(jobs.recent_all(64).unwrap().len(), before);
    assert!(jobs.pending_network_receipts(8).unwrap().is_empty());
    assert!(
        jobs.find_request("principal", "invalid-receipt")
            .unwrap()
            .is_none()
    );
}

fn device_reference(session_id: SessionId, turn_id: TurnId, key: &str) -> StoredDeviceReference {
    StoredDeviceReference {
        id: Ulid::new(),
        session_id,
        turn_id,
        device_id: "device-b".into(),
        profile_id: "receiver".into(),
        root_task_id: "root-task".into(),
        request_key: key.into(),
        prompt_hash: format!("{:x}", Sha256::digest(b"request content")),
        parent_grant_id: None,
        parent_job_id: None,
        peer: crate::device_network::DirectoryPeer {
            device_id: "device-b".into(),
            label: "WinB".into(),
            profile_id: "receiver".into(),
            name: "受付".into(),
            endpoint: "https://127.0.0.1:7332/mcp".into(),
            mode: "agent".into(),
            scope_id: "scope".into(),
            certificate_pem: "PUBLIC_CERTIFICATE_FIXTURE".into(),
            certificate_sha256: "a".repeat(64),
        },
        claims: None,
        job_id: None,
        state: "preparing".into(),
        stop_status: "none".into(),
        result: None,
    }
}

#[tokio::test]
async fn device_outgoing_reference_shutdown_stop_is_durable_and_survives_late_poll() {
    let (_temp, store, draft) = fixture().await;
    let session = store.session_repo().create_session(draft).await.unwrap();
    let jobs = store.remote_job_store();
    let mut unfinished = Vec::new();
    let mut finished = Vec::new();
    for state in [
        "preparing",
        "running",
        "unknown",
        "cancelling",
        "completed",
        "failed",
        "interrupted",
    ] {
        let mut row = device_reference(session.id, TurnId::new(), state);
        row.state = state.into();
        if state == "cancelling" {
            row.stop_status = "requested".into();
        }
        jobs.accept_device_reference(&row).unwrap();
        if matches!(state, "completed" | "failed" | "interrupted") {
            finished.push(row);
        } else {
            unfinished.push(row);
        }
    }
    jobs.mark_device_cancellations_unconfirmed().unwrap();
    let reopened = SqliteStore::open(store.paths()).unwrap();
    for row in &unfinished {
        let saved = reopened
            .remote_job_store()
            .device_reference(row.id)
            .unwrap()
            .unwrap();
        assert_eq!(saved.state, row.state);
        assert_eq!(saved.stop_status, "unconfirmed");
    }
    for row in &finished {
        assert_eq!(
            serde_json::to_value(jobs.device_reference(row.id).unwrap().unwrap()).unwrap(),
            serde_json::to_value(row).unwrap()
        );
    }
    let mut late_poll = unfinished[1].clone();
    late_poll.result = Some("observation started before shutdown".into());
    jobs.update_device_reference(&late_poll).unwrap();
    let saved = jobs.device_reference(late_poll.id).unwrap().unwrap();
    assert_eq!(saved.stop_status, "unconfirmed");
    assert_eq!(saved.result, late_poll.result);
    // Only a later terminal observation explicitly confirms termination.
    let mut confirmed = saved;
    confirmed.state = "interrupted".into();
    confirmed.stop_status = "confirmed".into();
    jobs.update_device_reference(&confirmed).unwrap();
    jobs.mark_device_cancellations_unconfirmed().unwrap();
    let saved = jobs.device_reference(confirmed.id).unwrap().unwrap();
    assert_eq!(saved.state, "interrupted");
    assert_eq!(saved.stop_status, "confirmed");
}

#[tokio::test]
async fn device_outgoing_reference_exact_lookup_outlives_recent_projection_and_rejects_ambiguity() {
    let (_temp, store, draft) = fixture().await;
    let session = store.session_repo().create_session(draft).await.unwrap();
    let jobs = store.remote_job_store();
    let mut old = device_reference(session.id, TurnId::new(), "saved-request");
    old.id = Ulid::from(0);
    old.state = "completed".into();
    old.job_id = Some("saved-job".into());
    jobs.accept_device_reference(&old).unwrap();
    for n in 0..70 {
        let mut row = device_reference(session.id, TurnId::new(), &format!("newer-{n}"));
        row.state = "completed".into();
        jobs.accept_device_reference(&row).unwrap();
    }
    assert!(
        !jobs
            .device_references(Some(session.id), 64)
            .unwrap()
            .iter()
            .any(|r| r.id == old.id)
    );
    for (key, job) in [(Some("saved-request"), None), (None, Some("saved-job"))] {
        assert_eq!(
            jobs.find_device_reference(session.id, key, job)
                .unwrap()
                .unwrap()
                .id,
            old.id
        );
        assert!(
            jobs.find_device_reference(SessionId::new(), key, job)
                .unwrap()
                .is_none()
        );
    }
    assert!(jobs.find_device_reference(session.id, None, None).is_err());
    assert!(
        jobs.find_device_reference(session.id, Some("saved-request"), Some("saved-job"))
            .is_err()
    );
    assert!(
        jobs.find_device_reference(session.id, Some(""), None)
            .is_err()
    );
    assert!(
        jobs.find_device_reference(session.id, Some("missing"), None)
            .unwrap()
            .is_none()
    );

    let mut collision = old.clone();
    collision.id = Ulid::new();
    collision.device_id = "device-c".into();
    collision.peer.device_id = collision.device_id.clone();
    jobs.accept_device_reference(&collision).unwrap();
    assert!(
        jobs.find_device_reference(session.id, Some("saved-request"), None)
            .is_err()
    );
    assert!(
        jobs.find_device_reference(session.id, None, Some("saved-job"))
            .is_err()
    );
    // An explicit immutable locator still identifies each record without ambiguity.
    assert_eq!(
        jobs.device_reference(old.id).unwrap().unwrap().device_id,
        "device-b"
    );
    assert_eq!(
        jobs.device_reference(collision.id)
            .unwrap()
            .unwrap()
            .device_id,
        "device-c"
    );
}

#[tokio::test]
async fn device_outgoing_reference_replay_is_atomic_and_content_identity_is_immutable() {
    let (_temp, store, draft) = fixture().await;
    let session = store.session_repo().create_session(draft).await.unwrap();
    let jobs = store.remote_job_store();
    let row = device_reference(session.id, TurnId::new(), "one");
    let accepted = jobs.accept_device_reference(&row).unwrap();
    let mut replay = row.clone();
    replay.id = Ulid::new();
    replay.peer.name = "refreshed label".into();
    assert_eq!(
        jobs.accept_device_reference(&replay).unwrap().id,
        accepted.id
    );
    for mutate in [0, 1, 2] {
        let mut changed = replay.clone();
        match mutate {
            0 => changed.prompt_hash = "b".repeat(64),
            1 => changed.root_task_id = "another-task".into(),
            _ => {
                changed.parent_grant_id = Some("parent-grant".into());
                changed.parent_job_id = Some("parent-job".into());
            }
        }
        assert!(jobs.accept_device_reference(&changed).is_err());
    }
    assert_eq!(jobs.device_references(None, 64).unwrap().len(), 1);
    let sessions: i64 = jobs
        .connection
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sessions, 1);
    let mut result = accepted.clone();
    result.peer.endpoint = "https://127.0.0.1:7443/mcp".into();
    result.job_id = Some("real-remote-job".into());
    result.state = "completed".into();
    result.result = Some("CPU 12.96%".into());
    jobs.update_device_reference(&result).unwrap();
    let saved = jobs.device_reference(accepted.id).unwrap().unwrap();
    assert_eq!(saved.job_id.as_deref(), Some("real-remote-job"));
    assert_eq!(saved.result.as_deref(), Some("CPU 12.96%"));
    let mut changed = result.clone();
    changed.job_id = Some("replacement-job".into());
    assert!(jobs.update_device_reference(&changed).is_err());
    changed = result.clone();
    changed.root_task_id = "wrong-task".into();
    assert!(jobs.update_device_reference(&changed).is_err());
    // Independent SQL callers cannot bypass the immutable identity constraint.
    assert!(
        jobs.connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE device_outgoing_references SET request_key='changed' WHERE id=?1",
                params![accepted.id.to_string()]
            )
            .is_err()
    );
}

#[tokio::test]
async fn device_outgoing_reference_survives_deleted_local_history_and_reopen_without_credentials() {
    let (_temp, store, draft) = fixture().await;
    let paths = store.paths().clone();
    let session = store.session_repo().create_session(draft).await.unwrap();
    let row = device_reference(session.id, TurnId::new(), "one");
    let jobs = store.remote_job_store();
    jobs.accept_device_reference(&row).unwrap();
    store
        .session_repo()
        .delete_session(session.id)
        .await
        .unwrap();
    assert_eq!(
        jobs.device_references(None, 64).unwrap()[0].session_id,
        session.id
    );
    let sqlite = SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let reopened = sqlite
        .remote_job_store()
        .device_reference(row.id)
        .unwrap()
        .unwrap();
    assert_eq!(reopened.request_key, row.request_key);
    let encoded = serde_json::to_value(&reopened).unwrap();
    let object = encoded.as_object().unwrap();
    assert!(!object.contains_key("token"));
    assert!(!object.contains_key("private_key_pem"));
    assert!(!object.contains_key("prompt"));
    let mut with_secret = encoded;
    with_secret["peer"]["private_key_pem"] = serde_json::json!("SECRET_FIELD_MUST_NOT_BE_ACCEPTED");
    assert!(serde_json::from_value::<StoredDeviceReference>(with_secret).is_err());
}

#[tokio::test]
async fn device_outgoing_reference_active_rows_are_not_hidden_by_recent_terminal_results() {
    let (_temp, store, draft) = fixture().await;
    let session = store.session_repo().create_session(draft).await.unwrap();
    let jobs = store.remote_job_store();
    let active = device_reference(session.id, TurnId::new(), "active");
    jobs.accept_device_reference(&active).unwrap();
    for n in 0..70 {
        let mut terminal = device_reference(session.id, TurnId::new(), &format!("terminal-{n}"));
        terminal.state = "completed".into();
        jobs.accept_device_reference(&terminal).unwrap();
    }
    let rows = jobs.device_references(None, usize::MAX).unwrap();
    assert_eq!(rows.len(), 64);
    assert_eq!(rows[0].id, active.id);
    assert_eq!(
        jobs.device_references(Some(session.id), 64).unwrap()[0].id,
        active.id
    );
    assert!(
        jobs.device_references(Some(SessionId::new()), 64)
            .unwrap()
            .is_empty()
    );
    let mut oversized = active.clone();
    oversized.result = Some("x".repeat(64 * 1024 + 1));
    assert!(jobs.update_device_reference(&oversized).is_err());
    let mut malformed = serde_json::to_value(&active).unwrap();
    malformed["unknown_new_authority"] = serde_json::json!(true);
    let encoded = serde_json::to_string(&malformed).unwrap();
    jobs.connection
        .lock()
        .unwrap()
        .execute(
            "UPDATE device_outgoing_references SET payload_json=?1 WHERE id=?2",
            params![encoded, active.id.to_string()],
        )
        .unwrap();
    assert!(jobs.device_reference(active.id).is_err());
    let unchanged: String = jobs
        .connection
        .lock()
        .unwrap()
        .query_row(
            "SELECT payload_json FROM device_outgoing_references WHERE id=?1",
            params![active.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unchanged, encoded);
}

#[tokio::test]
async fn device_outgoing_reference_migration_advances_v62_without_rewriting_receiver_jobs() {
    let (_temp, store, draft) = fixture().await;
    let jobs = store.remote_job_store();
    let profile = Ulid::new();
    let (receiver, _) = jobs
        .accept("principal", profile, &request("before-v63"), "{}", &draft)
        .unwrap();
    {
        let connection = jobs.connection.lock().unwrap();
        connection.execute_batch("DROP TABLE device_outgoing_references; DELETE FROM moyai_schema_migrations WHERE version=63;").unwrap();
        crate::storage::migration::run_to_current(&connection).unwrap();
    }
    assert_eq!(
        jobs.get("principal", receiver.id)
            .unwrap()
            .unwrap()
            .session_id,
        receiver.session_id
    );
    let marker:i64=jobs.connection.lock().unwrap().query_row("SELECT COUNT(*) FROM moyai_schema_migrations WHERE version=63 AND name='device_outgoing_references'",[],|r|r.get(0)).unwrap();
    assert_eq!(marker, 1);
    let row = device_reference(receiver.session_id, TurnId::new(), "after-v63");
    jobs.accept_device_reference(&row).unwrap();
    assert_eq!(
        jobs.device_reference(row.id).unwrap().unwrap().request_key,
        "after-v63"
    );
}

async fn fixture() -> (tempfile::TempDir, StoreBundle, NewSession) {
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
    let paths = StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/db.sqlite3"),
        truncation_dir: root.join("data/output"),
    };
    let sqlite = SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let store = StoreBundle::new(sqlite);
    let project = ProjectId::new();
    store
        .project_repo()
        .upsert_project(project, &root, "remote fixture", "none")
        .await
        .unwrap();
    (
        temp,
        store,
        NewSession {
            project_id: project,
            title: "remote fixture".into(),
            cwd: root,
            model: "fixture-model".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            access_mode: AccessMode::Default,
            provider_connection: None,
        },
    )
}

fn request(key: &str) -> RemoteTaskRequest {
    RemoteTaskRequest {
        request_key: key.into(),
        parent: RemoteJobParent {
            peer_id: "WinA".into(),
            task_id: "parent-task".into(),
            turn_id: "parent-turn".into(),
        },
        prompt: "この端末で時刻を確認してください。".into(),
        inputs: vec![],
    }
}

fn user_turn(id: TurnId) -> UserTurn {
    UserTurn {
        turn_id: id,
        items: vec![UserInputItem::Text {
            text: "receiver task".into(),
        }],
        prompt_dispatch: None,
        editor_context: None,
    }
}

#[tokio::test]
async fn remote_job_acceptance_is_atomic_and_replay_is_scoped_to_principal() {
    let (_temp, store, draft) = fixture().await;
    let repository = store.remote_job_store();
    let profile = Ulid::new();
    let (first, created) = repository
        .accept("principal-a", profile, &request("one"), "{}", &draft)
        .unwrap();
    assert!(created);
    let (replay, created) = repository
        .accept("principal-a", profile, &request("one"), "{}", &draft)
        .unwrap();
    assert!(!created);
    assert_eq!(first.id, replay.id);
    assert_eq!(first.session_id, replay.session_id);
    assert!(repository.get("principal-b", first.id).unwrap().is_none());
    let (independent, created) = repository
        .accept("principal-b", profile, &request("one"), "{}", &draft)
        .unwrap();
    assert!(created);
    assert_ne!(first.session_id, independent.session_id);
    let mut changed = request("one");
    changed.prompt.push_str("変更");
    assert!(
        repository
            .accept("principal-a", profile, &changed, "{}", &draft)
            .is_err()
    );
    assert!(
        repository
            .accept(
                "principal-a",
                profile,
                &request("one"),
                "{\"scope\":2}",
                &draft
            )
            .is_err()
    );
    let count_before: i64 = repository
        .connection
        .lock()
        .unwrap()
        .query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert!(
        repository
            .accept(
                "principal-a",
                profile,
                &request("invalid-scope"),
                "not json",
                &draft
            )
            .is_err()
    );
    let count_after: i64 = repository
        .connection
        .lock()
        .unwrap()
        .query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        count_before, count_after,
        "mapping failure rolls back its session"
    );
    assert_eq!(repository.recent_all(64).unwrap().len(), 2);
}

#[tokio::test]
async fn remote_job_claim_and_canonical_turn_are_atomic_and_cannot_be_borrowed() {
    let (_temp, store, draft) = fixture().await;
    let repository = store.remote_job_store();
    let (job, _) = repository
        .accept("principal", Ulid::new(), &request("claim"), "{}", &draft)
        .unwrap();
    let wrong_turn = TurnId::new();
    assert!(
        store
            .session_repo()
            .admit_session_turn_with_initial_user_turn_after_latest(
                job.session_id,
                wrong_turn,
                Some(&user_turn(wrong_turn)),
                None,
                0
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .session_repo()
            .admit_remote_task(
                job.session_id,
                wrong_turn,
                Ulid::new(),
                &user_turn(wrong_turn)
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .get("principal", job.id)
            .unwrap()
            .unwrap()
            .admitted_turn_id
            .is_none()
    );
    let turn = TurnId::new();
    let admitted = store
        .session_repo()
        .admit_remote_task(job.session_id, turn, job.id, &user_turn(turn))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(admitted.admission_revision, 1);
    assert_eq!(
        repository
            .get("principal", job.id)
            .unwrap()
            .unwrap()
            .admitted_turn_id,
        Some(turn)
    );
    let replay_turn = TurnId::new();
    assert!(
        store
            .session_repo()
            .admit_remote_task(job.session_id, replay_turn, job.id, &user_turn(replay_turn))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE remote_agent_jobs SET request_hash = ?1 WHERE id = ?2",
                params!["a".repeat(64), job.id.to_string()]
            )
            .is_err()
    );
}

#[tokio::test]
async fn remote_job_receipt_survives_reopen_without_starting_a_session() {
    let (_temp, store, draft) = fixture().await;
    let profile = Ulid::new();
    let (job, _) = store
        .remote_job_store()
        .accept("principal", profile, &request("reopen"), "{}", &draft)
        .unwrap();
    let reopened = SqliteStore::open(store.paths()).unwrap();
    reopened.migrate().unwrap();
    let (receipt, created) = reopened
        .remote_job_store()
        .accept("principal", profile, &request("reopen"), "{}", &draft)
        .unwrap();
    assert!(!created);
    assert_eq!(receipt.id, job.id);
    assert_eq!(receipt.admitted_turn_id, None);
    assert_eq!(
        reopened
            .remote_job_store()
            .job_id_for_session(job.session_id)
            .unwrap(),
        Some(job.id)
    );
}

#[test]
fn remote_request_has_strict_bounded_shape() {
    let mut request = request("valid-key");
    assert!(request.validate());
    request.prompt = "x".repeat(MAX_REMOTE_PROMPT_BYTES + 1);
    assert!(!request.validate());
    assert!(serde_json::from_value::<RemoteTaskRequest>(serde_json::json!({"request_key":"key","parent":{"peer_id":"a","task_id":"t","turn_id":"u","access_mode":"full_access"},"prompt":"task"})).is_err());
}

#[tokio::test]
async fn remote_sessions_are_excluded_from_human_discovery_and_root_setting_cas() {
    let (_temp, store, draft) = fixture().await;
    let (job, _) = store
        .remote_job_store()
        .accept("principal", Ulid::new(), &request("hidden"), "{}", &draft)
        .unwrap();
    let repository = store.session_repo();
    assert!(
        repository
            .list_sessions(draft.project_id, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repository
            .list_sessions_with_archived(draft.project_id, 100, true)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repository
            .list_recent_sessions(100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repository
            .latest_session(draft.project_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .search_sessions(draft.project_id, "remote", 100, true)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repository
            .list_sessions_with_projection_state(draft.project_id, 100, true)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repository
            .compare_and_set_root_session_access_mode(
                job.session_id,
                AccessMode::Default,
                AccessMode::FullAccess
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("remote job session")
    );
    assert!(
        repository
            .compare_and_set_root_session_settings(
                job.session_id,
                0,
                &SessionSettingsPatch {
                    access_mode: Some(AccessMode::FullAccess),
                    ..Default::default()
                }
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("remote job session")
    );
    let session = repository.get_session(job.session_id).await.unwrap();
    assert_eq!(session.access_mode, AccessMode::Default);
    assert_eq!(session.session_settings_revision, 0);
}

#[tokio::test]
async fn remote_temp_project_purpose_is_atomic_immutable_and_keeps_real_projects_visible() {
    let (_temp, store, draft) = fixture().await;
    let projects = store.project_repo();
    let profile = Ulid::new();
    assert!(
        projects
            .upsert_remote_temp_project(draft.project_id, &draft.cwd, "must not convert", profile)
            .await
            .is_err()
    );
    let temp_project = ProjectId::new();
    let temp_path = draft.cwd.join("internal-temp");
    projects
        .upsert_remote_temp_project(temp_project, &temp_path, "internal", profile)
        .await
        .unwrap();
    assert!(
        projects
            .upsert_remote_temp_project(temp_project, &temp_path, "other owner", Ulid::new())
            .await
            .is_err()
    );
    projects
        .upsert_project(
            temp_project,
            &temp_path,
            "normal upsert keeps purpose",
            "none",
        )
        .await
        .unwrap();
    let visible = projects.list_projects(64).await.unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].id, draft.project_id);
    assert_eq!(
        projects.get_project(temp_project).await.unwrap().root_path,
        temp_path
    );
    assert!(
        store
            .remote_job_store()
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE projects SET remote_temp_profile_id = NULL WHERE id = ?1",
                params![temp_project.to_string()]
            )
            .is_err()
    );
    let reopened = SqliteStore::open(store.paths()).unwrap();
    reopened.migrate().unwrap();
    assert_eq!(
        reopened
            .project_repo()
            .list_projects(64)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn remote_job_prevents_project_delete_without_affecting_ordinary_projects() {
    let (_temp, store, draft) = fixture().await;
    let jobs = store.remote_job_store();
    let (job, _) = jobs
        .accept("principal", Ulid::new(), &request("preserve"), "{}", &draft)
        .unwrap();
    let human = store
        .session_repo()
        .create_session(draft.clone())
        .await
        .unwrap();
    jobs.connection
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO protocol_turn_sequence_allocators
         (session_id, turn_id, next_sequence_no) VALUES (?1, 'human-turn', 1)",
            params![human.id.to_string()],
        )
        .unwrap();

    let error = store
        .project_repo()
        .delete_project(draft.project_id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("遠隔タスクの実行記録"));
    assert!(
        store
            .project_repo()
            .get_project(draft.project_id)
            .await
            .is_ok()
    );
    assert!(store.session_repo().get_session(human.id).await.is_ok());
    assert!(
        store
            .session_repo()
            .get_session(job.session_id)
            .await
            .is_ok()
    );
    assert!(jobs.get("principal", job.id).unwrap().is_some());
    let allocator_count: i64 = jobs
        .connection
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM protocol_turn_sequence_allocators WHERE session_id = ?1",
            params![human.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(allocator_count, 1);

    let ordinary_project = ProjectId::new();
    let ordinary_root = draft.cwd.join("ordinary");
    store
        .project_repo()
        .upsert_project(ordinary_project, &ordinary_root, "ordinary", "none")
        .await
        .unwrap();
    let ordinary = store
        .session_repo()
        .create_session(NewSession {
            project_id: ordinary_project,
            cwd: ordinary_root,
            ..draft
        })
        .await
        .unwrap();
    store
        .project_repo()
        .delete_project(ordinary_project)
        .await
        .unwrap();
    assert!(
        store
            .project_repo()
            .get_project(ordinary_project)
            .await
            .is_err()
    );
    assert!(store.session_repo().get_session(ordinary.id).await.is_err());
    assert!(jobs.get("principal", job.id).unwrap().is_some());
}
