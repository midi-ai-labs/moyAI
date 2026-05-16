#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::process::ExitCode;

use camino::Utf8PathBuf;
use clap::Parser;
use moyai::app::AppBootstrap;
use moyai::cli::CliCommand;
use moyai::cli::parse::DesktopArgs as CliDesktopArgs;
use moyai::desktop::{self, DesktopArgs};

#[cfg(not(feature = "tauri-desktop"))]
const WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "moyai-desktop",
    about = "Launch the moyAI desktop application."
)]
struct DesktopLauncherArgs {
    #[arg(long = "dir")]
    directory: Option<Utf8PathBuf>,
    #[arg(long = "session")]
    session_id: Option<String>,
    #[arg(long)]
    continue_last: bool,
}

fn main() -> ExitCode {
    match run_desktop_launcher() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("{message}");
            ExitCode::from(code)
        }
    }
}

fn run_desktop_launcher() -> Result<(), (u8, String)> {
    #[cfg(feature = "tauri-desktop")]
    {
        run_on_current_thread()
    }
    #[cfg(not(feature = "tauri-desktop"))]
    {
        run_with_large_stack()
    }
}

#[cfg(not(feature = "tauri-desktop"))]
fn run_with_large_stack() -> Result<(), (u8, String)> {
    let join_handle = std::thread::Builder::new()
        .name("moyai-desktop-worker".to_string())
        .stack_size(WORKER_STACK_BYTES)
        .spawn(run_on_current_thread)
        .map_err(|error| (4, format!("failed to spawn desktop worker thread: {error}")))?;
    match join_handle.join() {
        Ok(result) => result,
        Err(_) => Err((4, "desktop worker thread panicked".to_string())),
    }
}

fn run_on_current_thread() -> Result<(), (u8, String)> {
    let args = DesktopLauncherArgs::parse();
    if args.session_id.is_some() && args.continue_last {
        return Err((
            2,
            "`--session` and `--continue-last` cannot be used together".to_string(),
        ));
    }
    let session_id = args
        .session_id
        .as_deref()
        .map(str::parse)
        .transpose()
        .map_err(|error| (2, format!("invalid session id: {error}")))?;
    let command = CliCommand::Desktop(CliDesktopArgs {
        directory: args.directory.clone(),
        session_id,
        continue_last: args.continue_last,
    });
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
            DesktopArgs {
                directory: args.directory,
                session_id,
                continue_last: args.continue_last,
                global_config_existed_at_launch,
            },
        )
        .await
        .map_err(|error| (4, error.to_string()))
    })
}
