#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::process::ExitCode;
use std::sync::Arc;

use camino::Utf8PathBuf;
use clap::Parser;
use moyai::app::AppBootstrap;
use moyai::cli::CliCommand;
use moyai::cli::parse::DesktopArgs as CliDesktopArgs;
use moyai::desktop::startup_report::StartupReport;
use moyai::desktop::{self, DesktopArgs};

#[derive(Debug, Parser)]
#[command(
    name = "moyai-desktop",
    version,
    about = "Launch the moyAI desktop application."
)]
struct DesktopLauncherArgs {
    #[arg(long = "dir")]
    directory: Option<Utf8PathBuf>,
    #[arg(long = "session")]
    session_id: Option<String>,
    #[arg(long)]
    continue_last: bool,
    /// Review a public Hub connection file before requesting device admission.
    #[arg(long = "join-config", value_name = "FILE")]
    join_config: Option<Utf8PathBuf>,
    /// Report the initial screen result to a new setup-owned JSON file.
    #[arg(long = "startup-report", value_name = "FILE")]
    startup_report: Option<Utf8PathBuf>,
}

fn main() -> ExitCode {
    let args = DesktopLauncherArgs::parse();
    let report = match args.startup_report.as_ref() {
        Some(path) => match StartupReport::create(path.as_std_path()) {
            Ok(report) => Arc::new(report),
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(4);
            }
        },
        None => Arc::new(StartupReport::default()),
    };
    if args.startup_report.is_some() {
        let panic_report = report.clone();
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ =
                panic_report.error(101, &format!("moyAIの起動中にエラーが発生しました: {info}"));
            previous_hook(info);
        }));
    }
    match run_desktop_launcher(args, report.clone()) {
        Ok(()) => match report.error(4, "初期画面の表示を確認する前にmoyAIが終了しました。")
        {
            Ok(true) => ExitCode::from(4),
            Ok(false) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(4)
            }
        },
        Err((code, message)) => {
            if let Err(error) = report.error(code, &message) {
                eprintln!("{error}");
            }
            eprintln!("{message}");
            ExitCode::from(code)
        }
    }
}

fn run_desktop_launcher(
    args: DesktopLauncherArgs,
    report: Arc<StartupReport>,
) -> Result<(), (u8, String)> {
    run_on_current_thread(args, report)
}

fn run_on_current_thread(
    args: DesktopLauncherArgs,
    report: Arc<StartupReport>,
) -> Result<(), (u8, String)> {
    if let Some(path) = &args.join_config {
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|path| Utf8PathBuf::from_path_buf(path).ok())
            .ok_or_else(|| (2, "接続ファイルを開く場所を確認できません。".into()))?;
        desktop::join_config::resolve_launch_path(path.as_str(), &cwd)
            .map_err(|error| (2, error))?;
    }
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
    let Some(_desktop_instance) =
        desktop::DesktopInstanceGuard::acquire_or_notify().map_err(|error| (4, error))?
    else {
        println!("moyAI Desktop is already running; showing the existing window.");
        return Ok(());
    };
    let _sandbox_broker = moyai::sandbox_broker::start().map_err(|error| {
        if args.startup_report.is_none() {
            let _ = rfd::MessageDialog::new()
                .set_title("moyAI")
                .set_description(&error)
                .set_level(rfd::MessageLevel::Error)
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
        }
        (4, error)
    })?;
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
        desktop::run_with_startup_report(
            app,
            DesktopArgs {
                directory: args.directory,
                session_id,
                continue_last: args.continue_last,
                global_config_existed_at_launch,
            },
            report,
        )
        .await
        .map_err(|error| (4, error.to_string()))
    })
}
