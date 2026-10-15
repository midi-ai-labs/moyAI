use super::*;
use crate::session::{ChangeRepository, ProjectRepository};

async fn fixture() -> (tempfile::TempDir, StoreBundle, crate::session::NewSession) {
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
    let paths = crate::storage::StoragePaths {
        data_dir: root.join("data"),
        database_path: root.join("data/db.sqlite3"),
        truncation_dir: root.join("data/output"),
    };
    let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let store = StoreBundle::new(sqlite);
    let project = crate::session::ProjectId::new();
    store
        .project_repo()
        .upsert_project(project, &root, "artifact fixture", "none")
        .await
        .unwrap();
    (
        temp,
        store,
        crate::session::NewSession {
            project_id: project,
            title: "artifact fixture".into(),
            cwd: root,
            model: "fixture".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            access_mode: crate::config::AccessMode::Default,
            provider_connection: None,
        },
    )
}
fn request(files: Vec<RemoteInputFile>) -> super::super::RemoteTaskRequest {
    super::super::RemoteTaskRequest {
        request_key: "artifact-request".into(),
        parent: super::super::RemoteJobParent {
            peer_id: "a".into(),
            task_id: "task".into(),
            turn_id: "turn".into(),
        },
        prompt: "work".into(),
        inputs: files,
    }
}

#[tokio::test]
async fn remote_artifact_input_versions_are_atomic_durable_and_never_install_into_workspace() {
    let (_temp, store, draft) = fixture().await;
    let jobs = store.remote_job_store();
    let profile = Ulid::new();
    let request = request(vec![input("src/input.txt", "original")]);
    let (job, created) = jobs
        .accept("principal", profile, &request, "{}", &draft)
        .unwrap();
    assert!(created);
    assert!(!draft.cwd.join("src/input.txt").exists());
    let staged = stage_inputs(&jobs, &job).unwrap().unwrap();
    assert!(!staged.root.starts_with(&draft.cwd));
    assert_eq!(
        std::fs::read_to_string(staged.root.join("src/input.txt")).unwrap(),
        "original"
    );
    assert!(staged.context().contains(&digest("original")));
    std::fs::write(staged.root.join("src/input.txt"), "local temporary edit").unwrap();
    let reopened = crate::storage::SqliteStore::open(store.paths()).unwrap();
    reopened.migrate().unwrap();
    let again = stage_inputs(&reopened.remote_job_store(), &job)
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(again.root.join("src/input.txt")).unwrap(),
        "original"
    );
    let mut changed = request.clone();
    changed.inputs[0] = input("src/input.txt", "different version");
    assert!(
        jobs.accept("principal", profile, &changed, "{}", &draft)
            .is_err()
    );
    assert!(
        jobs.connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE remote_job_inputs SET payload_json='[]' WHERE job_id=?1",
                params![job.id.to_string()]
            )
            .is_err()
    );
    assert!(jobs.get("other-principal", job.id).unwrap().is_none());
}

#[tokio::test]
async fn remote_artifact_forward_migration_preserves_old_job_and_rejects_partial_schema() {
    let (_temp, store, draft) = fixture().await;
    let jobs = store.remote_job_store();
    let (old, _) = jobs
        .accept("principal", Ulid::new(), &request(vec![]), "{}", &draft)
        .unwrap();
    {
        let connection = jobs.connection.lock().unwrap();
        connection.execute_batch("DROP TABLE remote_job_inputs; DROP TABLE remote_job_artifacts; DROP TABLE device_artifact_cache; DELETE FROM moyai_schema_migrations WHERE version=65;").unwrap();
        crate::storage::migration::run_to_current(&connection).unwrap();
    }
    assert!(stage_inputs(&jobs, &old).unwrap().is_none());
    assert_eq!(
        jobs.get("principal", old.id).unwrap().unwrap().session_id,
        old.session_id
    );
    let connection = jobs.connection.lock().unwrap();
    connection
        .execute_batch("DROP TRIGGER remote_job_artifacts_immutable")
        .unwrap();
    assert!(crate::storage::migration::run_to_current(&connection).is_err());
}

#[tokio::test]
async fn remote_artifact_snapshot_uses_only_settled_canonical_changes_and_survives_reopen() {
    use crate::protocol::*;
    let (_temp, store, draft) = fixture().await;
    let jobs = store.remote_job_store();
    let (mut job, _) = jobs
        .accept("principal", Ulid::new(), &request(vec![]), "{}", &draft)
        .unwrap();
    let turn = TurnId::new();
    store
        .session_repo()
        .admit_remote_task(
            job.session_id,
            turn,
            job.id,
            &UserTurn {
                turn_id: turn,
                items: vec![UserInputItem::Text {
                    text: "work".into(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    job = jobs.get("principal", job.id).unwrap().unwrap();
    let protocol = store.protocol_event_store();
    let declared = change("declared.txt", None, Some("delivered"));
    std::fs::write(draft.cwd.join("declared.txt"), "delivered").unwrap();
    let tool_history = HistoryItem {
        id: HistoryItemId::new(),
        session_id: job.session_id,
        scope: HistoryScope::Turn { turn_id: turn },
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::ToolCall {
            call_id: declared.tool_call_id,
            response_id: ModelResponseId::new(),
            model_call_id: "call".into(),
            tool_name: "write".into(),
            arguments_json: "{}".into(),
        },
    };
    protocol.seed_history_item_for_test(&tool_history).unwrap();
    jobs.connection.lock().unwrap().execute("INSERT INTO tool_calls(id,history_item_id,status,started_at_ms) VALUES(?1,?2,'completed',1)",params![declared.tool_call_id.to_string(),tool_history.id.to_string()]).unwrap();
    let mut unreferenced = change("not-declared.txt", None, Some("hidden"));
    unreferenced.tool_call_id = declared.tool_call_id;
    std::fs::write(draft.cwd.join("not-declared.txt"), "hidden").unwrap();
    store
        .change_repo()
        .insert_changes(&[declared.clone(), unreferenced])
        .await
        .unwrap();
    protocol
        .seed_history_item_for_test(&HistoryItem {
            id: HistoryItemId::new(),
            session_id: job.session_id,
            scope: HistoryScope::Turn { turn_id: turn },
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::FileChange {
                call_id: declared.tool_call_id,
                change_ids: vec![declared.id],
                changes: vec![],
                summary: "declared".into(),
            },
        })
        .unwrap();
    assert!(capture_outputs(&store, &job, &workspace(&draft.cwd)).is_err());
    protocol
        .seed_runtime_event_for_test(&RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id: job.session_id,
            turn_id: turn,
            sequence_no: 3,
            created_at_ms: 3,
            msg: RuntimeEventMsg::TurnTerminal {
                terminal: Box::new(crate::session::DurableTurnTerminal {
                    outcome: TurnTerminalOutcome::Completed,
                    final_response_id: None,
                    tool_call_count: 1,
                    failed_tool_count: 0,
                    change_count: 1,
                    metrics: Default::default(),
                }),
            },
        })
        .unwrap();
    let manifest = capture_outputs(&store, &job, &workspace(&draft.cwd)).unwrap();
    assert_eq!(manifest.files.len(), 1);
    assert_eq!(manifest.files[0].path, "declared.txt");
    std::fs::write(draft.cwd.join("declared.txt"), "later unrelated edit").unwrap();
    let reopened = crate::storage::SqliteStore::open(store.paths()).unwrap();
    reopened.migrate().unwrap();
    let bundle = reopened
        .remote_job_store()
        .artifact_bundle(job.id, Some(&manifest.version))
        .unwrap()
        .unwrap();
    assert_eq!(bundle.files[0].text, "delivered");
    assert!(jobs.artifact_bundle(job.id, Some(&"0".repeat(64))).is_err());
    assert!(
        jobs.connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE remote_job_artifacts SET version=?1 WHERE job_id=?2",
                params!["0".repeat(64), job.id.to_string()]
            )
            .is_err()
    );
}

fn input(path: &str, text: &str) -> RemoteInputFile {
    RemoteInputFile {
        path: path.into(),
        sha256: digest(text.as_bytes()),
        text: text.into(),
    }
}

#[test]
fn remote_artifact_inputs_are_explicit_bounded_portable_versions() {
    assert!(validate_inputs(&[input("src/日本語.rs", "fn main() {}")]).is_ok());
    for path in [
        "../secret",
        "/absolute",
        "C:/data",
        "a\\b",
        "a/../b",
        "CON.txt",
        "a.",
        "a:b",
        "",
    ] {
        assert!(validate_inputs(&[input(path, "x")]).is_err(), "{path}");
    }
    assert!(validate_inputs(&[input("a", "x"), input("A", "x")]).is_err());
    assert!(validate_inputs(&[input("a", "x"), input("a/b", "x")]).is_err());
    assert!(validate_inputs(&[input("a", &"x".repeat(MAX_ARTIFACT_FILE_BYTES + 1))]).is_err());
    assert!(
        validate_inputs(
            &(0..9)
                .map(|i| input(&format!("{i}.txt"), "x"))
                .collect::<Vec<_>>()
        )
        .is_err()
    );
    assert!(
        validate_inputs(
            &(0..5)
                .map(|i| input(&format!("{i}.txt"), &"x".repeat(MAX_ARTIFACT_FILE_BYTES)))
                .collect::<Vec<_>>()
        )
        .is_err()
    );
    let mut tampered = input("a", "original");
    tampered.text = "changed".into();
    assert!(validate_inputs(&[tampered]).is_err());
    // Raw UTF-8 remains within 256 KiB, but JSON escaping must also fit MCP's 1 MiB frame.
    let escaped = (0..4)
        .map(|i| {
            input(
                &format!("{i}.txt"),
                &"\u{1}".repeat(MAX_ARTIFACT_FILE_BYTES),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        escaped.iter().map(|file| file.text.len()).sum::<usize>(),
        MAX_ARTIFACT_TOTAL_BYTES
    );
    assert!(validate_inputs(&escaped).is_err());
}

#[test]
fn remote_artifact_empty_input_preserves_legacy_request_fingerprint() {
    let legacy = serde_json::json!({"request_key":"key","parent":{"peer_id":"a","task_id":"task","turn_id":"turn"},"prompt":"work"});
    let request: super::super::RemoteTaskRequest = serde_json::from_value(legacy).unwrap();
    assert!(request.inputs.is_empty());
    #[derive(Serialize)]
    struct OldRequest<'a> {
        request_key: &'a str,
        parent: &'a super::super::RemoteJobParent,
        prompt: &'a str,
    }
    let old = OldRequest {
        request_key: &request.request_key,
        parent: &request.parent,
        prompt: &request.prompt,
    };
    assert_eq!(
        request.fingerprint("scope").unwrap(),
        digest(serde_json::to_vec(&(old, "scope")).unwrap())
    );
    let mut changed = request.clone();
    changed.inputs.push(input("a.txt", "input version"));
    assert_ne!(
        changed.fingerprint("scope").unwrap(),
        request.fingerprint("scope").unwrap()
    );
}

fn workspace(root: &Utf8Path) -> Workspace {
    crate::workspace::WorkspaceDiscovery::discover_fixed_root(
        root,
        &crate::config::ResolvedConfig::default(),
    )
    .unwrap()
}
fn change(path: &str, before: Option<&str>, after: Option<&str>) -> crate::edit::FileChange {
    crate::edit::FileChange {
        id: crate::session::ChangeId::new(),
        tool_call_id: crate::session::ToolCallId::new(),
        kind: if before.is_none() {
            ChangeKind::Add
        } else if after.is_none() {
            ChangeKind::Delete
        } else {
            ChangeKind::Update
        },
        path_before: before.map(|_| path.into()),
        path_after: after.map(|_| path.into()),
        before_sha256: before.map(digest),
        after_sha256: after.map(digest),
        diff_text: String::new(),
        summary: String::new(),
        created_at_ms: 0,
    }
}

#[test]
fn remote_artifact_snapshot_checks_canonical_hash_and_exports_only_declared_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    std::fs::write(root.join("output.txt"), "v2").unwrap();
    std::fs::write(root.join("private.txt"), "not an output").unwrap();
    let bundle = snapshot_changes(
        Ulid::new(),
        &workspace(root),
        &[change("output.txt", Some("v1"), Some("v2"))],
    )
    .unwrap();
    assert_eq!(bundle.files.len(), 1);
    assert_eq!(bundle.manifest.files[0].base_sha256, Some(digest("v1")));
    assert_eq!(bundle.files[0].text, "v2");
    let destination = root.join("export");
    #[cfg(windows)]
    {
        export_bundle(&bundle, &destination).unwrap();
        assert_eq!(
            std::fs::read_to_string(destination.join("files/output.txt")).unwrap(),
            "v2"
        );
        assert!(!destination.join("files/private.txt").exists());
        assert!(export_bundle(&bundle, &destination).is_err());
    }
    #[cfg(not(windows))]
    {
        assert!(export_bundle(&bundle, &destination).is_err());
        assert!(!destination.exists());
    }
    let mut tampered = bundle.clone();
    tampered.files[0].text = "secret".into();
    assert!(export_bundle(&tampered, &root.join("bad")).is_err());
    assert!(!root.join("bad").exists());
    std::fs::write(root.join("output.txt"), "external edit").unwrap();
    assert!(
        snapshot_changes(
            Ulid::new(),
            &workspace(root),
            &[change("output.txt", Some("v1"), Some("v2"))]
        )
        .is_err()
    );
    assert_eq!(bundle.files[0].text, "v2");
}

#[test]
fn remote_artifact_delete_and_move_keep_version_metadata_without_arbitrary_reads() {
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    std::fs::write(root.join("new.txt"), "moved").unwrap();
    let mut moved = change("new.txt", Some("moved"), Some("moved"));
    moved.kind = ChangeKind::Move;
    moved.path_before = Some("old.txt".into());
    let bundle = snapshot_changes(
        Ulid::new(),
        &workspace(root),
        &[change("deleted.txt", Some("old"), None), moved],
    )
    .unwrap();
    assert_eq!(bundle.files.len(), 1);
    assert_eq!(bundle.manifest.files[0].kind, ChangeKind::Delete);
    assert_eq!(
        bundle.manifest.files[1].from_path.as_deref(),
        Some("old.txt")
    );
    assert!(
        snapshot_changes(
            Ulid::new(),
            &workspace(root),
            &[change("../outside", None, Some("secret"))]
        )
        .is_err()
    );
}

#[test]
fn remote_artifact_move_chain_and_later_updates_preserve_the_original_base() {
    let temp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(temp.path()).unwrap();
    std::fs::write(root.join("last.txt"), "v3").unwrap();
    let mut first = change("middle.txt", Some("v1"), Some("v1"));
    first.kind = ChangeKind::Move;
    first.path_before = Some("original.txt".into());
    let update = change("middle.txt", Some("v1"), Some("v2"));
    let mut last = change("last.txt", Some("v2"), Some("v3"));
    last.kind = ChangeKind::Move;
    last.path_before = Some("middle.txt".into());
    let changes = vec![first, update, last];
    let bundle = snapshot_changes(Ulid::new(), &workspace(root), &changes).unwrap();
    assert_eq!(bundle.manifest.files.len(), 1);
    let file = &bundle.manifest.files[0];
    assert_eq!(file.kind, ChangeKind::Move);
    assert_eq!(file.from_path.as_deref(), Some("original.txt"));
    assert_eq!(file.path, "last.txt");
    assert_eq!(file.base_sha256, Some(digest("v1")));
    assert_eq!(file.sha256, Some(digest("v3")));
    std::fs::remove_file(root.join("last.txt")).unwrap();
    let mut removed = changes;
    removed.push(change("last.txt", Some("v3"), None));
    let deleted = snapshot_changes(Ulid::new(), &workspace(root), &removed).unwrap();
    assert_eq!(deleted.manifest.files[0].kind, ChangeKind::Delete);
    assert_eq!(deleted.manifest.files[0].path, "original.txt");
    assert!(deleted.files.is_empty());
}
