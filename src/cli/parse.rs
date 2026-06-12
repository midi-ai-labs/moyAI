use camino::Utf8PathBuf;
use clap::{Args, Parser, Subcommand, error::ErrorKind};

use crate::config::AccessMode;
use crate::error::CliUsageError;
use crate::session::{SessionId, SessionMemoryMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    Human,
    Json,
}

#[derive(Debug, Clone)]
pub struct RunArgs {
    pub prompt: Option<String>,
    pub session_id: Option<SessionId>,
    pub continue_last: bool,
    pub title: Option<String>,
    pub directory: Option<Utf8PathBuf>,
    pub model_override: Option<String>,
    pub base_url_override: Option<String>,
    pub output_mode: OutputMode,
    pub show_reasoning: bool,
    pub review_uncommitted: bool,
    pub review_branch: Option<String>,
    pub active_file: Option<Utf8PathBuf>,
    pub open_tabs: Vec<Utf8PathBuf>,
    pub visible_files: Vec<Utf8PathBuf>,
    pub image_paths: Vec<Utf8PathBuf>,
}

#[derive(Debug, Clone)]
pub struct SessionListArgs {
    pub directory: Option<Utf8PathBuf>,
    pub limit: usize,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionLoadedArgs {
    pub directory: Option<Utf8PathBuf>,
    pub limit: usize,
    pub include_archived: bool,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionSearchArgs {
    pub directory: Option<Utf8PathBuf>,
    pub query: String,
    pub limit: usize,
    pub include_archived: bool,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionArchiveArgs {
    pub session_id: SessionId,
    pub archived: bool,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionSettingsArgs {
    pub session_id: SessionId,
    pub cwd: Option<Utf8PathBuf>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub access_mode: Option<AccessMode>,
    pub reset_model_parameters: bool,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionTitleArgs {
    pub session_id: SessionId,
    pub title: String,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionInterruptArgs {
    pub session_id: SessionId,
    pub reason: String,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionCompactArgs {
    pub session_id: SessionId,
    pub keep_recent: usize,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionMemoryArgs {
    pub session_id: SessionId,
    pub mode: SessionMemoryMode,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionShowArgs {
    pub session_id: SessionId,
    pub output_mode: OutputMode,
    pub show_reasoning: bool,
}

#[derive(Debug, Clone)]
pub struct SessionHistoryArgs {
    pub session_id: SessionId,
    pub offset: usize,
    pub limit: usize,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionReadArgs {
    pub session_id: SessionId,
    pub history_offset: usize,
    pub history_limit: usize,
    pub turn_offset: usize,
    pub turn_limit: usize,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionRejoinArgs {
    pub session_id: SessionId,
    pub history_offset: usize,
    pub history_limit: usize,
    pub turn_offset: usize,
    pub turn_limit: usize,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionRollbackArgs {
    pub session_id: SessionId,
    pub num_turns: usize,
    pub history_offset: usize,
    pub history_limit: usize,
    pub turn_offset: usize,
    pub turn_limit: usize,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionForkArgs {
    pub source_session_id: SessionId,
    pub title: Option<String>,
    pub history_offset: usize,
    pub history_limit: usize,
    pub turn_offset: usize,
    pub turn_limit: usize,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionTurnsArgs {
    pub session_id: SessionId,
    pub offset: usize,
    pub limit: usize,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionEventsArgs {
    pub session_id: SessionId,
    pub offset: usize,
    pub limit: usize,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct SessionSteerArgs {
    pub session_id: SessionId,
    pub prompt: String,
    pub directory: Option<Utf8PathBuf>,
    pub image_paths: Vec<Utf8PathBuf>,
    pub output_mode: OutputMode,
}

#[derive(Debug, Clone)]
pub struct TuiArgs {
    pub directory: Option<Utf8PathBuf>,
    pub session_id: Option<SessionId>,
    pub continue_last: bool,
}

#[derive(Debug, Clone)]
pub struct DesktopArgs {
    pub directory: Option<Utf8PathBuf>,
    pub session_id: Option<SessionId>,
    pub continue_last: bool,
}

#[derive(Debug, Clone)]
pub struct ReplayRunArgs {
    pub artifact_root: Utf8PathBuf,
    pub scenario_id: String,
    pub mode: String,
    pub output: Utf8PathBuf,
    pub event_log: Option<Utf8PathBuf>,
    pub artifact_manifest: Option<Utf8PathBuf>,
    pub contract_registry: Option<Utf8PathBuf>,
    pub data_dir: Option<Utf8PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ReplayReportArgs {
    pub run_id: String,
    pub data_dir: Option<Utf8PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ModelAvailabilityArgs {
    pub directory: Option<Utf8PathBuf>,
    pub model_override: Option<String>,
    pub base_url_override: Option<String>,
    pub output: Option<Utf8PathBuf>,
    pub require_vision: bool,
    pub openai_compatible_only: bool,
}

#[derive(Debug, Clone)]
pub struct SchemaExportArgs {
    pub output: Utf8PathBuf,
}

#[derive(Debug, Clone)]
pub struct ContractSnapshotArgs {
    pub scenario_id: String,
    pub source: Utf8PathBuf,
    pub output: Utf8PathBuf,
}

#[derive(Debug, Clone)]
pub enum CliCommand {
    Run(RunArgs),
    SessionArchive(SessionArchiveArgs),
    SessionList(SessionListArgs),
    SessionLoaded(SessionLoadedArgs),
    SessionSearch(SessionSearchArgs),
    SessionSettings(SessionSettingsArgs),
    SessionTitle(SessionTitleArgs),
    SessionInterrupt(SessionInterruptArgs),
    SessionCompact(SessionCompactArgs),
    SessionMemory(SessionMemoryArgs),
    SessionShow(SessionShowArgs),
    SessionHistory(SessionHistoryArgs),
    SessionRead(SessionReadArgs),
    SessionRejoin(SessionRejoinArgs),
    SessionRollback(SessionRollbackArgs),
    SessionFork(SessionForkArgs),
    SessionTurns(SessionTurnsArgs),
    SessionEvents(SessionEventsArgs),
    SessionSteer(SessionSteerArgs),
    Tui(TuiArgs),
    Desktop(DesktopArgs),
    ReplayRun(ReplayRunArgs),
    ReplayReport(ReplayReportArgs),
    ModelAvailability(ModelAvailabilityArgs),
    SchemaExport(SchemaExportArgs),
    ContractSnapshot(ContractSnapshotArgs),
}

pub fn parse() -> Result<CliCommand, CliUsageError> {
    let cli = match RootCli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            error
                .print()
                .map_err(|print_error| CliUsageError::Message(print_error.to_string()))?;
            std::process::exit(0);
        }
        Err(error) => return Err(CliUsageError::Message(error.to_string())),
    };
    match cli.command {
        RootCommand::Run(args) => {
            if args.session_id.is_some() && args.continue_last {
                return Err(CliUsageError::Message(
                    "`--session` and `--continue-last` cannot be used together".to_string(),
                ));
            }
            Ok(CliCommand::Run(RunArgs {
                prompt: if args.prompt.is_empty() {
                    None
                } else {
                    Some(args.prompt.join(" "))
                },
                session_id: args
                    .session_id
                    .map(|value| value.parse())
                    .transpose()
                    .map_err(|error| {
                        CliUsageError::Message(format!("invalid session id: {error}"))
                    })?,
                continue_last: args.continue_last,
                title: args.title,
                directory: args.directory,
                model_override: args.model_override,
                base_url_override: args.base_url_override,
                output_mode: args.output_mode,
                show_reasoning: args.show_reasoning,
                review_uncommitted: args.review_uncommitted,
                review_branch: args.review_branch,
                active_file: args.active_file,
                open_tabs: args.open_tabs,
                visible_files: args.visible_files,
                image_paths: args.image_paths,
            }))
        }
        RootCommand::Tui(args) => {
            if args.session_id.is_some() && args.continue_last {
                return Err(CliUsageError::Message(
                    "`--session` and `--continue-last` cannot be used together".to_string(),
                ));
            }
            Ok(CliCommand::Tui(TuiArgs {
                directory: args.directory,
                session_id: args
                    .session_id
                    .map(|value| value.parse())
                    .transpose()
                    .map_err(|error| {
                        CliUsageError::Message(format!("invalid session id: {error}"))
                    })?,
                continue_last: args.continue_last,
            }))
        }
        RootCommand::Desktop(args) => {
            if args.session_id.is_some() && args.continue_last {
                return Err(CliUsageError::Message(
                    "`--session` and `--continue-last` cannot be used together".to_string(),
                ));
            }
            Ok(CliCommand::Desktop(DesktopArgs {
                directory: args.directory,
                session_id: args
                    .session_id
                    .map(|value| value.parse())
                    .transpose()
                    .map_err(|error| {
                        CliUsageError::Message(format!("invalid session id: {error}"))
                    })?,
                continue_last: args.continue_last,
            }))
        }
        RootCommand::Session { command } => match command {
            SessionCommand::List(args) => Ok(CliCommand::SessionList(SessionListArgs {
                directory: args.directory,
                limit: args.limit,
                output_mode: args.output_mode,
            })),
            SessionCommand::Loaded(args) => Ok(CliCommand::SessionLoaded(SessionLoadedArgs {
                directory: args.directory,
                limit: args.limit,
                include_archived: args.include_archived,
                output_mode: args.output_mode,
            })),
            SessionCommand::Search(args) => {
                let query = args.query.join(" ");
                if query.trim().is_empty() {
                    return Err(CliUsageError::Message(
                        "session search query must not be empty".to_string(),
                    ));
                }
                Ok(CliCommand::SessionSearch(SessionSearchArgs {
                    directory: args.directory,
                    query,
                    limit: args.limit,
                    include_archived: args.include_archived,
                    output_mode: args.output_mode,
                }))
            }
            SessionCommand::Archive(args) => Ok(CliCommand::SessionArchive(SessionArchiveArgs {
                session_id: args.session_id.parse().map_err(|error| {
                    CliUsageError::Message(format!("invalid session id: {error}"))
                })?,
                archived: true,
                output_mode: args.output_mode,
            })),
            SessionCommand::Unarchive(args) => Ok(CliCommand::SessionArchive(SessionArchiveArgs {
                session_id: args.session_id.parse().map_err(|error| {
                    CliUsageError::Message(format!("invalid session id: {error}"))
                })?,
                archived: false,
                output_mode: args.output_mode,
            })),
            SessionCommand::Settings(args) => {
                if args.cwd.is_none()
                    && args.model.is_none()
                    && args.base_url.is_none()
                    && args.access_mode.is_none()
                    && !args.reset_model_parameters
                    && args.temperature.is_none()
                    && args.top_p.is_none()
                    && args.top_k.is_none()
                    && args.max_output_tokens.is_none()
                {
                    return Err(CliUsageError::Message(
                        "session settings requires at least one setting flag".to_string(),
                    ));
                }
                if let Some(value) = args.temperature {
                    validate_cli_finite_non_negative("session settings --temperature", value)?;
                }
                if let Some(value) = args.top_p {
                    validate_cli_finite_range("session settings --top-p", value, 0.0, 1.0)?;
                }
                if args.top_k == Some(0) {
                    return Err(CliUsageError::Message(
                        "session settings --top-k must be greater than zero".to_string(),
                    ));
                }
                if args.max_output_tokens == Some(0) {
                    return Err(CliUsageError::Message(
                        "session settings --max-output-tokens must be greater than zero"
                            .to_string(),
                    ));
                }
                Ok(CliCommand::SessionSettings(SessionSettingsArgs {
                    session_id: args.session_id.parse().map_err(|error| {
                        CliUsageError::Message(format!("invalid session id: {error}"))
                    })?,
                    cwd: args.cwd,
                    model: args.model,
                    base_url: args.base_url,
                    access_mode: args
                        .access_mode
                        .as_deref()
                        .map(parse_cli_access_mode)
                        .transpose()?,
                    reset_model_parameters: args.reset_model_parameters,
                    temperature: args.temperature,
                    top_p: args.top_p,
                    top_k: args.top_k,
                    max_output_tokens: args.max_output_tokens,
                    output_mode: args.output_mode,
                }))
            }
            SessionCommand::Title(args) => {
                let title = args.title.join(" ");
                if title.trim().is_empty() {
                    return Err(CliUsageError::Message(
                        "session title must not be empty".to_string(),
                    ));
                }
                Ok(CliCommand::SessionTitle(SessionTitleArgs {
                    session_id: args.session_id.parse().map_err(|error| {
                        CliUsageError::Message(format!("invalid session id: {error}"))
                    })?,
                    title,
                    output_mode: args.output_mode,
                }))
            }
            SessionCommand::Interrupt(args) => {
                Ok(CliCommand::SessionInterrupt(SessionInterruptArgs {
                    session_id: args.session_id.parse().map_err(|error| {
                        CliUsageError::Message(format!("invalid session id: {error}"))
                    })?,
                    reason: args.reason.join(" "),
                    output_mode: args.output_mode,
                }))
            }
            SessionCommand::Compact(args) => {
                if args.keep_recent == 0 {
                    return Err(CliUsageError::Message(
                        "session compact --keep-recent must be greater than zero".to_string(),
                    ));
                }
                Ok(CliCommand::SessionCompact(SessionCompactArgs {
                    session_id: args.session_id.parse().map_err(|error| {
                        CliUsageError::Message(format!("invalid session id: {error}"))
                    })?,
                    keep_recent: args.keep_recent,
                    output_mode: args.output_mode,
                }))
            }
            SessionCommand::Memory(args) => {
                let requested_mode = args
                    .mode
                    .as_deref()
                    .map(parse_cli_memory_mode)
                    .transpose()?;
                if requested_mode.is_some() == args.reset {
                    return Err(CliUsageError::Message(
                        "session memory requires exactly one of --mode or --reset".to_string(),
                    ));
                }
                Ok(CliCommand::SessionMemory(SessionMemoryArgs {
                    session_id: args.session_id.parse().map_err(|error| {
                        CliUsageError::Message(format!("invalid session id: {error}"))
                    })?,
                    mode: requested_mode.unwrap_or(SessionMemoryMode::Enabled),
                    output_mode: args.output_mode,
                }))
            }
            SessionCommand::Show(args) => Ok(CliCommand::SessionShow(SessionShowArgs {
                session_id: args.session_id.parse().map_err(|error| {
                    CliUsageError::Message(format!("invalid session id: {error}"))
                })?,
                output_mode: args.output_mode,
                show_reasoning: args.show_reasoning,
            })),
            SessionCommand::History(args) => Ok(CliCommand::SessionHistory(SessionHistoryArgs {
                session_id: args.session_id.parse().map_err(|error| {
                    CliUsageError::Message(format!("invalid session id: {error}"))
                })?,
                offset: args.offset,
                limit: args.limit,
                output_mode: args.output_mode,
            })),
            SessionCommand::Read(args) => Ok(CliCommand::SessionRead(SessionReadArgs {
                session_id: args.session_id.parse().map_err(|error| {
                    CliUsageError::Message(format!("invalid session id: {error}"))
                })?,
                history_offset: args.history_offset,
                history_limit: args.history_limit,
                turn_offset: args.turn_offset,
                turn_limit: args.turn_limit,
                output_mode: args.output_mode,
            })),
            SessionCommand::Rejoin(args) => Ok(CliCommand::SessionRejoin(SessionRejoinArgs {
                session_id: args.session_id.parse().map_err(|error| {
                    CliUsageError::Message(format!("invalid session id: {error}"))
                })?,
                history_offset: args.history_offset,
                history_limit: args.history_limit,
                turn_offset: args.turn_offset,
                turn_limit: args.turn_limit,
                output_mode: args.output_mode,
            })),
            SessionCommand::Rollback(args) => {
                if args.num_turns == 0 {
                    return Err(CliUsageError::Message(
                        "session rollback --turns must be greater than zero".to_string(),
                    ));
                }
                Ok(CliCommand::SessionRollback(SessionRollbackArgs {
                    session_id: args.session_id.parse().map_err(|error| {
                        CliUsageError::Message(format!("invalid session id: {error}"))
                    })?,
                    num_turns: args.num_turns,
                    history_offset: args.history_offset,
                    history_limit: args.history_limit,
                    turn_offset: args.turn_offset,
                    turn_limit: args.turn_limit,
                    output_mode: args.output_mode,
                }))
            }
            SessionCommand::Fork(args) => Ok(CliCommand::SessionFork(SessionForkArgs {
                source_session_id: args.session_id.parse().map_err(|error| {
                    CliUsageError::Message(format!("invalid session id: {error}"))
                })?,
                title: args.title,
                history_offset: args.history_offset,
                history_limit: args.history_limit,
                turn_offset: args.turn_offset,
                turn_limit: args.turn_limit,
                output_mode: args.output_mode,
            })),
            SessionCommand::Turns(args) => Ok(CliCommand::SessionTurns(SessionTurnsArgs {
                session_id: args.session_id.parse().map_err(|error| {
                    CliUsageError::Message(format!("invalid session id: {error}"))
                })?,
                offset: args.offset,
                limit: args.limit,
                output_mode: args.output_mode,
            })),
            SessionCommand::Events(args) => Ok(CliCommand::SessionEvents(SessionEventsArgs {
                session_id: args.session_id.parse().map_err(|error| {
                    CliUsageError::Message(format!("invalid session id: {error}"))
                })?,
                offset: args.offset,
                limit: args.limit,
                output_mode: args.output_mode,
            })),
            SessionCommand::Steer(args) => {
                let prompt = args.prompt.join(" ");
                if prompt.trim().is_empty() {
                    return Err(CliUsageError::Message(
                        "session steer prompt must not be empty".to_string(),
                    ));
                }
                Ok(CliCommand::SessionSteer(SessionSteerArgs {
                    session_id: args.session_id.parse().map_err(|error| {
                        CliUsageError::Message(format!("invalid session id: {error}"))
                    })?,
                    prompt,
                    directory: args.directory,
                    image_paths: args.image_paths,
                    output_mode: args.output_mode,
                }))
            }
        },
        RootCommand::Replay { command } => match command {
            ReplayCommand::Run(args) => Ok(CliCommand::ReplayRun(ReplayRunArgs {
                artifact_root: args.artifact_root,
                scenario_id: args.scenario_id,
                mode: args.mode,
                output: args.output,
                event_log: args.event_log,
                artifact_manifest: args.artifact_manifest,
                contract_registry: args.contract_registry,
                data_dir: args.data_dir,
            })),
            ReplayCommand::Report(args) => Ok(CliCommand::ReplayReport(ReplayReportArgs {
                run_id: args.run_id,
                data_dir: args.data_dir,
            })),
        },
        RootCommand::Model { command } => match command {
            ModelCommand::Availability(args) => {
                Ok(CliCommand::ModelAvailability(ModelAvailabilityArgs {
                    directory: args.directory,
                    model_override: args.model_override,
                    base_url_override: args.base_url_override,
                    output: args.output,
                    require_vision: args.require_vision,
                    openai_compatible_only: args.openai_compatible_only,
                }))
            }
        },
        RootCommand::Schema { command } => match command {
            SchemaCommand::Export(args) => Ok(CliCommand::SchemaExport(SchemaExportArgs {
                output: args.output,
            })),
        },
        RootCommand::Contract { command } => match command {
            ContractCommand::Snapshot(args) => {
                Ok(CliCommand::ContractSnapshot(ContractSnapshotArgs {
                    scenario_id: args.scenario_id,
                    source: args.source,
                    output: args.output,
                }))
            }
        },
    }
}

fn parse_cli_access_mode(value: &str) -> Result<AccessMode, CliUsageError> {
    AccessMode::parse(value).ok_or_else(|| {
        CliUsageError::Message(format!(
            "invalid access mode `{value}`; expected default, auto_review, or full_access"
        ))
    })
}

fn parse_cli_memory_mode(value: &str) -> Result<SessionMemoryMode, CliUsageError> {
    SessionMemoryMode::parse(value).ok_or_else(|| {
        CliUsageError::Message(format!(
            "invalid memory mode `{value}`; expected enabled or disabled"
        ))
    })
}

fn validate_cli_finite_non_negative(label: &str, value: f64) -> Result<(), CliUsageError> {
    if !value.is_finite() || value < 0.0 {
        return Err(CliUsageError::Message(format!(
            "{label} must be finite and non-negative"
        )));
    }
    Ok(())
}

fn validate_cli_finite_range(
    label: &str,
    value: f64,
    min: f64,
    max: f64,
) -> Result<(), CliUsageError> {
    if !value.is_finite() || value < min || value > max {
        return Err(CliUsageError::Message(format!(
            "{label} must be finite and between {min} and {max}"
        )));
    }
    Ok(())
}

#[derive(Parser)]
#[command(name = "moyai", version)]
struct RootCli {
    #[command(subcommand)]
    command: RootCommand,
}

#[derive(Subcommand)]
enum RootCommand {
    Run(RunCommand),
    Tui(TuiCommand),
    Desktop(DesktopCommand),
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    Replay {
        #[command(subcommand)]
        command: ReplayCommand,
    },
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    Schema {
        #[command(subcommand)]
        command: SchemaCommand,
    },
    Contract {
        #[command(subcommand)]
        command: ContractCommand,
    },
}

#[derive(Args)]
struct RunCommand {
    #[arg()]
    prompt: Vec<String>,
    #[arg(long = "session")]
    session_id: Option<String>,
    #[arg(long)]
    continue_last: bool,
    #[arg(long = "title")]
    title: Option<String>,
    #[arg(long = "dir")]
    directory: Option<Utf8PathBuf>,
    #[arg(long = "model")]
    model_override: Option<String>,
    #[arg(long = "base-url")]
    base_url_override: Option<String>,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
    #[arg(long = "show-reasoning")]
    show_reasoning: bool,
    #[arg(long = "review-uncommitted", conflicts_with = "review_branch")]
    review_uncommitted: bool,
    #[arg(
        long = "review-branch",
        value_name = "BASE_REF",
        conflicts_with = "review_uncommitted"
    )]
    review_branch: Option<String>,
    #[arg(long = "active-file")]
    active_file: Option<Utf8PathBuf>,
    #[arg(long = "open-tab")]
    open_tabs: Vec<Utf8PathBuf>,
    #[arg(long = "visible-file")]
    visible_files: Vec<Utf8PathBuf>,
    #[arg(long = "image", value_name = "PATH")]
    image_paths: Vec<Utf8PathBuf>,
}

#[derive(Args)]
struct TuiCommand {
    #[arg(long = "dir")]
    directory: Option<Utf8PathBuf>,
    #[arg(long = "session")]
    session_id: Option<String>,
    #[arg(long)]
    continue_last: bool,
}

#[derive(Args)]
struct DesktopCommand {
    #[arg(long = "dir")]
    directory: Option<Utf8PathBuf>,
    #[arg(long = "session")]
    session_id: Option<String>,
    #[arg(long)]
    continue_last: bool,
}

#[derive(Subcommand)]
enum SessionCommand {
    Archive(SessionArchiveCommand),
    List(SessionListCommand),
    Loaded(SessionLoadedCommand),
    Search(SessionSearchCommand),
    Settings(SessionSettingsCommand),
    Title(SessionTitleCommand),
    Interrupt(SessionInterruptCommand),
    Compact(SessionCompactCommand),
    Memory(SessionMemoryCommand),
    Show(SessionShowCommand),
    Unarchive(SessionArchiveCommand),
    History(SessionItemsCommand),
    Read(SessionReadCommand),
    Rejoin(SessionReadCommand),
    Rollback(SessionRollbackCommand),
    Fork(SessionForkCommand),
    Turns(SessionItemsCommand),
    Events(SessionItemsCommand),
    Steer(SessionSteerCommand),
}

#[derive(Subcommand)]
enum ReplayCommand {
    Run(ReplayRunCommand),
    Report(ReplayReportCommand),
}

#[derive(Subcommand)]
enum ModelCommand {
    Availability(ModelAvailabilityCommand),
}

#[derive(Subcommand)]
enum SchemaCommand {
    Export(SchemaExportCommand),
}

#[derive(Subcommand)]
enum ContractCommand {
    Snapshot(ContractSnapshotCommand),
}

#[derive(Args)]
struct SessionListCommand {
    #[arg(long = "dir")]
    directory: Option<Utf8PathBuf>,
    #[arg(long = "limit", default_value_t = 20)]
    limit: usize,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
}

#[derive(Args)]
struct SessionLoadedCommand {
    #[arg(long = "dir")]
    directory: Option<Utf8PathBuf>,
    #[arg(long = "limit", default_value_t = 20)]
    limit: usize,
    #[arg(long = "include-archived")]
    include_archived: bool,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
}

#[derive(Args)]
struct SessionSearchCommand {
    #[arg()]
    query: Vec<String>,
    #[arg(long = "dir")]
    directory: Option<Utf8PathBuf>,
    #[arg(long = "limit", default_value_t = 20)]
    limit: usize,
    #[arg(long = "include-archived")]
    include_archived: bool,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
}

#[derive(Args)]
struct SessionArchiveCommand {
    #[arg()]
    session_id: String,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
}

#[derive(Args)]
struct SessionSettingsCommand {
    #[arg()]
    session_id: String,
    #[arg(long = "cwd")]
    cwd: Option<Utf8PathBuf>,
    #[arg(long = "model")]
    model: Option<String>,
    #[arg(long = "base-url")]
    base_url: Option<String>,
    #[arg(
        long = "access-mode",
        value_parser = ["default", "auto_review", "full_access"]
    )]
    access_mode: Option<String>,
    #[arg(long = "reset-model-parameters")]
    reset_model_parameters: bool,
    #[arg(long = "temperature")]
    temperature: Option<f64>,
    #[arg(long = "top-p")]
    top_p: Option<f64>,
    #[arg(long = "top-k")]
    top_k: Option<u32>,
    #[arg(long = "max-output-tokens")]
    max_output_tokens: Option<u32>,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
}

#[derive(Args)]
struct SessionTitleCommand {
    #[arg()]
    session_id: String,
    #[arg()]
    title: Vec<String>,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
}

#[derive(Args)]
struct SessionInterruptCommand {
    #[arg()]
    session_id: String,
    #[arg()]
    reason: Vec<String>,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
}

#[derive(Args)]
struct SessionCompactCommand {
    #[arg()]
    session_id: String,
    #[arg(long = "keep-recent", default_value_t = 20)]
    keep_recent: usize,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
}

#[derive(Args)]
struct SessionMemoryCommand {
    #[arg()]
    session_id: String,
    #[arg(long = "mode", value_parser = ["enabled", "disabled"])]
    mode: Option<String>,
    #[arg(long = "reset", conflicts_with = "mode")]
    reset: bool,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
}

#[derive(Args)]
struct SessionShowCommand {
    #[arg()]
    session_id: String,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
    #[arg(long = "show-reasoning")]
    show_reasoning: bool,
}

#[derive(Args)]
struct SessionItemsCommand {
    #[arg()]
    session_id: String,
    #[arg(long = "offset", default_value_t = 0)]
    offset: usize,
    #[arg(long = "limit", default_value_t = 100)]
    limit: usize,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
}

#[derive(Args)]
struct SessionReadCommand {
    #[arg()]
    session_id: String,
    #[arg(long = "history-offset", default_value_t = 0)]
    history_offset: usize,
    #[arg(long = "history-limit", default_value_t = 50)]
    history_limit: usize,
    #[arg(long = "turn-offset", default_value_t = 0)]
    turn_offset: usize,
    #[arg(long = "turn-limit", default_value_t = 50)]
    turn_limit: usize,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
}

#[derive(Args)]
struct SessionRollbackCommand {
    #[arg()]
    session_id: String,
    #[arg(long = "turns", default_value_t = 1)]
    num_turns: usize,
    #[arg(long = "history-offset", default_value_t = 0)]
    history_offset: usize,
    #[arg(long = "history-limit", default_value_t = 50)]
    history_limit: usize,
    #[arg(long = "turn-offset", default_value_t = 0)]
    turn_offset: usize,
    #[arg(long = "turn-limit", default_value_t = 50)]
    turn_limit: usize,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
}

#[derive(Args)]
struct SessionForkCommand {
    #[arg()]
    session_id: String,
    #[arg(long = "title")]
    title: Option<String>,
    #[arg(long = "history-offset", default_value_t = 0)]
    history_offset: usize,
    #[arg(long = "history-limit", default_value_t = 50)]
    history_limit: usize,
    #[arg(long = "turn-offset", default_value_t = 0)]
    turn_offset: usize,
    #[arg(long = "turn-limit", default_value_t = 50)]
    turn_limit: usize,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
}

#[derive(Args)]
struct SessionSteerCommand {
    #[arg()]
    session_id: String,
    #[arg()]
    prompt: Vec<String>,
    #[arg(long = "dir")]
    directory: Option<Utf8PathBuf>,
    #[arg(long = "image", value_name = "PATH")]
    image_paths: Vec<Utf8PathBuf>,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
}

#[derive(Args)]
struct ReplayRunCommand {
    #[arg(long = "artifact-root")]
    artifact_root: Utf8PathBuf,
    #[arg(long = "scenario")]
    scenario_id: String,
    #[arg(long = "mode", default_value = "stored-artifact")]
    mode: String,
    #[arg(long = "output")]
    output: Utf8PathBuf,
    #[arg(long = "event-log")]
    event_log: Option<Utf8PathBuf>,
    #[arg(long = "artifact-manifest")]
    artifact_manifest: Option<Utf8PathBuf>,
    #[arg(long = "contract-registry")]
    contract_registry: Option<Utf8PathBuf>,
    #[arg(long = "data-dir")]
    data_dir: Option<Utf8PathBuf>,
}

#[derive(Args)]
struct ReplayReportCommand {
    #[arg(long = "run-id")]
    run_id: String,
    #[arg(long = "data-dir")]
    data_dir: Option<Utf8PathBuf>,
}

#[derive(Args)]
struct ModelAvailabilityCommand {
    #[arg(long = "dir")]
    directory: Option<Utf8PathBuf>,
    #[arg(long = "model")]
    model_override: Option<String>,
    #[arg(long = "base-url")]
    base_url_override: Option<String>,
    #[arg(long = "output")]
    output: Option<Utf8PathBuf>,
    #[arg(long = "require-vision")]
    require_vision: bool,
    #[arg(long = "openai-compatible-only")]
    openai_compatible_only: bool,
}

#[derive(Args)]
struct SchemaExportCommand {
    #[arg(long = "output")]
    output: Utf8PathBuf,
}

#[derive(Args)]
struct ContractSnapshotCommand {
    #[arg(long = "scenario")]
    scenario_id: String,
    #[arg(long = "source")]
    source: Utf8PathBuf,
    #[arg(long = "output")]
    output: Utf8PathBuf,
}

impl clap::ValueEnum for OutputMode {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Human, Self::Json]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(match self {
            Self::Human => clap::builder::PossibleValue::new("human"),
            Self::Json => clap::builder::PossibleValue::new("json"),
        })
    }
}
