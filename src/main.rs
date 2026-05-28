use std::io::{self, IsTerminal, Read};
use std::process::ExitCode;

use camino::Utf8PathBuf;
use moyai::app::{
    AppBootstrap, AppCommand, ReviewRequest, RunRequest, SessionListRequest, SessionShowRequest,
};
use moyai::cli::parse::parse as parse_cli;
use moyai::cli::{
    CliCommand, EventRenderer, HumanRenderer, JsonRenderer, ModelAvailabilityArgs, OutputMode,
    RunArgs, StdConfirmationPrompt,
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
        | CliCommand::SessionList(_)
        | CliCommand::SessionShow(_)
        | CliCommand::ReplayRun(_)
        | CliCommand::ReplayReport(_)
        | CliCommand::PreflightRun(_)
        | CliCommand::PreflightArtifact(_)
        | CliCommand::ModelAvailability(_)
        | CliCommand::SchemaExport(_)
        | CliCommand::ContractSnapshot(_)
        | CliCommand::ManualStRoute(_) => run_with_large_stack(command),
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
    if let CliCommand::ManualStRoute(args) = command.clone() {
        let route = args
            .route
            .parse::<moyai::harness::manual_st::ManualStRouteKind>()
            .map_err(|error| (2, error))?;
        let result = moyai::harness::manual_st::run_manual_st_route(
            moyai::harness::manual_st::ManualStRouteRunConfig {
                route,
                output_root: args.output_root,
                preflight_report: args.preflight_report,
                model_override: args.model_override,
                base_url_override: args.base_url_override,
                provider_metadata_mode_override: args
                    .openai_compatible_only
                    .then_some(moyai::config::ProviderMetadataMode::OpenAiCompatibleOnly),
                context_window_override: args.context_window,
                max_output_tokens_override: args.max_output_tokens,
                max_turn_seconds: args.max_turn_seconds,
                dry_run: args.dry_run,
            },
        )
        .await
        .map_err(|error| (4, error))?;
        println!(
            "{}",
            serde_json::to_string(&result).map_err(|error| (4, error.to_string()))?
        );
        if !matches!(
            result.route_level_verdict,
            moyai::harness::manual_st::RouteVerdict::Pass
                | moyai::harness::manual_st::RouteVerdict::NotRun
        ) {
            return Err((
                4,
                result
                    .stop_reason
                    .unwrap_or_else(|| "manual ST route did not pass".to_string()),
            ));
        }
        return Ok(());
    }
    let desktop_global_config_existed_at_launch = if matches!(command, CliCommand::Desktop(_)) {
        moyai::config::loader::global_config_path()
            .map(|path| path.exists())
            .unwrap_or(false)
    } else {
        false
    };
    let app = AppBootstrap::build(&command)
        .await
        .map_err(|error| (3, error.to_string()))?;
    if let CliCommand::Tui(args) = command.clone() {
        tui::run(app, args)
            .await
            .map_err(|error| (4, error.to_string()))?;
        return Ok(());
    }
    if let CliCommand::Desktop(args) = command.clone() {
        #[cfg(feature = "tauri-desktop")]
        {
            desktop::run(
                app,
                desktop::DesktopArgs {
                    directory: args.directory.clone(),
                    session_id: args.session_id,
                    continue_last: args.continue_last,
                    global_config_existed_at_launch: desktop_global_config_existed_at_launch,
                },
            )
            .await
            .map_err(|error| (4, error.to_string()))?;
            return Ok(());
        }
        #[cfg(not(feature = "tauri-desktop"))]
        {
            let _ = (app, args);
            return Err((
                2,
                "desktop command requires the tauri-desktop feature".to_string(),
            ));
        }
    }
    let app_command = to_app_command(&command, &app);
    install_cli_interrupt_handler(&app_command);
    let output_mode = command_output_mode(&command);
    let mut renderer = build_renderer(output_mode);
    let mut prompt = StdConfirmationPrompt;
    let summary = app
        .run_service
        .execute(app_command, renderer.as_mut(), &mut prompt)
        .await
        .map_err(|error| (4, error.to_string()))?;
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
            model: args
                .model_override
                .clone()
                .unwrap_or_else(|| app.config.model.model.clone()),
            base_url: args
                .base_url_override
                .clone()
                .unwrap_or_else(|| app.config.model.base_url.clone()),
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
        }),
        CliCommand::SessionList(args) => AppCommand::SessionList(SessionListRequest {
            project_id: app.workspace.project_id,
            limit: args.limit,
        }),
        CliCommand::SessionShow(args) => AppCommand::SessionShow(SessionShowRequest {
            session_id: args.session_id,
            show_reasoning: args.show_reasoning,
        }),
        CliCommand::ReplayRun(_)
        | CliCommand::ReplayReport(_)
        | CliCommand::PreflightRun(_)
        | CliCommand::PreflightArtifact(_)
        | CliCommand::ModelAvailability(_)
        | CliCommand::SchemaExport(_)
        | CliCommand::ContractSnapshot(_)
        | CliCommand::ManualStRoute(_) => {
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
        CliCommand::SessionList(args) => args.output_mode,
        CliCommand::SessionShow(args) => args.output_mode,
        CliCommand::ReplayRun(_)
        | CliCommand::ReplayReport(_)
        | CliCommand::PreflightRun(_)
        | CliCommand::PreflightArtifact(_)
        | CliCommand::ModelAvailability(_)
        | CliCommand::SchemaExport(_)
        | CliCommand::ContractSnapshot(_)
        | CliCommand::ManualStRoute(_) => OutputMode::Json,
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
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent.as_std_path())
                .map_err(|error| (4, error.to_string()))?;
        }
        std::fs::write(output.as_std_path(), &encoded).map_err(|error| (4, error.to_string()))?;
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
        CliCommand::PreflightRun(args) => {
            let report = moyai::harness::preflight::run_default_active_preflight();
            if let Some(output) = args.output.as_ref() {
                moyai::harness::preflight::write_preflight_report(&report, output)
                    .map_err(|error| error.to_string())?;
            }
            println!(
                "{}",
                serde_json::to_string(&report).map_err(|error| error.to_string())?
            );
            if !matches!(
                report.status,
                moyai::harness::preflight::PreflightResultStatus::Pass
            ) {
                return Err("preflight did not pass".to_string());
            }
            Ok(true)
        }
        CliCommand::PreflightArtifact(args) => {
            let report = moyai::harness::preflight::run_artifact_replay_preflight(
                &args.artifact_root,
                args.failure_ids.clone(),
            )
            .map_err(|error| error.to_string())?;
            if let Some(output) = args.output.as_ref() {
                moyai::harness::preflight::write_preflight_report(&report, output)
                    .map_err(|error| error.to_string())?;
            }
            println!(
                "{}",
                serde_json::to_string(&report).map_err(|error| error.to_string())?
            );
            if !matches!(
                report.status,
                moyai::harness::preflight::PreflightResultStatus::Pass
            ) {
                return Err("artifact preflight did not pass".to_string());
            }
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
            if let Some(parent) = args.output.parent() {
                std::fs::create_dir_all(parent.as_std_path()).map_err(|error| error.to_string())?;
            }
            std::fs::write(
                args.output.as_std_path(),
                serde_json::to_string_pretty(&vec![record]).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
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
