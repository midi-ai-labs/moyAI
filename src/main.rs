use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;

use camino::Utf8PathBuf;
use moyai::app::{
    AppBootstrap, AppCommand, ReviewRequest, RunRequest, SessionArchiveRequest,
    SessionCompactRequest, SessionEventsRequest, SessionForkRequest, SessionGoalClearRequest,
    SessionGoalGetRequest, SessionGoalSetRequest, SessionHistoryRequest, SessionInterruptRequest,
    SessionListRequest, SessionLoadedRequest, SessionMemoryRequest, SessionReadRequest,
    SessionRejoinRequest, SessionRollbackRequest, SessionSearchRequest,
    SessionSettingsUpdateRequest, SessionShowRequest, SessionSteerRequest,
    SessionTitleUpdateRequest, SessionTurnsRequest,
};
use moyai::cli::parse::parse as parse_cli;
use moyai::cli::{
    CliCommand, EventRenderer, HumanRenderer, JsonRenderer, ModelAvailabilityArgs, OutputMode,
    RunArgs, SharedConfirmationPrompt, StdConfirmationPrompt,
};
use moyai::config::{ConfigLoader, ProviderMetadataMode, ShellFamily};
#[cfg(feature = "tauri-desktop")]
use moyai::desktop;
use moyai::harness::artifact::hash_file;
use moyai::harness::{
    ArtifactStore, ContractId, ContractKind, ContractRecord, ContractStore, GateResultStore,
    HarnessEventStore, HarnessRunId, HarnessRunRecord, HarnessRunStatus, HarnessRunStore,
    ReplayExecution, ReplayMode, ReplayProfile, ReplayReportStore, ReplayStatus,
};
use moyai::runtime::{SystemClock, build_cancel_token};
use moyai::session::EditorContext;
use moyai::session::SessionStatus;
use moyai::storage::{SqliteStore, StoragePaths};
use moyai::tui;
use tempfile::NamedTempFile;

const WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("{message}");
            ExitCode::from(code)
        }
    }
}

fn run() -> Result<(), (u8, String)> {
    let command = parse_cli().map_err(|error| (2, error.to_string()))?;
    let command = hydrate_run_prompt(command).map_err(|error| (2, error))?;
    match command {
        CliCommand::Run(_)
        | CliCommand::SessionArchive(_)
        | CliCommand::SessionList(_)
        | CliCommand::SessionLoaded(_)
        | CliCommand::SessionSearch(_)
        | CliCommand::SessionSettings(_)
        | CliCommand::SessionTitle(_)
        | CliCommand::SessionInterrupt(_)
        | CliCommand::SessionCompact(_)
        | CliCommand::SessionMemory(_)
        | CliCommand::SessionGoalGet(_)
        | CliCommand::SessionGoalSet(_)
        | CliCommand::SessionGoalClear(_)
        | CliCommand::SessionShow(_)
        | CliCommand::SessionHistory(_)
        | CliCommand::SessionRead(_)
        | CliCommand::SessionRejoin(_)
        | CliCommand::SessionRollback(_)
        | CliCommand::SessionFork(_)
        | CliCommand::SessionTurns(_)
        | CliCommand::SessionEvents(_)
        | CliCommand::SessionSteer(_)
        | CliCommand::ReplayRun(_)
        | CliCommand::ReplayReport(_)
        | CliCommand::ModelAvailability(_)
        | CliCommand::SchemaExport(_)
        | CliCommand::ContractSnapshot(_) => run_with_large_stack(command),
        CliCommand::Desktop(_) => run_desktop_command(command),
        CliCommand::Tui(_) => run_on_current_thread(command),
    }
}

fn run_desktop_command(command: CliCommand) -> Result<(), (u8, String)> {
    #[cfg(feature = "tauri-desktop")]
    {
        let CliCommand::Desktop(args) = command else {
            return Err((
                2,
                "desktop launcher received a non-desktop command".to_string(),
            ));
        };
        run_desktop_on_current_thread(args)
    }
    #[cfg(not(feature = "tauri-desktop"))]
    {
        let _ = command;
        Err((
            2,
            "desktop command requires the tauri-desktop feature".to_string(),
        ))
    }
}

#[cfg(feature = "tauri-desktop")]
fn run_desktop_on_current_thread(args: moyai::cli::parse::DesktopArgs) -> Result<(), (u8, String)> {
    let Some(_desktop_instance) =
        desktop::DesktopInstanceGuard::acquire_or_notify().map_err(|error| (4, error))?
    else {
        return Ok(());
    };
    let command = CliCommand::Desktop(args.clone());
    let global_config_existed_at_launch = moyai::config::loader::global_config_path()
        .map(|path| path.exists())
        .unwrap_or(false);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| (4, format!("failed to build desktop runtime: {error}")))?;
    runtime.block_on(async move {
        let app = AppBootstrap::build(&command)
            .await
            .map_err(|error| (3, error.to_string()))?;
        desktop::run(
            app,
            desktop::DesktopArgs {
                directory: args.directory,
                session_id: args.session_id,
                continue_last: args.continue_last,
                global_config_existed_at_launch,
            },
        )
        .await
        .map_err(|error| (4, error.to_string()))
    })
}

fn run_with_large_stack(command: CliCommand) -> Result<(), (u8, String)> {
    let join_handle = std::thread::Builder::new()
        .name("moyai-worker".to_string())
        .stack_size(WORKER_STACK_BYTES)
        .spawn(move || run_on_current_thread(command))
        .map_err(|error| (4, format!("failed to spawn worker thread: {error}")))?;
    match join_handle.join() {
        Ok(result) => result,
        Err(_) => Err((4, "worker thread panicked".to_string())),
    }
}

fn run_on_current_thread(command: CliCommand) -> Result<(), (u8, String)> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| (4, format!("failed to build runtime: {error}")))?;
    runtime.block_on(run_command(command))
}

async fn run_command(command: CliCommand) -> Result<(), (u8, String)> {
    if run_harness_command(&command).map_err(|error| (4, error))? {
        return Ok(());
    }
    if let CliCommand::ModelAvailability(args) = command.clone() {
        return run_model_availability_command(args).await;
    }
    if matches!(command, CliCommand::Desktop(_)) {
        return Err((
            4,
            "desktop commands must use the guarded desktop launcher".to_string(),
        ));
    }
    let app = AppBootstrap::build(&command)
        .await
        .map_err(|error| (3, error.to_string()))?;
    if let CliCommand::Tui(args) = command.clone() {
        tui::run(app, args)
            .await
            .map_err(|error| (4, error.to_string()))?;
        return Ok(());
    }
    let wait_for_agent_tree = matches!(&command, CliCommand::Run(_));
    let mut app_command = to_app_command(&command, &app);
    let mut prompt = SharedConfirmationPrompt::new(StdConfirmationPrompt);
    if let AppCommand::Run(request) = &mut app_command {
        request.agent_confirmation = Some(prompt.clone());
    }
    install_cli_interrupt_handler(&app_command);
    let output_mode = command_output_mode(&command);
    let mut renderer = build_renderer(output_mode);
    let summary = app
        .run_service
        .execute(app_command, renderer.as_mut(), &mut prompt)
        .await
        .map_err(|error| (4, error.to_string()))?;
    if wait_for_agent_tree {
        app.run_service
            .wait_for_agent_tree_quiescence(summary.session_id)
            .await
            .map_err(|error| (4, error.to_string()))?;
    }
    if summary.status == SessionStatus::Cancelled {
        return Err((130, "run cancelled by user".to_string()));
    }
    Ok(())
}

fn install_cli_interrupt_handler(command: &AppCommand) {
    let AppCommand::Run(request) = command else {
        return;
    };
    let cancel = request.cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancel.cancel();
            eprintln!("interrupt requested; cancelling active run...");
        }
    });
}

fn hydrate_run_prompt(command: CliCommand) -> Result<CliCommand, String> {
    match command {
        CliCommand::Run(mut args) => {
            let stdin_text = read_stdin_if_piped()?;
            let prompt = match (args.prompt.take(), stdin_text) {
                (Some(existing), Some(stdin)) if !stdin.trim().is_empty() => {
                    Some(format!("{existing}\n{stdin}"))
                }
                (Some(existing), _) => Some(existing),
                (None, Some(stdin)) if !stdin.trim().is_empty() => Some(stdin),
                (None, _) => None,
            };
            if prompt
                .as_ref()
                .map(|value| value.trim().is_empty())
                .unwrap_or(true)
                && !args.review_uncommitted
                && args.review_branch.is_none()
                && args.session_id.is_none()
                && !args.continue_last
            {
                return Err("prompt is required unless resuming a session".to_string());
            }
            args.prompt = prompt;
            Ok(CliCommand::Run(args))
        }
        other => Ok(other),
    }
}

fn read_stdin_if_piped() -> Result<Option<String>, String> {
    if io::stdin().is_terminal() {
        return Ok(None);
    }
    let mut buffer = String::new();
    io::stdin()
        .read_to_string(&mut buffer)
        .map_err(|error| error.to_string())?;
    Ok(Some(buffer))
}

fn to_app_command(command: &CliCommand, app: &moyai::app::App) -> AppCommand {
    match command {
        CliCommand::Run(args) => AppCommand::Run(RunRequest {
            prompt: args.prompt.clone().unwrap_or_default(),
            session_id: args.session_id,
            continue_last: args.continue_last,
            title: args.title.clone(),
            cwd: app.workspace.cwd.clone(),
            model: args.model_override.clone().unwrap_or_default(),
            base_url: args.base_url_override.clone().unwrap_or_default(),
            config_override: None,
            output_mode: args.output_mode,
            show_reasoning: args.show_reasoning,
            prompt_dispatch: None,
            editor_context: Some(EditorContext {
                active_file: args.active_file.clone(),
                visible_files: args.visible_files.clone(),
                open_tabs: args.open_tabs.clone(),
                shell_family: app.config.shell.family.unwrap_or(if cfg!(windows) {
                    ShellFamily::PowerShell
                } else {
                    ShellFamily::Bash
                }),
                current_time_ms: SystemClock::now_ms(),
            }),
            review_request: if args.review_uncommitted {
                Some(ReviewRequest::Uncommitted)
            } else {
                args.review_branch
                    .as_ref()
                    .map(|base_ref| ReviewRequest::Branch {
                        base_ref: base_ref.clone(),
                    })
            },
            image_paths: args.image_paths.clone(),
            cancel: build_cancel_token(),
            live_config: None,
            agent_confirmation: None,
            agent_context: None,
        }),
        CliCommand::SessionList(args) => AppCommand::SessionList(SessionListRequest {
            project_id: app.workspace.project_id,
            limit: args.limit,
        }),
        CliCommand::SessionLoaded(args) => AppCommand::SessionLoaded(SessionLoadedRequest {
            project_id: app.workspace.project_id,
            limit: args.limit,
            include_archived: args.include_archived,
        }),
        CliCommand::SessionSearch(args) => AppCommand::SessionSearch(SessionSearchRequest {
            project_id: app.workspace.project_id,
            query: args.query.clone(),
            limit: args.limit,
            include_archived: args.include_archived,
        }),
        CliCommand::SessionArchive(args) => AppCommand::SessionArchive(SessionArchiveRequest {
            session_id: args.session_id,
            archived: args.archived,
        }),
        CliCommand::SessionSettings(args) => {
            AppCommand::SessionSettingsUpdate(SessionSettingsUpdateRequest {
                session_id: args.session_id,
                cwd: args.cwd.clone(),
                model: args.model.clone(),
                base_url: args.base_url.clone(),
                access_mode: args.access_mode,
                reset_model_parameters: args.reset_model_parameters,
                temperature: args.temperature,
                top_p: args.top_p,
                top_k: args.top_k,
                max_output_tokens: args.max_output_tokens,
            })
        }
        CliCommand::SessionTitle(args) => {
            AppCommand::SessionTitleUpdate(SessionTitleUpdateRequest {
                session_id: args.session_id,
                title: args.title.clone(),
            })
        }
        CliCommand::SessionInterrupt(args) => {
            AppCommand::SessionInterrupt(SessionInterruptRequest {
                session_id: args.session_id,
                reason: args.reason.clone(),
            })
        }
        CliCommand::SessionCompact(args) => AppCommand::SessionCompact(SessionCompactRequest {
            session_id: args.session_id,
            keep_recent: args.keep_recent,
        }),
        CliCommand::SessionMemory(args) => AppCommand::SessionMemory(SessionMemoryRequest {
            session_id: args.session_id,
            mode: args.mode,
        }),
        CliCommand::SessionGoalGet(args) => AppCommand::SessionGoalGet(SessionGoalGetRequest {
            session_id: args.session_id,
        }),
        CliCommand::SessionGoalSet(args) => AppCommand::SessionGoalSet(SessionGoalSetRequest {
            session_id: args.session_id,
            objective: args.objective.clone(),
            status: args.status,
            token_budget: args.token_budget,
        }),
        CliCommand::SessionGoalClear(args) => {
            AppCommand::SessionGoalClear(SessionGoalClearRequest {
                session_id: args.session_id,
            })
        }
        CliCommand::SessionShow(args) => AppCommand::SessionShow(SessionShowRequest {
            session_id: args.session_id,
            show_reasoning: args.show_reasoning,
        }),
        CliCommand::SessionHistory(args) => AppCommand::SessionHistory(SessionHistoryRequest {
            session_id: args.session_id,
            offset: args.offset,
            limit: args.limit,
        }),
        CliCommand::SessionRead(args) => AppCommand::SessionRead(SessionReadRequest {
            session_id: args.session_id,
            history_offset: args.history_offset,
            history_limit: args.history_limit,
            turn_offset: args.turn_offset,
            turn_limit: args.turn_limit,
        }),
        CliCommand::SessionRejoin(args) => AppCommand::SessionRejoin(SessionRejoinRequest {
            session_id: args.session_id,
            history_offset: args.history_offset,
            history_limit: args.history_limit,
            turn_offset: args.turn_offset,
            turn_limit: args.turn_limit,
        }),
        CliCommand::SessionRollback(args) => AppCommand::SessionRollback(SessionRollbackRequest {
            session_id: args.session_id,
            num_turns: args.num_turns,
            history_offset: args.history_offset,
            history_limit: args.history_limit,
            turn_offset: args.turn_offset,
            turn_limit: args.turn_limit,
        }),
        CliCommand::SessionFork(args) => AppCommand::SessionFork(SessionForkRequest {
            source_session_id: args.source_session_id,
            title: args.title.clone(),
            history_offset: args.history_offset,
            history_limit: args.history_limit,
            turn_offset: args.turn_offset,
            turn_limit: args.turn_limit,
        }),
        CliCommand::SessionTurns(args) => AppCommand::SessionTurns(SessionTurnsRequest {
            session_id: args.session_id,
            offset: args.offset,
            limit: args.limit,
        }),
        CliCommand::SessionEvents(args) => AppCommand::SessionEvents(SessionEventsRequest {
            session_id: args.session_id,
            offset: args.offset,
            limit: args.limit,
        }),
        CliCommand::SessionSteer(args) => AppCommand::SessionSteer(SessionSteerRequest {
            session_id: args.session_id,
            prompt: args.prompt.clone(),
            cwd: args
                .directory
                .clone()
                .unwrap_or_else(|| app.workspace.cwd.clone()),
            image_paths: args.image_paths.clone(),
            client_user_message_id: None,
        }),
        CliCommand::ReplayRun(_)
        | CliCommand::ReplayReport(_)
        | CliCommand::ModelAvailability(_)
        | CliCommand::SchemaExport(_)
        | CliCommand::ContractSnapshot(_) => {
            unreachable!("harness command is handled before renderer dispatch")
        }
        CliCommand::Tui(_) | CliCommand::Desktop(_) => {
            unreachable!("interactive command is handled before renderer dispatch")
        }
    }
}

fn command_output_mode(command: &CliCommand) -> OutputMode {
    match command {
        CliCommand::Run(args) => args.output_mode,
        CliCommand::SessionArchive(args) => args.output_mode,
        CliCommand::SessionList(args) => args.output_mode,
        CliCommand::SessionLoaded(args) => args.output_mode,
        CliCommand::SessionSearch(args) => args.output_mode,
        CliCommand::SessionSettings(args) => args.output_mode,
        CliCommand::SessionTitle(args) => args.output_mode,
        CliCommand::SessionInterrupt(args) => args.output_mode,
        CliCommand::SessionCompact(args) => args.output_mode,
        CliCommand::SessionMemory(args) => args.output_mode,
        CliCommand::SessionGoalGet(args) => args.output_mode,
        CliCommand::SessionGoalSet(args) => args.output_mode,
        CliCommand::SessionGoalClear(args) => args.output_mode,
        CliCommand::SessionShow(args) => args.output_mode,
        CliCommand::SessionHistory(args) => args.output_mode,
        CliCommand::SessionRead(args) => args.output_mode,
        CliCommand::SessionRejoin(args) => args.output_mode,
        CliCommand::SessionRollback(args) => args.output_mode,
        CliCommand::SessionFork(args) => args.output_mode,
        CliCommand::SessionTurns(args) => args.output_mode,
        CliCommand::SessionEvents(args) => args.output_mode,
        CliCommand::SessionSteer(args) => args.output_mode,
        CliCommand::ReplayRun(_)
        | CliCommand::ReplayReport(_)
        | CliCommand::ModelAvailability(_)
        | CliCommand::SchemaExport(_)
        | CliCommand::ContractSnapshot(_) => OutputMode::Json,
        CliCommand::Tui(_) | CliCommand::Desktop(_) => OutputMode::Human,
    }
}

fn build_renderer(mode: OutputMode) -> Box<dyn EventRenderer> {
    match mode {
        OutputMode::Human => Box::new(HumanRenderer::new()),
        OutputMode::Json => Box::new(JsonRenderer::new()),
    }
}

async fn run_model_availability_command(args: ModelAvailabilityArgs) -> Result<(), (u8, String)> {
    let start_dir = match args.directory.as_ref() {
        Some(directory) => directory.clone(),
        None => current_utf8_dir().map_err(|error| (3, error))?,
    };
    let config_args = RunArgs {
        prompt: None,
        session_id: None,
        continue_last: false,
        title: None,
        directory: args.directory.clone(),
        model_override: args.model_override.clone(),
        base_url_override: args.base_url_override.clone(),
        output_mode: OutputMode::Json,
        show_reasoning: false,
        review_uncommitted: false,
        review_branch: None,
        active_file: None,
        open_tabs: Vec::new(),
        visible_files: Vec::new(),
        image_paths: Vec::new(),
    };
    let mut config = ConfigLoader::load(&start_dir, Some(&config_args))
        .map_err(|error| (3, error.to_string()))?;
    if args.openai_compatible_only {
        config.model.provider_metadata_mode = ProviderMetadataMode::OpenAiCompatibleOnly;
    }
    let report = moyai::llm::check_model_availability(
        &config,
        args.model_override.as_deref(),
        args.base_url_override.as_deref(),
        args.require_vision,
    )
    .await;
    let encoded = serde_json::to_string_pretty(&report).map_err(|error| (4, error.to_string()))?;
    if let Some(output) = args.output.as_ref() {
        write_cli_artifact_atomic(output, &encoded).map_err(|error| (4, error))?;
    }
    println!("{encoded}");
    if !matches!(report.status, moyai::llm::ModelAvailabilityStatus::Pass) {
        return Err((4, "model availability gate did not pass".to_string()));
    }
    Ok(())
}

fn run_harness_command(command: &CliCommand) -> Result<bool, String> {
    match command {
        CliCommand::SchemaExport(args) => {
            moyai::harness::schema::write_schema_files(&args.output)
                .map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "completed",
                    "kind": "schema_exported",
                    "output": args.output
                })
            );
            Ok(true)
        }
        CliCommand::ReplayRun(args) => {
            let mode = args.mode.parse::<ReplayMode>()?;
            let input = moyai::harness::ReplayRunInput {
                schema_version: "replay.run_input.v1".to_string(),
                run_id: None,
                mode,
                scenario_id: args.scenario_id.clone(),
                artifact_root: args.artifact_root.clone(),
                event_log: args.event_log.clone(),
                artifact_manifest: args.artifact_manifest.clone(),
                contract_registry: args.contract_registry.clone(),
                profile: ReplayProfile::default(),
            };
            let execution = moyai::harness::ReplayService::replay_with_evidence(input)
                .map_err(|error| error.to_string())?;
            persist_replay_execution(
                &execution,
                &args.artifact_root,
                &args.mode,
                args.data_dir.as_ref(),
            )?;
            moyai::harness::replay::write_report(&execution.report, &args.output)
                .map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string(&execution.report).map_err(|error| error.to_string())?
            );
            if matches!(
                execution.report.status,
                moyai::harness::ReplayStatus::Fail | moyai::harness::ReplayStatus::Blocked
            ) {
                return Err(format!("replay did not pass: {}", execution.report.summary));
            }
            Ok(true)
        }
        CliCommand::ReplayReport(args) => {
            let store = open_harness_store(args.data_dir.as_ref())?;
            let run_id = args
                .run_id
                .parse::<HarnessRunId>()
                .map_err(|error| format!("invalid run id `{}`: {error}", args.run_id))?;
            let report = store
                .harness_replay_report_store()
                .get_report(run_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("replay report not found for run id `{}`", args.run_id))?;
            println!(
                "{}",
                serde_json::to_string(&report).map_err(|error| error.to_string())?
            );
            Ok(true)
        }
        CliCommand::ContractSnapshot(args) => {
            let source_path = args.source.clone();
            let (content_sha256, _) = hash_file(&source_path).map_err(|error| error.to_string())?;
            let record = ContractRecord {
                id: ContractId::new(format!("scenario.{}", args.scenario_id)),
                kind: ContractKind::Scenario,
                version: "manual".to_string(),
                source_path,
                content_sha256,
                schema_ref: None,
                model_visible_summary: Some(format!(
                    "Scenario contract snapshot for {}",
                    args.scenario_id
                )),
            };
            let encoded =
                serde_json::to_string_pretty(&vec![record]).map_err(|error| error.to_string())?;
            write_cli_artifact_atomic(&args.output, &encoded)?;
            println!(
                "{}",
                serde_json::json!({
                    "status": "completed",
                    "kind": "contract_snapshot_written",
                    "output": args.output
                })
            );
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn open_harness_store(data_dir: Option<&Utf8PathBuf>) -> Result<SqliteStore, String> {
    let paths = match data_dir {
        Some(data_dir) => StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        },
        None => StoragePaths::discover().map_err(|error| error.to_string())?,
    };
    let store = SqliteStore::open(&paths).map_err(|error| error.to_string())?;
    store.migrate().map_err(|error| error.to_string())?;
    Ok(store)
}

fn persist_replay_execution(
    execution: &ReplayExecution,
    artifact_root: &Utf8PathBuf,
    mode: &str,
    data_dir: Option<&Utf8PathBuf>,
) -> Result<(), String> {
    let store = open_harness_store(data_dir)?;
    let now = SystemClock::now_ms();
    let run = HarnessRunRecord {
        id: execution.report.run_id,
        session_id: None,
        workspace_root: current_utf8_dir()?,
        artifact_root: artifact_root.clone(),
        mode: mode.to_string(),
        started_at_ms: now,
        completed_at_ms: Some(now),
        status: harness_run_status(execution.report.status),
    };
    store
        .harness_run_store()
        .upsert_run(&run)
        .map_err(|error| error.to_string())?;
    let event_store = store.harness_event_store();
    for event in &execution.events {
        event_store
            .append_event(event)
            .map_err(|error| error.to_string())?;
    }
    let artifact_store = store.harness_artifact_store();
    for artifact in &execution.artifacts {
        artifact_store
            .insert_artifact(artifact)
            .map_err(|error| error.to_string())?;
    }
    let contract_store = store.harness_contract_store();
    for contract in &execution.contracts {
        contract_store
            .upsert_contract(execution.report.run_id, contract)
            .map_err(|error| error.to_string())?;
    }
    let gate_store = store.harness_gate_result_store();
    for result in &execution.report.gate_results {
        gate_store
            .insert_gate_result(execution.report.run_id, result)
            .map_err(|error| error.to_string())?;
    }
    store
        .harness_replay_report_store()
        .save_report(&execution.report)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn harness_run_status(status: ReplayStatus) -> HarnessRunStatus {
    match status {
        ReplayStatus::Pass => HarnessRunStatus::Pass,
        ReplayStatus::Fail => HarnessRunStatus::Fail,
        ReplayStatus::Blocked => HarnessRunStatus::Blocked,
    }
}

fn current_utf8_dir() -> Result<Utf8PathBuf, String> {
    Utf8PathBuf::from_path_buf(std::env::current_dir().map_err(|error| error.to_string())?)
        .map_err(|_| "current directory is not valid UTF-8".to_string())
}

fn write_cli_artifact_atomic(path: &Utf8PathBuf, text: &str) -> Result<(), String> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_str().is_empty() => parent.to_path_buf(),
        _ => current_utf8_dir()?,
    };
    std::fs::create_dir_all(parent.as_std_path()).map_err(|error| error.to_string())?;
    let mut temp =
        NamedTempFile::new_in(parent.as_std_path()).map_err(|error| error.to_string())?;
    temp.write_all(text.as_bytes())
        .map_err(|error| error.to_string())?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temp.persist(path.as_std_path())
        .map(|_| ())
        .map_err(|error| error.error.to_string())
}
