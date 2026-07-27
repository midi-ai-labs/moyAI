use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};

use crate::agent::{AgentLoop, PromptBuilder};
use crate::app::{App, AppProcessRuntime, RunService};
use crate::cli::{CliCommand, RunArgs};
use crate::config::{ConfigLoader, ResolvedConfig};
use crate::edit::{ChangeTracker, EditSafety, Formatter};
use crate::error::AppBootstrapError;
use crate::llm::{OpenAiCompatClient, resolve_api_key_from_env};
use crate::runtime::SessionRuntimeEventHub;
use crate::session::{ProjectRepository, SessionId, SessionRecord, SessionRepository};
use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
use crate::tool::context::ToolServices;
use crate::tool::registry::ToolRegistry;
use crate::tool::truncate::ToolTruncator;
use crate::workspace::{WorkspaceDiscovery, project::project_display_name};

pub struct AppBootstrap;

enum WorkspaceRootMode {
    Discover,
    FixedToStart,
    Stored(Utf8PathBuf),
}

impl AppBootstrap {
    pub async fn build(command: &CliCommand) -> Result<App, AppBootstrapError> {
        let start_dir = command_directory(command)?;
        let run_args = match command {
            CliCommand::Run(args) => Some(args),
            _ => None,
        };
        let storage_paths = StoragePaths::discover()?;
        let sqlite = SqliteStore::open(&storage_paths)?;
        sqlite.migrate()?;
        let store = StoreBundle::new(sqlite);
        ConfigLoader::ensure_default_global_config()?;
        let restored = restore_run_session_directory(start_dir, run_args, &store).await?;
        let root_mode = restored
            .project_root
            .clone()
            .map(WorkspaceRootMode::Stored)
            .unwrap_or(WorkspaceRootMode::Discover);
        let mut app =
            Self::build_with_store_with_root_mode(&restored.directory, run_args, store, root_mode)
                .await?;
        app.resolved_run_session_id = restored.session_id;
        Ok(app)
    }

    pub(crate) async fn rebuild_for_directory_with_process_runtime(
        start_dir: &Utf8Path,
        process_runtime: AppProcessRuntime,
    ) -> Result<App, AppBootstrapError> {
        let config = ConfigLoader::load(start_dir, None)?;
        let store = process_runtime.store();
        Self::build_with_resolved_config(
            start_dir,
            store,
            WorkspaceRootMode::Discover,
            config,
            Some(process_runtime),
        )
        .await
    }

    pub(crate) async fn rebuild_for_session_with_process_runtime(
        session: &SessionRecord,
        process_runtime: AppProcessRuntime,
    ) -> Result<App, AppBootstrapError> {
        let store = process_runtime.store();
        let project = store.project_repo().get_project(session.project_id).await?;
        let directory = crate::session::service::normalize_session_cwd_for_project(
            &project.root_path,
            session.project_id,
            &project.vcs_kind,
            &session.cwd,
        )
        .map_err(|error| AppBootstrapError::Message(error.to_string()))?;
        let config = ConfigLoader::load(&directory, None)?;
        Self::build_with_resolved_config(
            &directory,
            store,
            WorkspaceRootMode::Stored(project.root_path),
            config,
            Some(process_runtime),
        )
        .await
    }

    pub(crate) async fn rebuild_for_directory_as_workspace_root_with_process_runtime(
        start_dir: &Utf8Path,
        process_runtime: AppProcessRuntime,
    ) -> Result<App, AppBootstrapError> {
        let config = ConfigLoader::load(start_dir, None)?;
        let store = process_runtime.store();
        Self::build_with_resolved_config(
            start_dir,
            store,
            WorkspaceRootMode::FixedToStart,
            config,
            Some(process_runtime),
        )
        .await
    }

    #[cfg(test)]
    async fn build_with_store(
        start_dir: &Utf8Path,
        run_args: Option<&RunArgs>,
        store: StoreBundle,
    ) -> Result<App, AppBootstrapError> {
        Self::build_with_store_with_root_mode(
            start_dir,
            run_args,
            store,
            WorkspaceRootMode::Discover,
        )
        .await
    }

    async fn build_with_store_with_root_mode(
        start_dir: &Utf8Path,
        run_args: Option<&RunArgs>,
        store: StoreBundle,
        root_mode: WorkspaceRootMode,
    ) -> Result<App, AppBootstrapError> {
        let config = ConfigLoader::load(start_dir, run_args)?;
        Self::build_with_resolved_config(start_dir, store, root_mode, config, None).await
    }

    #[cfg(test)]
    pub(crate) async fn rebuild_for_directory_as_workspace_root_with_config(
        start_dir: &Utf8Path,
        store: StoreBundle,
        config: ResolvedConfig,
    ) -> Result<App, AppBootstrapError> {
        Self::build_with_resolved_config(
            start_dir,
            store,
            WorkspaceRootMode::FixedToStart,
            config,
            None,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn rebuild_for_directory_as_workspace_root_with_process_runtime_and_config(
        start_dir: &Utf8Path,
        process_runtime: AppProcessRuntime,
        config: ResolvedConfig,
    ) -> Result<App, AppBootstrapError> {
        let store = process_runtime.store();
        Self::build_with_resolved_config(
            start_dir,
            store,
            WorkspaceRootMode::FixedToStart,
            config,
            Some(process_runtime),
        )
        .await
    }

    async fn build_with_resolved_config(
        start_dir: &Utf8Path,
        store: StoreBundle,
        root_mode: WorkspaceRootMode,
        config: ResolvedConfig,
        process_runtime: Option<AppProcessRuntime>,
    ) -> Result<App, AppBootstrapError> {
        let workspace = match root_mode {
            WorkspaceRootMode::Discover => WorkspaceDiscovery::discover(start_dir, &config)?,
            WorkspaceRootMode::FixedToStart => {
                WorkspaceDiscovery::discover_fixed_root(start_dir, &config)?
            }
            WorkspaceRootMode::Stored(root) => {
                WorkspaceDiscovery::discover_with_stored_root(start_dir, &root, &config)?
            }
        };
        let project_name = project_display_name(&workspace.root);
        store
            .project_repo()
            .upsert_project(
                workspace.project_id,
                &workspace.root,
                &project_name,
                match workspace.vcs {
                    crate::workspace::VcsKind::Git => "git",
                    crate::workspace::VcsKind::None => "none",
                },
            )
            .await?;

        let process_runtime = if let Some(process_runtime) = process_runtime {
            process_runtime
        } else {
            let session_event_hub = SessionRuntimeEventHub::new(1024);
            let runtime_event_projector = crate::protocol::CanonicalRuntimeEventProjector::new(
                store.protocol_event_store(),
                store.harness_run_store(),
                session_event_hub.publisher(),
            );
            let session_service = crate::session::SessionService::new(store.clone())
                .with_runtime_event_projector(runtime_event_projector);
            session_service
                .mark_stale_running_sessions(
                    "Application started without an active worker for this run; marking the prior run interrupted.",
                )
                .await
                .map_err(|error| AppBootstrapError::Message(error.to_string()))?;
            if let Err(error) = session_service.reconcile_started_harness_terminals() {
                // The canonical terminal is already durable and remains available
                // for the next startup replay. Observer repair must not rewrite or
                // reverse that semantic result.
                eprintln!(
                    "warning: startup could not reconcile every committed terminal into the native harness: {error}"
                );
            }
            let agent_runtime = Arc::new(crate::app::AgentRuntime::new(
                store.clone(),
                session_service.clone(),
            ));
            AppProcessRuntime::new(
                store.clone(),
                session_service,
                session_event_hub,
                agent_runtime,
            )
        };
        let session_event_hub = process_runtime.session_event_hub();
        let session_service = process_runtime.session_service();
        let agent_runtime = process_runtime.agent_runtime();
        let tool_services = ToolServices {
            edit_safety: EditSafety::default(),
            formatter: Formatter::new(config.format.clone()),
            change_tracker: ChangeTracker::default(),
            store: store.clone(),
            storage_paths: store.paths().clone(),
            truncator: ToolTruncator,
            mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
            skills: crate::skill::SkillsService::new(),
        };
        let registry = ToolRegistry::core_agent_for_config(&config);
        let api_key = resolve_api_key_from_env(config.model.api_key_env.as_deref())?;
        let llm = Arc::new(OpenAiCompatClient::new(api_key));
        let agent_loop = AgentLoop::new(llm, registry, store.clone(), PromptBuilder, tool_services);
        let run_service = RunService::new(
            store.clone(),
            config.clone(),
            workspace.clone(),
            session_service.clone(),
            agent_loop,
            session_event_hub.clone(),
            agent_runtime.clone(),
        );
        let run_service = Arc::new(run_service);

        Ok(App {
            config,
            workspace,
            store,
            session_service,
            run_service,
            session_event_hub,
            resolved_run_session_id: None,
            process_runtime,
        })
    }
}

#[derive(Debug)]
struct RestoredRunSessionDirectory {
    directory: Utf8PathBuf,
    session_id: Option<SessionId>,
    project_root: Option<Utf8PathBuf>,
}

async fn restore_run_session_directory(
    start_dir: Utf8PathBuf,
    run_args: Option<&RunArgs>,
    store: &StoreBundle,
) -> Result<RestoredRunSessionDirectory, AppBootstrapError> {
    let Some(run_args) = run_args else {
        return Ok(RestoredRunSessionDirectory {
            directory: start_dir,
            session_id: None,
            project_root: None,
        });
    };
    let session = if let Some(session_id) = run_args.session_id {
        Some(
            store
                .session_repo()
                .get_session(session_id)
                .await
                .map_err(AppBootstrapError::from)?,
        )
    } else if run_args.continue_last {
        let workspace = WorkspaceDiscovery::discover(&start_dir, &ResolvedConfig::default())?;
        store
            .session_repo()
            .latest_session(workspace.project_id)
            .await?
    } else {
        None
    };
    let Some(session) = session else {
        return Ok(RestoredRunSessionDirectory {
            directory: start_dir,
            session_id: None,
            project_root: None,
        });
    };
    let project = store.project_repo().get_project(session.project_id).await?;
    let directory = crate::session::service::normalize_session_cwd_for_project(
        &project.root_path,
        session.project_id,
        &project.vcs_kind,
        &session.cwd,
    )
    .map_err(|error| AppBootstrapError::Message(error.to_string()))?;
    Ok(RestoredRunSessionDirectory {
        directory,
        session_id: Some(session.id),
        project_root: Some(project.root_path),
    })
}

fn command_directory(command: &CliCommand) -> Result<camino::Utf8PathBuf, AppBootstrapError> {
    let current =
        std::env::current_dir().map_err(|error| AppBootstrapError::Message(error.to_string()))?;
    let current = Utf8PathBuf::from_path_buf(current).map_err(|_| {
        AppBootstrapError::Message("current directory is not valid UTF-8".to_string())
    })?;
    Ok(match command {
        CliCommand::Run(args) => args.directory.clone().unwrap_or(current),
        CliCommand::SessionList(args) => args.directory.clone().unwrap_or(current),
        CliCommand::SessionLoaded(args) => args.directory.clone().unwrap_or(current),
        CliCommand::SessionSearch(args) => args.directory.clone().unwrap_or(current),
        CliCommand::SessionSteer(args) => args.directory.clone().unwrap_or(current),
        CliCommand::Tui(args) => args.directory.clone().unwrap_or(current),
        CliCommand::Desktop(args) => {
            if let Some(directory) = args.directory.clone() {
                directory
            } else {
                default_desktop_workspace_directory()?.unwrap_or(current)
            }
        }
        CliCommand::SessionArchive(_)
        | CliCommand::SessionSettings(_)
        | CliCommand::SessionTitle(_)
        | CliCommand::SessionInterrupt(_)
        | CliCommand::SessionGoalGet(_)
        | CliCommand::SessionGoalSet(_)
        | CliCommand::SessionGoalClear(_)
        | CliCommand::SessionShow(_)
        | CliCommand::SessionHistory(_)
        | CliCommand::SessionRead(_)
        | CliCommand::SessionRejoin(_)
        | CliCommand::SessionRollback(_)
        | CliCommand::SessionFork(_)
        | CliCommand::SessionEvents(_)
        | CliCommand::SessionTurns(_) => current,
        CliCommand::ReplayRun(_)
        | CliCommand::ReplayReport(_)
        | CliCommand::ModelAvailability(_)
        | CliCommand::SchemaExport(_)
        | CliCommand::ContractSnapshot(_) => current,
    })
}

fn default_desktop_workspace_directory() -> Result<Option<Utf8PathBuf>, AppBootstrapError> {
    let Some(path) = StoragePaths::discover()
        .ok()
        .map(|paths| paths.data_dir.join("quick-chat-workspace"))
    else {
        return Ok(None);
    };
    std::fs::create_dir_all(path.as_std_path())?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{
        HarnessEventKind, HarnessEventStore, HarnessRunStatus, HarnessRunStore,
        NativeHarnessRecorder,
    };
    use crate::protocol::{
        ProtocolEventStore, RuntimeEvent, RuntimeEventId, RuntimeEventMsg, TurnId,
        TurnTerminalOutcome,
    };
    use crate::session::{
        DurableTurnTerminal, NewSession, RunEvent, SessionSelector, SessionStartRequest,
        SessionStatus,
    };

    #[tokio::test]
    async fn cli_run_session_restores_its_nested_workspace_directory_before_bootstrap() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root =
            Utf8PathBuf::from_path_buf(temp.path().join("aaa")).expect("utf8 project root");
        let selected = project_root.join("bbb");
        std::fs::create_dir_all(project_root.join(".git")).expect("git marker");
        std::fs::create_dir_all(&selected).expect("selected directory");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let workspace = WorkspaceDiscovery::discover(&selected, &ResolvedConfig::default())
            .expect("nested workspace");
        store
            .project_repo()
            .upsert_project(workspace.project_id, &workspace.root, "aaa", "git")
            .await
            .expect("project");
        let session = store
            .session_repo()
            .create_session(NewSession {
                project_id: workspace.project_id,
                title: "nested CLI session".to_string(),
                cwd: selected.clone(),
                model: "test-model".to_string(),
                base_url: "http://127.0.0.1:1234/v1".to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("session");
        let args = RunArgs {
            prompt: Some("continue".to_string()),
            session_id: Some(session.id),
            continue_last: false,
            title: None,
            directory: Some(project_root.clone()),
            model_override: None,
            base_url_override: None,
            output_mode: crate::cli::OutputMode::Human,
            show_reasoning_summary: false,
            review_uncommitted: false,
            review_branch: None,
            active_file: None,
            open_tabs: Vec::new(),
            visible_files: Vec::new(),
            image_paths: Vec::new(),
        };

        let restored = restore_run_session_directory(project_root.clone(), Some(&args), &store)
            .await
            .expect("restored directory");
        let app = AppBootstrap::build_with_store(&restored.directory, Some(&args), store)
            .await
            .expect("restored app");

        assert_eq!(restored.directory, selected);
        assert_eq!(restored.session_id, Some(session.id));
        assert_eq!(app.workspace.root, project_root);
        assert_eq!(app.workspace.cwd, selected);
        assert_eq!(app.workspace.authority_root(), selected);
    }

    #[tokio::test]
    async fn cli_continue_last_pins_the_exact_nested_session_selected_before_bootstrap() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root =
            Utf8PathBuf::from_path_buf(temp.path().join("aaa")).expect("utf8 project root");
        let first_cwd = project_root.join("bbb");
        let latest_cwd = project_root.join("ccc");
        std::fs::create_dir_all(project_root.join(".git")).expect("git marker");
        std::fs::create_dir_all(&first_cwd).expect("first cwd");
        std::fs::create_dir_all(&latest_cwd).expect("latest cwd");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let workspace = WorkspaceDiscovery::discover(&first_cwd, &ResolvedConfig::default())
            .expect("workspace");
        store
            .project_repo()
            .upsert_project(workspace.project_id, &workspace.root, "aaa", "git")
            .await
            .expect("project");
        let first = store
            .session_repo()
            .create_session(NewSession {
                project_id: workspace.project_id,
                title: "first".to_string(),
                cwd: first_cwd,
                model: "test-model".to_string(),
                base_url: "http://127.0.0.1:1234/v1".to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("first session");
        let latest = store
            .session_repo()
            .create_session(NewSession {
                project_id: workspace.project_id,
                title: "latest".to_string(),
                cwd: latest_cwd.clone(),
                model: "test-model".to_string(),
                base_url: "http://127.0.0.1:1234/v1".to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("latest session");
        store
            .session_repo()
            .update_session_settings(
                latest.id,
                &crate::session::SessionSettingsPatch {
                    model: Some("latest-model".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("make latest deterministic");
        let args = RunArgs {
            prompt: Some("continue".to_string()),
            session_id: None,
            continue_last: true,
            title: None,
            directory: Some(project_root.clone()),
            model_override: None,
            base_url_override: None,
            output_mode: crate::cli::OutputMode::Human,
            show_reasoning_summary: false,
            review_uncommitted: false,
            review_branch: None,
            active_file: None,
            open_tabs: Vec::new(),
            visible_files: Vec::new(),
            image_paths: Vec::new(),
        };

        let restored = restore_run_session_directory(project_root, Some(&args), &store)
            .await
            .expect("restored latest session");

        assert_ne!(restored.session_id, Some(first.id));
        assert_eq!(restored.session_id, Some(latest.id));
        assert_eq!(restored.directory, latest_cwd);
    }

    #[tokio::test]
    async fn cli_continue_last_rejects_a_session_cwd_from_another_project() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first_root =
            Utf8PathBuf::from_path_buf(temp.path().join("first")).expect("utf8 first root");
        let second_root =
            Utf8PathBuf::from_path_buf(temp.path().join("second")).expect("utf8 second root");
        std::fs::create_dir_all(first_root.join(".git")).expect("first git marker");
        std::fs::create_dir_all(second_root.join(".git")).expect("second git marker");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let first = WorkspaceDiscovery::discover(&first_root, &ResolvedConfig::default())
            .expect("first workspace");
        let second = WorkspaceDiscovery::discover(&second_root, &ResolvedConfig::default())
            .expect("second workspace");
        for (workspace, name) in [(&first, "first"), (&second, "second")] {
            store
                .project_repo()
                .upsert_project(workspace.project_id, &workspace.root, name, "git")
                .await
                .expect("project");
        }
        store
            .session_repo()
            .create_session(NewSession {
                project_id: first.project_id,
                title: "invalid first-project cwd".to_string(),
                cwd: second_root.clone(),
                model: "test-model".to_string(),
                base_url: "http://127.0.0.1:1234/v1".to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("legacy invalid session");
        store
            .session_repo()
            .create_session(NewSession {
                project_id: second.project_id,
                title: "second project latest".to_string(),
                cwd: second_root,
                model: "test-model".to_string(),
                base_url: "http://127.0.0.1:1234/v1".to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("second project session");
        let args = RunArgs {
            prompt: Some("continue".to_string()),
            session_id: None,
            continue_last: true,
            title: None,
            directory: Some(first_root.clone()),
            model_override: None,
            base_url_override: None,
            output_mode: crate::cli::OutputMode::Human,
            show_reasoning_summary: false,
            review_uncommitted: false,
            review_branch: None,
            active_file: None,
            open_tabs: Vec::new(),
            visible_files: Vec::new(),
            image_paths: Vec::new(),
        };

        let error = restore_run_session_directory(first_root, Some(&args), &store)
            .await
            .err()
            .expect("cross-project cwd must fail before bootstrap");

        assert!(error.to_string().contains("outside stored project root"));
    }

    #[tokio::test]
    async fn fixed_non_git_session_under_parent_git_restores_its_stored_project_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outer = Utf8PathBuf::from_path_buf(temp.path().join("aaa")).expect("utf8 outer");
        let data_dir = outer.join("data");
        let quick_chat = data_dir.join("quick-chat-workspace");
        std::fs::create_dir_all(outer.join(".git")).expect("parent git marker");
        std::fs::create_dir_all(&quick_chat).expect("quick chat workspace");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let app = AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
            &quick_chat,
            store,
            ResolvedConfig::default(),
        )
        .await
        .expect("fixed quick-chat app");
        let project_id = app.workspace.project_id;
        let process_runtime = app.process_runtime.clone();
        let session = app
            .session_service
            .start_or_resume(
                SessionStartRequest {
                    selector: SessionSelector::New,
                    title: Some("quick chat".to_string()),
                    cwd: quick_chat.clone(),
                    model: app.config.model.model.clone(),
                    base_url: app.config.model.base_url.clone(),
                    access_mode: crate::config::AccessMode::Default,
                },
                app.workspace.clone(),
            )
            .await
            .expect("quick-chat session");
        let projected = app
            .session_service
            .get_session(session.session.id)
            .await
            .expect("quick-chat session projection");

        let restored =
            AppBootstrap::rebuild_for_session_with_process_runtime(&projected, process_runtime)
                .await
                .expect("restored quick-chat session");

        assert_eq!(restored.workspace.root, quick_chat);
        assert_eq!(restored.workspace.cwd, quick_chat);
        assert_eq!(restored.workspace.authority_root(), quick_chat.as_path());
        assert_eq!(restored.workspace.project_id, project_id);
        assert_eq!(restored.workspace.vcs, crate::workspace::VcsKind::None);
    }

    #[tokio::test]
    async fn workspace_rebuild_reuses_the_process_host_without_replaying_startup_recovery() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first_workspace =
            Utf8PathBuf::from_path_buf(temp.path().join("workspace-a")).expect("utf8 workspace");
        let second_workspace =
            Utf8PathBuf::from_path_buf(temp.path().join("workspace-b")).expect("utf8 workspace");
        std::fs::create_dir_all(&first_workspace).expect("first workspace");
        std::fs::create_dir_all(&second_workspace).expect("second workspace");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let config = ResolvedConfig::default();
        let first = AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
            &first_workspace,
            StoreBundle::new(sqlite),
            config.clone(),
        )
        .await
        .expect("initial process bootstrap");
        let process_runtime = first.process_runtime.clone();
        let original_agent_runtime = process_runtime.agent_runtime();
        let original_publisher = first.session_event_hub.publisher();
        let session = first
            .session_service
            .start_or_resume(
                SessionStartRequest {
                    selector: SessionSelector::New,
                    title: Some("live across workspace navigation".to_string()),
                    cwd: first.workspace.cwd.clone(),
                    model: first.config.model.model.clone(),
                    base_url: first.config.model.base_url.clone(),
                    access_mode: first.config.permissions.access_mode,
                },
                first.workspace.clone(),
            )
            .await
            .expect("session");
        let turn_id = TurnId::new();
        first
            .store
            .session_repo()
            .admit_session_turn(session.session.id, turn_id)
            .await
            .expect("turn admission")
            .expect("admitted turn");

        let rebuilt =
            AppBootstrap::rebuild_for_directory_as_workspace_root_with_process_runtime_and_config(
                &second_workspace,
                process_runtime.clone(),
                config,
            )
            .await
            .expect("workspace rebuild");

        assert!(process_runtime.ptr_eq(&rebuilt.process_runtime));
        assert!(Arc::ptr_eq(
            &original_agent_runtime,
            &rebuilt.process_runtime.agent_runtime()
        ));
        assert!(
            !Arc::ptr_eq(&first.run_service, &rebuilt.run_service),
            "workspace-specific run services must not be reused"
        );
        assert_eq!(
            rebuilt
                .session_service
                .get_session(session.session.id)
                .await
                .expect("session after workspace rebuild")
                .status,
            SessionStatus::Running,
            "workspace navigation must not replay process-start stale-run recovery"
        );

        let mut subscriber = rebuilt.subscribe_session_runtime_events(session.session.id);
        let live_event = RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id: session.session.id,
            turn_id,
            sequence_no: 1,
            created_at_ms: crate::runtime::SystemClock::now_ms(),
            msg: RuntimeEventMsg::Warning {
                message: "published by the pre-navigation owner".to_string(),
            },
        };
        original_publisher
            .publish(live_event.clone())
            .expect("publish through original hub handle");
        drop(first);
        let observed = tokio::time::timeout(std::time::Duration::from_secs(1), subscriber.recv())
            .await
            .expect("same-hub event observation timeout")
            .expect("same-hub event observation");
        assert_eq!(observed.id, live_event.id);
    }

    #[tokio::test]
    async fn shared_bootstrap_recovers_stale_running_sessions_without_surface_hook() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 workspace");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let first = AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
            &workspace_root,
            StoreBundle::new(sqlite),
            ResolvedConfig::default(),
        )
        .await
        .expect("first bootstrap");
        let session = first
            .session_service
            .start_or_resume(
                SessionStartRequest {
                    selector: SessionSelector::New,
                    title: Some("crashed run".to_string()),
                    cwd: first.workspace.cwd.clone(),
                    model: first.config.model.model.clone(),
                    base_url: first.config.model.base_url.clone(),
                    access_mode: first.config.permissions.access_mode,
                },
                first.workspace.clone(),
            )
            .await
            .expect("session");
        first
            .store
            .session_repo()
            .admit_session_turn(session.session.id, TurnId::new())
            .await
            .expect("admission")
            .expect("admitted turn");
        let session_id = session.session.id;
        drop(first);

        let reopened = SqliteStore::open(&paths).expect("reopened sqlite");
        reopened.migrate().expect("reopened migration");
        let rebuilt = AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
            &workspace_root,
            StoreBundle::new(reopened),
            ResolvedConfig::default(),
        )
        .await
        .expect("shared bootstrap recovery");

        assert_eq!(
            rebuilt
                .session_service
                .get_session(session_id)
                .await
                .expect("recovered session")
                .status,
            SessionStatus::Failed
        );
    }

    #[tokio::test]
    async fn startup_reconciles_a_previously_committed_terminal_into_harness_exactly_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_root =
            Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 workspace");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let first = AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
            &workspace_root,
            StoreBundle::new(sqlite),
            ResolvedConfig::default(),
        )
        .await
        .expect("first bootstrap");
        let session = first
            .session_service
            .start_or_resume(
                SessionStartRequest {
                    selector: SessionSelector::New,
                    title: Some("terminal committed before observer projection".to_string()),
                    cwd: first.workspace.cwd.clone(),
                    model: first.config.model.model.clone(),
                    base_url: first.config.model.base_url.clone(),
                    access_mode: first.config.permissions.access_mode,
                },
                first.workspace.clone(),
            )
            .await
            .expect("session");
        let turn_id = TurnId::new();
        first
            .store
            .session_repo()
            .admit_session_turn(session.session.id, turn_id)
            .await
            .expect("admission")
            .expect("admitted turn");
        let recorder = NativeHarnessRecorder::start_harness_only_for_turn(
            &first.store,
            Some(session.session.id),
            first.workspace.root.clone(),
            turn_id,
        )
        .expect("mapped native harness");
        let run_id = recorder.run_id();
        let target = first
            .store
            .session_repo()
            .captured_running_terminal_target(session.session.id)
            .await
            .expect("capture terminal target")
            .expect("running terminal target");
        assert!(
            first
                .store
                .session_repo()
                .terminalize_captured_running_session_with_protocol_event(
                    session.session.id,
                    &RunEvent::TurnTerminal {
                        session_id: session.session.id,
                        terminal: Box::new(DurableTurnTerminal {
                            outcome: TurnTerminalOutcome::Completed,
                            final_response_id: None,
                            tool_call_count: 7,
                            failed_tool_count: 1,
                            change_count: 3,
                            metrics: Default::default(),
                        }),
                    },
                    target,
                )
                .await
                .expect("commit terminal without process-local projection")
        );
        let canonical_terminal = first
            .store
            .protocol_event_store()
            .list_runtime_events(session.session.id, turn_id)
            .expect("canonical runtime events")
            .into_iter()
            .find(|event| matches!(event.msg, RuntimeEventMsg::TurnTerminal { .. }))
            .expect("canonical terminal");
        let started = first
            .store
            .harness_run_store()
            .get_run(run_id)
            .expect("started harness lookup")
            .expect("started harness");
        assert_eq!(started.status, HarnessRunStatus::Started);
        assert!(started.completed_at_ms.is_none());
        assert!(started.canonical_terminal_runtime_event_id.is_none());
        assert!(
            first
                .store
                .harness_event_store()
                .list_events(run_id)
                .expect("pre-reconciliation events")
                .iter()
                .all(|event| event.kind != HarnessEventKind::RunTerminalized)
        );
        drop(recorder);
        drop(first);

        let reopened = SqliteStore::open(&paths).expect("reopened sqlite");
        reopened.migrate().expect("reopened migration");
        let rebuilt = AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
            &workspace_root,
            StoreBundle::new(reopened),
            ResolvedConfig::default(),
        )
        .await
        .expect("startup terminal reconciliation");
        let projected = rebuilt
            .store
            .harness_run_store()
            .get_run(run_id)
            .expect("projected harness lookup")
            .expect("projected harness");
        assert_eq!(projected.status, HarnessRunStatus::Pass);
        assert_eq!(
            projected.canonical_terminal_runtime_event_id,
            Some(canonical_terminal.id)
        );
        assert_eq!(
            projected.completed_at_ms,
            Some(canonical_terminal.created_at_ms)
        );
        let terminal_events = rebuilt
            .store
            .harness_event_store()
            .list_events(run_id)
            .expect("projected terminal evidence")
            .into_iter()
            .filter(|event| event.kind == HarnessEventKind::RunTerminalized)
            .collect::<Vec<_>>();
        assert_eq!(terminal_events.len(), 1);
        let RuntimeEventMsg::TurnTerminal { terminal } = &canonical_terminal.msg else {
            panic!("canonical event must be terminal");
        };
        assert_eq!(
            terminal_events[0].payload,
            crate::harness::HarnessEventPayload::generic(
                serde_json::to_value(RunEvent::TurnTerminal {
                    session_id: session.session.id,
                    terminal: terminal.clone(),
                })
                .expect("canonical terminal harness payload")
            )
        );
        drop(rebuilt);

        let reopened_again = SqliteStore::open(&paths).expect("second reopened sqlite");
        reopened_again.migrate().expect("second reopened migration");
        let rebuilt_again = AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
            &workspace_root,
            StoreBundle::new(reopened_again),
            ResolvedConfig::default(),
        )
        .await
        .expect("idempotent startup reconciliation");
        assert_eq!(
            rebuilt_again
                .store
                .harness_event_store()
                .list_events(run_id)
                .expect("replayed terminal evidence")
                .into_iter()
                .filter(|event| event.kind == HarnessEventKind::RunTerminalized)
                .count(),
            1,
            "reopening again must not duplicate canonical terminal evidence"
        );
    }
}
