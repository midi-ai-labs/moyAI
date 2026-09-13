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
    /// Authenticate local CLI/TUI/legacy work to the Hub; password is read with console echo off.
    SignIn {
        #[arg(long)]
        runner: Ulid,
        #[arg(long)]
        username: String,
        #[arg(long)]
        project: String,
    },
    SignOut {
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
        Command::SignIn {
            runner,
            username,
            project,
        } => RunnerCommand::Operations {
            runner_id: runner,
            operation: moyai::runner::operations::RunnerOperation::LocalSignIn {
                credentials: moyai::runner::operations::LocalCredentials {
                    username,
                    password: read_password()?,
                    project_id: project,
                },
            },
        },
        Command::SignOut { runner } => RunnerCommand::Operations {
            runner_id: runner,
            operation: moyai::runner::operations::RunnerOperation::LocalSignOut,
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

#[cfg(windows)]
fn read_password() -> Result<String, Box<dyn std::error::Error>> {
    use std::io::{BufRead, Read, Write};
    use windows_sys::Win32::{Foundation::HANDLE, System::Console::*};
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let mut mode = 0;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        return Err("Hub sign-in needs an interactive console for hidden password entry".into());
    }
    struct Restore(HANDLE, u32);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                SetConsoleMode(self.0, self.1);
            }
        }
    }
    let _restore = Restore(handle, mode);
    if unsafe { SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT) } == 0 {
        return Err("Cannot disable password echo".into());
    }
    eprint!("Hub password: ");
    std::io::stderr().flush()?;
    let mut password = String::new();
    std::io::stdin()
        .lock()
        .take(1026)
        .read_line(&mut password)?;
    eprintln!();
    if password.len() > 1024 {
        return Err("Password exceeds its bound".into());
    }
    Ok(password.trim_end_matches(['\r', '\n']).to_owned())
}

#[cfg(not(windows))]
fn run(_: Args) -> Result<(), Box<dyn std::error::Error>> {
    Err("Local Runner IPC currently supports Windows only".into())
}
