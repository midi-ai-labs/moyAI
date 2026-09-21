//! Independent execution host. Private local mode requires neither Desktop nor Hub.
#![cfg_attr(windows, windows_subsystem = "windows")]

use clap::{Parser, Subcommand};
use moyai::runner::{LocalApprovalDecision, LocalRunRequest, RunnerCommand, RunnerResponse};
use ulid::Ulid;

#[derive(Parser)]
#[command(
    name = "moyai-runner",
    about = "Independent moyAI execution host (Windows)"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the host until an explicit shutdown. Client disconnects do not stop execution.
    Serve {
        /// Operator-installed Hub environment mappings. This selects shared-only admission.
        #[arg(long)]
        shared_settings: Option<camino::Utf8PathBuf>,
        /// Remain in the background without attaching a terminal.
        #[arg(long)]
        background: bool,
    },
    /// Read the current Runner incarnation ID before issuing a command.
    Identity,
    SharedStatus {
        #[arg(long)]
        runner: Ulid,
    },
    /// Submit or retry one exact ID within the observed Runner incarnation.
    Run {
        #[arg(long)]
        runner: Ulid,
        #[arg(long)]
        run: Ulid,
        #[arg(long)]
        directory: camino::Utf8PathBuf,
        #[arg(long)]
        session: Option<moyai::session::SessionId>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        single_agent: bool,
        prompt: String,
    },
    Status {
        #[arg(long)]
        runner: Ulid,
        #[arg(long)]
        run: Ulid,
    },
    List {
        #[arg(long)]
        runner: Ulid,
    },
    Stop {
        #[arg(long)]
        runner: Ulid,
        #[arg(long)]
        run: Ulid,
    },
    Approve {
        #[arg(long)]
        runner: Ulid,
        #[arg(long)]
        run: Ulid,
        #[arg(long)]
        approval: Ulid,
        #[arg(value_parser = ["approve", "deny", "stop"])]
        decision: String,
    },
    Shutdown {
        #[arg(long)]
        runner: Ulid,
    },
    /// Generate a new request ID without submitting anything.
    NewId,
    /// Read operational state or submit a typed operator command as JSON.
    Operations {
        #[arg(long)]
        runner: Ulid,
        #[arg(default_value = "{\"operation\":\"status\"}")]
        request: String,
    },
    /// Select an allowed Hub project for local CLI/TUI work on this approved device.
    UseProject {
        #[arg(long)]
        runner: Ulid,
        #[arg(long)]
        project: String,
    },
    /// Clear the selected local Hub project; device enrollment is preserved.
    ClearProject {
        #[arg(long)]
        runner: Ulid,
    },
}

fn main() {
    let args = Args::parse();
    #[cfg(windows)]
    if !matches!(
        args.command,
        Command::Serve {
            background: true,
            ..
        }
    ) {
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(
                windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
            );
        }
    }
    if let Err(error) = run(args) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    use moyai::runner::{RunnerHost, windows};
    let command = match args.command {
        Command::Serve {
            shared_settings, ..
        } => {
            let listener = windows::LocalListener::bind()?;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let host = runtime.block_on(RunnerHost::open())?;
            let installed = match shared_settings {
                Some(path) => Some(moyai::runner::shared::SharedSettings::load(&path)?),
                None => host.installed_shared_settings()?,
            };
            let shared = match installed {
                Some(settings) => Some(runtime.block_on(
                    moyai::runner::shared::SharedWorker::start(host.clone(), settings),
                )?),
                None => None,
            };
            println!(
                "{}",
                serde_json::to_string(&RunnerResponse::Identity {
                    identity: host.identity()
                })?
            );
            let signal_host = host.clone();
            runtime.spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    let _ = signal_host.begin_shutdown();
                }
            });
            let result = listener.serve(host.clone(), &runtime);
            host.begin_shutdown()?;
            runtime.block_on(host.wait_stopped());
            if let Some(shared) = shared {
                runtime.block_on(shared.wait())?;
            }
            result?;
            return Ok(());
        }
        Command::NewId => {
            println!("{}", Ulid::new());
            return Ok(());
        }
        Command::Identity => RunnerCommand::Identity,
        Command::UseProject { runner, project } => RunnerCommand::Operations {
            runner_id: runner,
            operation: moyai::runner::operations::RunnerOperation::LocalProject {
                project_id: Some(project),
            },
        },
        Command::ClearProject { runner } => RunnerCommand::Operations {
            runner_id: runner,
            operation: moyai::runner::operations::RunnerOperation::LocalProject {
                project_id: None,
            },
        },
        Command::SharedStatus { runner } => RunnerCommand::SharedStatus { runner_id: runner },
        Command::Operations { runner, request } => RunnerCommand::Operations {
            runner_id: runner,
            operation: serde_json::from_str(&request)?,
        },
        Command::Run {
            runner,
            run,
            directory,
            session,
            title,
            single_agent,
            prompt,
        } => RunnerCommand::Submit {
            runner_id: runner,
            run_id: run,
            request: LocalRunRequest {
                directory,
                prompt,
                session_id: session,
                title,
                single_agent,
            },
        },
        Command::Status { runner, run } => RunnerCommand::Status {
            runner_id: runner,
            run_id: run,
        },
        Command::List { runner } => RunnerCommand::List { runner_id: runner },
        Command::Stop { runner, run } => RunnerCommand::Stop {
            runner_id: runner,
            run_id: run,
        },
        Command::Approve {
            runner,
            run,
            approval,
            decision,
        } => RunnerCommand::Approve {
            runner_id: runner,
            run_id: run,
            approval_id: approval,
            decision: match decision.as_str() {
                "approve" => LocalApprovalDecision::Approve,
                "deny" => LocalApprovalDecision::Deny,
                _ => LocalApprovalDecision::Stop,
            },
        },
        Command::Shutdown { runner } => RunnerCommand::Shutdown { runner_id: runner },
    };
    println!("{}", serde_json::to_string(&windows::request(&command)?)?);
    Ok(())
}

#[cfg(not(windows))]
fn run(_: Args) -> Result<(), Box<dyn std::error::Error>> {
    Err("Local Runner IPC currently supports Windows only".into())
}
