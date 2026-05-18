use camino::Utf8PathBuf;
use clap::{Args, Parser, Subcommand};

use crate::error::CliUsageError;
use crate::session::SessionId;

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
pub struct SessionShowArgs {
    pub session_id: SessionId,
    pub output_mode: OutputMode,
    pub show_reasoning: bool,
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
pub struct PreflightRunArgs {
    pub output: Option<Utf8PathBuf>,
}

#[derive(Debug, Clone)]
pub struct PreflightArtifactArgs {
    pub artifact_root: Utf8PathBuf,
    pub failure_ids: Vec<String>,
    pub output: Option<Utf8PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ModelAvailabilityArgs {
    pub directory: Option<Utf8PathBuf>,
    pub model_override: Option<String>,
    pub base_url_override: Option<String>,
    pub output: Option<Utf8PathBuf>,
    pub require_vision: bool,
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
pub struct ManualStRouteArgs {
    pub route: String,
    pub output_root: Option<Utf8PathBuf>,
    pub preflight_report: Utf8PathBuf,
    pub model_override: Option<String>,
    pub base_url_override: Option<String>,
    pub max_turn_seconds: u64,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub enum CliCommand {
    Run(RunArgs),
    SessionList(SessionListArgs),
    SessionShow(SessionShowArgs),
    Tui(TuiArgs),
    Desktop(DesktopArgs),
    ReplayRun(ReplayRunArgs),
    ReplayReport(ReplayReportArgs),
    PreflightRun(PreflightRunArgs),
    PreflightArtifact(PreflightArtifactArgs),
    ModelAvailability(ModelAvailabilityArgs),
    SchemaExport(SchemaExportArgs),
    ContractSnapshot(ContractSnapshotArgs),
    ManualStRoute(ManualStRouteArgs),
}

pub fn parse() -> Result<CliCommand, CliUsageError> {
    let cli = RootCli::try_parse().map_err(|error| CliUsageError::Message(error.to_string()))?;
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
            SessionCommand::Show(args) => Ok(CliCommand::SessionShow(SessionShowArgs {
                session_id: args.session_id.parse().map_err(|error| {
                    CliUsageError::Message(format!("invalid session id: {error}"))
                })?,
                output_mode: args.output_mode,
                show_reasoning: args.show_reasoning,
            })),
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
        RootCommand::Preflight { command } => match command {
            PreflightCommand::Run(args) => Ok(CliCommand::PreflightRun(PreflightRunArgs {
                output: args.output,
            })),
            PreflightCommand::Artifact(args) => {
                Ok(CliCommand::PreflightArtifact(PreflightArtifactArgs {
                    artifact_root: args.artifact_root,
                    failure_ids: args.failure_ids,
                    output: args.output,
                }))
            }
        },
        RootCommand::Model { command } => match command {
            ModelCommand::Availability(args) => {
                Ok(CliCommand::ModelAvailability(ModelAvailabilityArgs {
                    directory: args.directory,
                    model_override: args.model_override,
                    base_url_override: args.base_url_override,
                    output: args.output,
                    require_vision: args.require_vision,
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
        RootCommand::ManualSt { command } => match command {
            ManualStCommand::Route(args) => Ok(CliCommand::ManualStRoute(ManualStRouteArgs {
                route: args.route,
                output_root: args.output_root,
                preflight_report: args.preflight_report,
                model_override: args.model_override,
                base_url_override: args.base_url_override,
                max_turn_seconds: args.max_turn_seconds,
                dry_run: args.dry_run,
            })),
        },
    }
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
    Preflight {
        #[command(subcommand)]
        command: PreflightCommand,
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
    ManualSt {
        #[command(subcommand)]
        command: ManualStCommand,
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
    List(SessionListCommand),
    Show(SessionShowCommand),
}

#[derive(Subcommand)]
enum ReplayCommand {
    Run(ReplayRunCommand),
    Report(ReplayReportCommand),
}

#[derive(Subcommand)]
enum PreflightCommand {
    Run(PreflightRunCommand),
    Artifact(PreflightArtifactCommand),
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

#[derive(Subcommand)]
enum ManualStCommand {
    Route(ManualStRouteCommand),
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
struct SessionShowCommand {
    #[arg()]
    session_id: String,
    #[arg(long = "format", value_enum, default_value_t = OutputMode::Human)]
    output_mode: OutputMode,
    #[arg(long = "show-reasoning")]
    show_reasoning: bool,
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
struct PreflightRunCommand {
    #[arg(long = "output")]
    output: Option<Utf8PathBuf>,
}

#[derive(Args)]
struct PreflightArtifactCommand {
    #[arg(long = "artifact-root")]
    artifact_root: Utf8PathBuf,
    #[arg(long = "failure-id")]
    failure_ids: Vec<String>,
    #[arg(long = "output")]
    output: Option<Utf8PathBuf>,
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

#[derive(Args)]
struct ManualStRouteCommand {
    #[arg(long = "route")]
    route: String,
    #[arg(long = "output-root")]
    output_root: Option<Utf8PathBuf>,
    #[arg(long = "preflight-report")]
    preflight_report: Utf8PathBuf,
    #[arg(long = "model")]
    model_override: Option<String>,
    #[arg(long = "base-url")]
    base_url_override: Option<String>,
    #[arg(long = "max-turn-seconds", default_value_t = 7200)]
    max_turn_seconds: u64,
    #[arg(long = "dry-run")]
    dry_run: bool,
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
