use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};

use crate::agent::{AgentLoop, PromptBuilder};
use crate::app::{App, RunService};
use crate::cli::{CliCommand, RunArgs};
use crate::config::ConfigLoader;
use crate::edit::{ChangeTracker, EditSafety, Formatter};
use crate::error::AppBootstrapError;
use crate::llm::OpenAiCompatClient;
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

    pub async fn rebuild_for_directory(
        start_dir: &Utf8Path,
        store: StoreBundle,
    ) -> Result<App, AppBootstrapError> {
        Self::build_with_store(start_dir, None, store).await
    }

    pub async fn rebuild_for_directory_as_workspace_root(
        start_dir: &Utf8Path,
        store: StoreBundle,
    ) -> Result<App, AppBootstrapError> {
        Self::build_with_store_with_root_mode(start_dir, None, store, true).await
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

        let session_service = crate::session::SessionService::new(store.clone());
        let tool_services = ToolServices {
            edit_safety: EditSafety::default(),
            formatter: Formatter::new(config.format.clone()),
            change_tracker: ChangeTracker::default(),
            store: store.clone(),
            storage_paths: store.paths().clone(),
            truncator: ToolTruncator,
            mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
        };
        let registry = ToolRegistry::builtin(tool_services.clone());
        let api_key = config
            .model
            .api_key_env
            .as_ref()
            .and_then(|value| std::env::var(value).ok());
        let llm = Arc::new(OpenAiCompatClient::new(
            config.model.connect_timeout_ms,
            config.model.request_timeout_ms,
            config.model.max_retries,
            api_key,
        )?);
        let agent_loop = AgentLoop::new(llm, registry, store.clone(), PromptBuilder, tool_services);
        let session_event_hub = SessionRuntimeEventHub::new(1024);
        let run_service = RunService::new(
            store.clone(),
            config.clone(),
            workspace.clone(),
            session_service.clone(),
            agent_loop,
            session_event_hub.clone(),
        );

        Ok(App {
            config,
            workspace,
            store,
            session_service,
            run_service,
            session_event_hub,
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
        | CliCommand::SessionCompact(_)
        | CliCommand::SessionMemory(_)
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
