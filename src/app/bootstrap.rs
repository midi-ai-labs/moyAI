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
use crate::session::ProjectRepository;
use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
use crate::tool::context::ToolServices;
use crate::tool::registry::ToolRegistry;
use crate::tool::truncate::ToolTruncator;
use crate::workspace::WorkspaceDiscovery;

pub struct AppBootstrap;

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
        Self::build_with_store(&start_dir, run_args, store).await
    }

    pub(crate) async fn rebuild_for_directory_with_process_runtime(
        start_dir: &Utf8Path,
        process_runtime: AppProcessRuntime,
    ) -> Result<App, AppBootstrapError> {
        let config = ConfigLoader::load(start_dir, None)?;
        let store = process_runtime.store();
        Self::build_with_resolved_config(start_dir, store, false, config, Some(process_runtime))
            .await
    }

    pub(crate) async fn rebuild_for_directory_as_workspace_root_with_process_runtime(
        start_dir: &Utf8Path,
        process_runtime: AppProcessRuntime,
    ) -> Result<App, AppBootstrapError> {
        let config = ConfigLoader::load(start_dir, None)?;
        let store = process_runtime.store();
        Self::build_with_resolved_config(start_dir, store, true, config, Some(process_runtime))
            .await
    }

    async fn build_with_store(
        start_dir: &Utf8Path,
        run_args: Option<&RunArgs>,
        store: StoreBundle,
    ) -> Result<App, AppBootstrapError> {
        Self::build_with_store_with_root_mode(start_dir, run_args, store, false).await
    }

    async fn build_with_store_with_root_mode(
        start_dir: &Utf8Path,
        run_args: Option<&RunArgs>,
        store: StoreBundle,
        fixed_workspace_root: bool,
    ) -> Result<App, AppBootstrapError> {
        let config = ConfigLoader::load(start_dir, run_args)?;
        Self::build_with_resolved_config(start_dir, store, fixed_workspace_root, config, None).await
    }

    #[cfg(test)]
    pub(crate) async fn rebuild_for_directory_as_workspace_root_with_config(
        start_dir: &Utf8Path,
        store: StoreBundle,
        config: ResolvedConfig,
    ) -> Result<App, AppBootstrapError> {
        Self::build_with_resolved_config(start_dir, store, true, config, None).await
    }

    #[cfg(test)]
    pub(crate) async fn rebuild_for_directory_as_workspace_root_with_process_runtime_and_config(
        start_dir: &Utf8Path,
        process_runtime: AppProcessRuntime,
        config: ResolvedConfig,
    ) -> Result<App, AppBootstrapError> {
        let store = process_runtime.store();
        Self::build_with_resolved_config(start_dir, store, true, config, Some(process_runtime))
            .await
    }

    async fn build_with_resolved_config(
        start_dir: &Utf8Path,
        store: StoreBundle,
        fixed_workspace_root: bool,
        config: ResolvedConfig,
        process_runtime: Option<AppProcessRuntime>,
    ) -> Result<App, AppBootstrapError> {
        let workspace = if fixed_workspace_root {
            WorkspaceDiscovery::discover_fixed_root(start_dir, &config)?
        } else {
            WorkspaceDiscovery::discover(start_dir, &config)?
        };
        let project_name = workspace
            .root
            .file_name()
            .map(|value| value.to_string())
            .unwrap_or_else(|| workspace.root.to_string());
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
            process_runtime,
        })
    }
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
        DurableTurnTerminal, RunEvent, SessionSelector, SessionStartRequest, SessionStatus,
    };

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
