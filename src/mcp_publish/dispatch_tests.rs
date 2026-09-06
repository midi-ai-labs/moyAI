use super::*;

use crate::config::AccessMode;
use crate::protocol::ProtocolEventStore;
use crate::session::{NewSession, ProjectId, SessionId};
use crate::storage::{SqliteStore, StoragePaths};
use crate::tool::context::ToolContext;

/// A canonical project and root session, separate from the app's private data.
/// Serving-owner tests can reuse this without depending on table or folder internals.
pub(crate) async fn fixture() -> (tempfile::TempDir, StoreBundle, PublishProfile) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = Utf8PathBuf::from_path_buf(directory.path().to_path_buf()).expect("UTF-8 temp");
    let workspace_root = root.join("workspace");
    std::fs::create_dir_all(workspace_root.join("nested")).expect("workspace");
    std::fs::write(workspace_root.join("visible.txt"), "alpha\nbeta\n").expect("text file");
    std::fs::write(workspace_root.join("nested/other.txt"), "alpha nested\n").expect("nested file");
    let data_dir = root.join("data");
    let paths = StoragePaths {
        database_path: data_dir.join("store.sqlite3"),
        truncation_dir: data_dir.join("outputs"),
        data_dir,
    };
    let sqlite = SqliteStore::open(&paths).expect("canonical store");
    sqlite.migrate().expect("current migrations");
    let store = StoreBundle::new(sqlite);
    let workspace =
        WorkspaceDiscovery::discover_fixed_root(&workspace_root, &ResolvedConfig::default())
            .expect("workspace boundary");
    store
        .project_repo()
        .upsert_project(
            workspace.project_id,
            &workspace.root,
            "Published project",
            "none",
        )
        .await
        .expect("project");
    let session = store
        .session_repo()
        .create_session(NewSession {
            project_id: workspace.project_id,
            title: "Published root".to_string(),
            cwd: workspace.cwd,
            model: "not-used-by-published-tools".to_string(),
            base_url: "http://127.0.0.1:9/v1".to_string(),
            access_mode: AccessMode::FullAccess,
            provider_connection: None,
        })
        .await
        .expect("root session");
    let mut profile = PublishProfile::new(
        "Project reads".to_string(),
        PublishTarget::LegacySession {
            project_id: session.project_id,
            root_session_id: session.id,
            workspace_root,
        },
    );
    profile.tools = vec![
        ToolName::List,
        ToolName::Glob,
        ToolName::Grep,
        ToolName::Read,
        ToolName::InspectDirectory,
        ToolName::CurrentTime,
    ];
    profile.authentication = PublishAuthentication::LocalCredential {
        credential_id: ulid::Ulid::new(),
    };
    profile.enabled = true;
    (directory, store, profile)
}

pub(crate) fn legacy_target_fields(
    profile: &PublishProfile,
) -> (ProjectId, SessionId, Utf8PathBuf) {
    match &profile.target {
        PublishTarget::LegacySession {
            project_id,
            root_session_id,
            workspace_root,
        } => (*project_id, *root_session_id, workspace_root.clone()),
        _ => panic!("legacy fixture target"),
    }
}

fn storage_counts(store: &StoreBundle) -> std::collections::BTreeMap<String, u64> {
    let connection = rusqlite::Connection::open_with_flags(
        &store.paths().database_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let tables = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let sql = format!("SELECT count(*) FROM \"{}\"", table.replace('"', "\"\""));
            let count = connection.query_row(&sql, [], |row| row.get(0)).unwrap();
            (table, count)
        })
        .collect()
}

#[tokio::test]
async fn project_reads_without_any_chat_preserve_storage_and_source() {
    let (_directory, store, mut profile) = fixture().await;
    let (project_id, session_id, workspace_root) = legacy_target_fields(&profile);
    store
        .session_repo()
        .delete_session(session_id)
        .await
        .unwrap();
    profile.target = PublishTarget::Project {
        project_id,
        workspace_root: workspace_root.clone(),
    };
    let before = storage_counts(&store);
    let reader = dispatcher(profile, store.clone()).await;
    for (name, arguments) in [
        ("list", json!({})),
        ("glob", json!({"pattern":"**/*.txt"})),
        ("grep", json!({"pattern":"alpha"})),
        ("read", json!({"path":"visible.txt"})),
        ("inspect_directory", json!({})),
        ("current_time", json!({})),
    ] {
        let result = call(&reader, name, arguments).await;
        assert_eq!(result["isError"], false, "{name}");
        if name == "read" {
            assert_eq!(
                result["structuredContent"]["edit_baseline"]["recorded"],
                false
            );
        }
    }
    assert!(
        store
            .session_repo()
            .list_sessions(project_id, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(storage_counts(&store), before);
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("visible.txt")).unwrap(),
        "alpha\nbeta\n"
    );
    assert_eq!(
        std::fs::read_dir(&store.paths().truncation_dir)
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn project_target_requires_exact_registered_root_and_survives_chat_deletion() {
    let (_directory, store, mut profile) = fixture().await;
    let (project_id, session_id, workspace_root) = legacy_target_fields(&profile);
    for target in [
        PublishTarget::Project {
            project_id: ProjectId::new(),
            workspace_root: workspace_root.clone(),
        },
        PublishTarget::Project {
            project_id,
            workspace_root: workspace_root.join("nested"),
        },
    ] {
        assert_eq!(
            validate_target(&store, &target, &[]).await,
            Err(PublishCallError::InvalidTarget)
        );
    }
    profile.target = PublishTarget::Project {
        project_id,
        workspace_root,
    };
    let reader = dispatcher(profile, store.clone()).await;
    store
        .session_repo()
        .delete_session(session_id)
        .await
        .unwrap();
    assert_eq!(
        call(&reader, "read", json!({"path":"visible.txt"})).await["isError"],
        false
    );
    assert_eq!(
        call(&reader, "current_time", json!({})).await["isError"],
        false
    );
}

#[tokio::test]
async fn project_deletion_during_read_withholds_already_read_content() {
    let (_directory, store, mut profile) = fixture().await;
    let (project_id, session_id, workspace_root) = legacy_target_fields(&profile);
    store
        .session_repo()
        .delete_session(session_id)
        .await
        .unwrap();
    profile.target = PublishTarget::Project {
        project_id,
        workspace_root,
    };
    let (reader, completed_read, release) = paused_reader(profile, store.clone()).await;
    let pending = tokio::spawn(async move {
        reader
            .call(
                "read",
                json!({"path":"visible.txt"}),
                CancellationToken::new(),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), completed_read.notified())
        .await
        .unwrap();
    store
        .project_repo()
        .delete_project(project_id)
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(pending.await.unwrap(), Err(PublishCallError::TargetChanged));
}

#[tokio::test]
async fn temp_time_needs_no_project_or_workspace_and_cannot_publish_filesystem_tools() {
    let directory = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(directory.path().to_owned()).unwrap();
    let paths = StoragePaths {
        database_path: root.join("data/store.sqlite3"),
        truncation_dir: root.join("data/outputs"),
        data_dir: root.join("data"),
    };
    let sqlite = SqliteStore::open(&paths).unwrap();
    sqlite.migrate().unwrap();
    let store = StoreBundle::new(sqlite);
    let mut profile = PublishProfile::new("Temporary clock".into(), PublishTarget::Temp {});
    profile.enabled = true;
    profile.authentication = PublishAuthentication::LocalCredential {
        credential_id: ulid::Ulid::new(),
    };
    profile.tools = vec![ToolName::CurrentTime];
    let before = storage_counts(&store);
    // No path is validated for temp, including the application's protected data root.
    validate_target(&store, &profile.target, &[root.clone()])
        .await
        .unwrap();
    let reader = PublishReadDispatcher::new(
        profile.clone(),
        store.clone(),
        ResolvedConfig::default(),
        vec![root],
    )
    .await
    .unwrap();
    assert!(reader.inner.target.workspace().is_none());
    assert_eq!(reader.tool_descriptors().len(), 1);
    assert_eq!(
        call(&reader, "current_time", Value::Null).await["isError"],
        false
    );
    assert_eq!(
        reader
            .call(
                "current_time",
                json!({"path":"private"}),
                CancellationToken::new()
            )
            .await,
        Err(PublishCallError::InvalidArguments)
    );
    for tool in [
        ToolName::List,
        ToolName::Glob,
        ToolName::Grep,
        ToolName::Read,
        ToolName::InspectDirectory,
    ] {
        assert_eq!(
            reader
                .call(
                    &tool.to_string(),
                    json!({"path":"."}),
                    CancellationToken::new()
                )
                .await,
            Err(PublishCallError::ToolUnavailable)
        );
        let mut invalid = profile.clone();
        invalid.tools.push(tool);
        assert!(
            PublishReadDispatcher::new(invalid, store.clone(), ResolvedConfig::default(), vec![])
                .await
                .is_err()
        );
    }
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        reader.call("current_time", json!({}), cancel).await,
        Err(PublishCallError::Cancelled)
    );
    assert_eq!(storage_counts(&store), before);
    assert!(
        store
            .project_repo()
            .list_projects(10)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        std::fs::read_dir(&store.paths().truncation_dir)
            .unwrap()
            .count(),
        0
    );
}

async fn dispatcher(profile: PublishProfile, store: StoreBundle) -> PublishReadDispatcher {
    PublishReadDispatcher::new(profile, store, ResolvedConfig::default(), vec![])
        .await
        .expect("published reader")
}

async fn call(dispatcher: &PublishReadDispatcher, name: &str, args: Value) -> Value {
    dispatcher
        .call(name, args, CancellationToken::new())
        .await
        .expect("published call")
}

#[tokio::test]
async fn six_real_read_tools_preserve_canonical_history_admission_and_source() {
    let (_directory, store, profile) = fixture().await;
    let session_id = legacy_target_fields(&profile).1;
    let source = legacy_target_fields(&profile).2.join("visible.txt");
    let reader = dispatcher(profile, store.clone()).await;
    let tools = reader.tool_descriptors();
    assert_eq!(tools.len(), 6);
    for tool in &tools {
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
    }
    for (name, args) in [
        ("list", json!({})),
        ("glob", json!({"pattern": "**/*.txt"})),
        ("grep", json!({"pattern": "alpha"})),
        ("read", json!({"path": "visible.txt"})),
        ("inspect_directory", json!({})),
        ("current_time", Value::Null),
    ] {
        let result = call(&reader, name, args).await;
        assert_eq!(result["isError"], false, "{name}: {result}");
        assert!(
            !result["content"][0]["text"]
                .as_str()
                .expect("text")
                .is_empty()
        );
        if name == "read" {
            assert!(
                result["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("alpha")
            );
            assert_eq!(
                result["structuredContent"]["edit_baseline"]["recorded"],
                false
            );
        }
    }
    let events = store.protocol_event_store();
    assert!(
        events
            .list_history_items_for_session(session_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        events
            .list_turn_items_for_session(session_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        events
            .list_runtime_events_for_session(session_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        !store
            .session_repo()
            .has_fresh_run_admission(session_id)
            .await
            .unwrap()
    );
    assert_eq!(std::fs::read_to_string(source).unwrap(), "alpha\nbeta\n");
    assert_eq!(
        std::fs::read_dir(&store.paths().truncation_dir)
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn nested_session_authority_excludes_project_parent_and_sibling_files() {
    let (_directory, store, mut profile) = fixture().await;
    let project_root = legacy_target_fields(&profile).2.clone();
    let nested = project_root.join("nested");
    let sibling = project_root.join("sibling");
    std::fs::create_dir(&sibling).unwrap();
    std::fs::write(
        sibling.join("secret.txt"),
        "sibling-content-must-not-be-published",
    )
    .unwrap();
    let session = store
        .session_repo()
        .create_session(NewSession {
            project_id: legacy_target_fields(&profile).0,
            title: "Nested root chat".to_string(),
            cwd: nested.clone(),
            model: "unused".to_string(),
            base_url: "http://127.0.0.1:9/v1".to_string(),
            access_mode: AccessMode::FullAccess,
            provider_connection: None,
        })
        .await
        .unwrap();
    profile.target = PublishTarget::LegacySession {
        project_id: session.project_id,
        root_session_id: session.id,
        workspace_root: nested,
    };
    let reader = dispatcher(profile, store).await;
    assert_eq!(
        call(&reader, "read", json!({"path":"other.txt"})).await["isError"],
        false
    );
    for path in [
        Utf8PathBuf::from("../visible.txt"),
        project_root.join("visible.txt"),
        sibling.join("secret.txt"),
    ] {
        for tool in ["read", "list", "glob", "grep", "inspect_directory"] {
            let arguments = if matches!(tool, "glob" | "grep") {
                json!({"path":path,"pattern":"secret"})
            } else {
                json!({"path":path})
            };
            assert_eq!(
                reader.call(tool, arguments, CancellationToken::new()).await,
                Err(PublishCallError::InvalidArguments),
                "{tool}"
            );
        }
    }
    for (name, args) in [
        ("list", json!({})),
        ("glob", json!({"pattern":"**/*"})),
        ("grep", json!({"pattern":"alpha"})),
        ("inspect_directory", json!({})),
    ] {
        let output = call(&reader, name, args).await;
        assert_eq!(output["isError"], false);
        assert!(!output.to_string().contains("visible.txt"));
        assert!(!output.to_string().contains("secret.txt"));
    }
}

#[tokio::test]
async fn profile_allowlist_is_enforced_by_discovery_and_execution() {
    let (_directory, store, mut profile) = fixture().await;
    profile.tools = vec![ToolName::CurrentTime];
    let reader = dispatcher(profile, store).await;
    assert_eq!(
        reader
            .tool_descriptors()
            .iter()
            .map(|spec| spec["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["current_time"]
    );
    for name in [
        "read",
        "write",
        "shell",
        "mcp_call",
        "update_plan",
        "unknown",
    ] {
        assert_eq!(
            reader.call(name, json!({}), CancellationToken::new()).await,
            Err(PublishCallError::ToolUnavailable)
        );
    }
}

#[tokio::test]
async fn full_access_and_additional_roots_do_not_extend_external_read_authority() {
    let (directory, store, profile) = fixture().await;
    let external = Utf8PathBuf::from_path_buf(directory.path().join("outside.txt")).unwrap();
    std::fs::write(&external, "private-outside-marker").unwrap();
    let protected = legacy_target_fields(&profile).2.join("application-config");
    std::fs::create_dir(&protected).unwrap();
    std::fs::write(
        protected.join("credentials.json"),
        "private-credential-marker",
    )
    .unwrap();
    let mut config = ResolvedConfig::default();
    config
        .permissions
        .additional_read_roots
        .push(external.parent().unwrap().to_path_buf());
    let reader =
        PublishReadDispatcher::new(profile, store.clone(), config, vec![protected.clone()])
            .await
            .unwrap();
    for path in [
        external,
        protected.join("credentials.json"),
        store.paths().database_path.clone(),
    ] {
        for name in ["read", "grep", "glob", "list", "inspect_directory"] {
            let arguments = match name {
                "grep" | "glob" => json!({"path": path, "pattern": "private"}),
                _ => json!({"path": path}),
            };
            assert_eq!(
                reader.call(name, arguments, CancellationToken::new()).await,
                Err(PublishCallError::InvalidArguments),
                "{name}"
            );
        }
    }
    for name in ["list", "glob", "grep", "inspect_directory"] {
        let args = match name {
            "grep" => json!({"pattern": "private"}),
            "glob" => json!({"pattern": "**/*"}),
            _ => json!({}),
        };
        let result = call(&reader, name, args).await.to_string();
        assert!(!result.contains("private-credential-marker"));
    }
}

#[tokio::test]
async fn target_validation_rejects_wrong_project_unknown_session_and_private_root() {
    let (_directory, store, profile) = fixture().await;
    validate_target(&store, &profile.target, &[]).await.unwrap();
    let (_, root_session_id, workspace_root) = legacy_target_fields(&profile);
    let mut wrong = PublishTarget::LegacySession {
        project_id: ProjectId::new(),
        root_session_id,
        workspace_root: workspace_root.clone(),
    };
    assert_eq!(
        validate_target(&store, &wrong, &[]).await,
        Err(PublishCallError::InvalidTarget)
    );
    wrong = PublishTarget::LegacySession {
        project_id: legacy_target_fields(&profile).0,
        root_session_id: SessionId::new(),
        workspace_root,
    };
    assert_eq!(
        validate_target(&store, &wrong, &[]).await,
        Err(PublishCallError::InvalidTarget)
    );
    assert_eq!(
        validate_target(
            &store,
            &profile.target,
            &[legacy_target_fields(&profile).2.clone()]
        )
        .await,
        Err(PublishCallError::InvalidTarget)
    );
}

#[tokio::test]
async fn side_conversation_cannot_be_published_as_a_root_session() {
    let (_directory, store, profile) = fixture().await;
    let mut side = ResolvedConfig::default().side_chat;
    side.base_url = "http://127.0.0.1:9/v1".to_string();
    side.model = "not-used-by-published-tools".to_string();
    let binding = store
        .side_chat_repo()
        .ensure(
            legacy_target_fields(&profile).1,
            crate::storage::SideChatProviderTarget::try_from(&side).unwrap(),
        )
        .unwrap();
    let (project_id, _, workspace_root) = legacy_target_fields(&profile);
    let target = PublishTarget::LegacySession {
        project_id,
        root_session_id: binding.conversation_session_id,
        workspace_root,
    };
    assert_eq!(
        validate_target(&store, &target, &[]).await,
        Err(PublishCallError::InvalidTarget)
    );
}

struct PausedRead {
    completed_read: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait(?Send)]
impl crate::tool::registry::Tool for PausedRead {
    fn spec(&self) -> crate::tool::ToolSpec {
        crate::tool::registry::Tool::spec(&crate::tool::read::ReadTool)
    }

    async fn execute(
        &self,
        arguments: Value,
        context: ToolContext<'_>,
    ) -> Result<crate::tool::ToolResult, crate::error::ToolError> {
        self.execute_read(arguments, ReadToolContext::agent(context))
            .await
    }

    async fn execute_read(
        &self,
        arguments: Value,
        context: ReadToolContext<'_>,
    ) -> Result<crate::tool::ToolResult, crate::error::ToolError> {
        let result = crate::tool::registry::Tool::execute_read(
            &crate::tool::read::ReadTool,
            arguments,
            context,
        )
        .await;
        self.completed_read.notify_one();
        self.release.notified().await;
        result
    }
}

async fn paused_reader(
    profile: PublishProfile,
    store: StoreBundle,
) -> (
    PublishReadDispatcher,
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
) {
    let mut reader = dispatcher(profile, store).await;
    let completed_read = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    Arc::get_mut(&mut reader.inner)
        .unwrap()
        .registry
        .replace_tool_for_test(Arc::new(PausedRead {
            completed_read: completed_read.clone(),
            release: release.clone(),
        }));
    (reader, completed_read, release)
}

#[tokio::test]
async fn deleted_target_during_read_withholds_already_read_content() {
    let (_directory, store, profile) = fixture().await;
    let (reader, completed_read, release) = paused_reader(profile.clone(), store.clone()).await;
    let pending = tokio::spawn(async move {
        reader
            .call(
                "read",
                json!({"path":"visible.txt"}),
                CancellationToken::new(),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), completed_read.notified())
        .await
        .unwrap();
    store
        .session_repo()
        .delete_session(legacy_target_fields(&profile).1)
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(pending.await.unwrap(), Err(PublishCallError::TargetChanged));
}

#[tokio::test]
async fn cancellation_retains_worker_ownership_and_discards_read_content() {
    let (_directory, store, profile) = fixture().await;
    let (reader, completed_read, release) = paused_reader(profile, store).await;
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let pending = tokio::spawn(async move {
        reader
            .call("read", json!({"path":"visible.txt"}), task_cancel)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), completed_read.notified())
        .await
        .unwrap();
    cancel.cancel();
    tokio::task::yield_now().await;
    assert!(
        !pending.is_finished(),
        "the synchronous worker is still owned while cancellation settles"
    );
    release.notify_one();
    assert_eq!(pending.await.unwrap(), Err(PublishCallError::Cancelled));
}

struct PausedBeforeReadAdmission {
    admission: std::sync::Mutex<
        Option<tokio::sync::oneshot::Sender<crate::tool::context::ToolEffectAdmission>>,
    >,
    release: Arc<tokio::sync::Notify>,
    reads_started: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait(?Send)]
impl crate::tool::registry::Tool for PausedBeforeReadAdmission {
    fn spec(&self) -> crate::tool::ToolSpec {
        crate::tool::registry::Tool::spec(&crate::tool::read::ReadTool)
    }

    async fn execute(
        &self,
        arguments: Value,
        context: ToolContext<'_>,
    ) -> Result<crate::tool::ToolResult, crate::error::ToolError> {
        self.execute_read(arguments, ReadToolContext::agent(context))
            .await
    }

    async fn execute_read(
        &self,
        arguments: Value,
        mut context: ReadToolContext<'_>,
    ) -> Result<crate::tool::ToolResult, crate::error::ToolError> {
        let path = context.workspace().cwd.join("visible.txt");
        let admission = context
            .confirm_if_needed(
                AccessKind::Read,
                "Read published fixture".into(),
                vec![path],
                false,
                vec![],
            )
            .await?;
        self.admission
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(admission.clone())
            .unwrap_or_else(|_| panic!("admission observer"));
        self.release.notified().await;
        admission.admit()?;
        self.reads_started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        crate::tool::registry::Tool::execute_read(&crate::tool::read::ReadTool, arguments, context)
            .await
    }
}

#[tokio::test]
async fn cancellation_before_read_admission_blocks_execution_and_retains_worker_until_settled() {
    let (_directory, store, profile) = fixture().await;
    let mut reader = dispatcher(profile, store).await;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let reads_started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    Arc::get_mut(&mut reader.inner)
        .unwrap()
        .registry
        .replace_tool_for_test(Arc::new(PausedBeforeReadAdmission {
            admission: std::sync::Mutex::new(Some(sender)),
            release: release.clone(),
            reads_started: reads_started.clone(),
        }));
    let cancel = CancellationToken::new();
    let operation_cancel = cancel.clone();
    let pending = tokio::spawn(async move {
        reader
            .call("read", json!({"path":"visible.txt"}), operation_cancel)
            .await
    });
    let admission = tokio::time::timeout(std::time::Duration::from_secs(5), receiver)
        .await
        .unwrap()
        .unwrap();
    cancel.cancel();
    // Observe the actual RunControl owner through its normal admission interface.
    // A wake-token-only cancellation never closes this boundary. The worker stays
    // paused and performs no file read while this observer waits for classification.
    let classified = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if admission.admit().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    let held = !pending.is_finished();
    release.notify_one();
    assert_eq!(pending.await.unwrap(), Err(PublishCallError::Cancelled));
    assert!(
        classified,
        "MCP cancellation must close the RunControl admission boundary"
    );
    assert!(held, "cancellation retains the still-paused worker");
    assert_eq!(reads_started.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn target_deletion_invalidates_even_current_time_without_creating_a_new_session() {
    let (_directory, store, profile) = fixture().await;
    let reader = dispatcher(profile.clone(), store.clone()).await;
    store
        .session_repo()
        .delete_session(legacy_target_fields(&profile).1)
        .await
        .unwrap();
    assert_eq!(
        reader
            .call("current_time", json!({}), CancellationToken::new())
            .await,
        Err(PublishCallError::TargetChanged)
    );
    assert!(
        store
            .session_repo()
            .list_sessions(legacy_target_fields(&profile).0, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn canonical_project_immutability_keeps_started_dispatcher_on_its_target() {
    let (directory, store, profile) = fixture().await;
    let reader = dispatcher(profile.clone(), store.clone()).await;
    let other = Utf8PathBuf::from_path_buf(directory.path().join("different-project")).unwrap();
    std::fs::create_dir(&other).unwrap();
    assert!(
        store
            .project_repo()
            .upsert_project(
                legacy_target_fields(&profile).0,
                &other,
                "Changed project",
                "none"
            )
            .await
            .is_err()
    );
    let result = call(&reader, "read", json!({"path":"visible.txt"})).await;
    assert_eq!(result["isError"], false);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("alpha")
    );
}

#[tokio::test]
async fn renamed_and_replaced_directory_is_not_the_published_object() {
    let (_directory, store, profile) = fixture().await;
    let reader = dispatcher(profile.clone(), store).await;
    let original = &legacy_target_fields(&profile).2;
    let moved = original.with_file_name("old-workspace");
    // Some platforms prevent rename while the directory pin is open, which also
    // enforces the required identity boundary. Otherwise replacement must fail.
    if std::fs::rename(original, &moved).is_ok() {
        std::fs::create_dir(original).unwrap();
        std::fs::write(
            original.join("visible.txt"),
            "replacement-must-not-be-published",
        )
        .unwrap();
        assert_eq!(
            reader
                .call(
                    "read",
                    json!({"path":"visible.txt"}),
                    CancellationToken::new()
                )
                .await,
            Err(PublishCallError::TargetChanged)
        );
    } else {
        assert_eq!(
            call(&reader, "read", json!({"path":"visible.txt"})).await["isError"],
            false
        );
    }
}

#[tokio::test]
async fn arguments_and_tool_failures_never_echo_private_input() {
    let (_directory, store, profile) = fixture().await;
    let reader = dispatcher(profile, store).await;
    for args in [
        json!({"secret-field":"private-marker"}),
        json!({"path":true}),
        json!({"path":"x".repeat(MAX_ARGUMENT_BYTES)}),
    ] {
        assert_eq!(
            reader.call("read", args, CancellationToken::new()).await,
            Err(PublishCallError::InvalidArguments)
        );
    }
    let failed = call(&reader, "grep", json!({"pattern":"[private-marker"})).await;
    assert_eq!(failed["isError"], true);
    assert!(!failed.to_string().contains("private-marker"));
}

#[tokio::test]
async fn cancelled_calls_and_disabled_profiles_are_not_admitted() {
    let (_directory, store, mut profile) = fixture().await;
    let reader = dispatcher(profile.clone(), store.clone()).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        reader.call("current_time", json!({}), cancel).await,
        Err(PublishCallError::Cancelled)
    );
    profile.enabled = false;
    assert!(matches!(
        PublishReadDispatcher::new(
            profile.clone(),
            store.clone(),
            ResolvedConfig::default(),
            vec![]
        )
        .await,
        Err(PublishCallError::Unavailable)
    ));
    profile.enabled = true;
    profile.authentication = PublishAuthentication::Unpaired {};
    assert!(matches!(
        PublishReadDispatcher::new(profile, store, ResolvedConfig::default(), vec![]).await,
        Err(PublishCallError::Unavailable)
    ));
}

#[tokio::test]
async fn bounded_read_keeps_continuation_without_canonical_output_files() {
    let (_directory, store, profile) = fixture().await;
    let mut config = ResolvedConfig::default();
    config.tool_output.max_lines = 1;
    let reader = PublishReadDispatcher::new(profile, store.clone(), config, vec![])
        .await
        .unwrap();
    let first = call(&reader, "read", json!({"path":"visible.txt"})).await;
    assert_eq!(first["isError"], false);
    assert!(first["structuredContent"]["next_offset"].is_number());
    let next = first["structuredContent"]["next_offset"].clone();
    let second = call(
        &reader,
        "read",
        json!({"path":"visible.txt", "offset": next}),
    )
    .await;
    assert_eq!(second["isError"], false);
    assert!(
        second["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("beta")
    );
    assert_eq!(
        std::fs::read_dir(&store.paths().truncation_dir)
            .unwrap()
            .count(),
        0
    );
}
