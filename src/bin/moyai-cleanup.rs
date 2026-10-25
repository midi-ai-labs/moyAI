use std::fs;
use std::process::ExitCode;

use camino::{Utf8Path, Utf8PathBuf};
use directories_next::ProjectDirs;

fn main() -> ExitCode {
    match cleanup_command(&std::env::args().skip(1).collect::<Vec<_>>()) {
        #[cfg(windows)]
        Ok(CleanupCommand::Interactive) => return run_interactive(),
        Ok(CleanupCommand::Help) => {
            println!("{}", cleanup_help());
            return ExitCode::SUCCESS;
        }
        Ok(CleanupCommand::Version) => {
            println!("moyai-cleanup {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(CleanupCommand::Preview) => {
            return match cleanup_targets() {
                Ok(targets) => {
                    println!("{}", cleanup_preview(&targets));
                    ExitCode::SUCCESS
                }
                Err(message) => {
                    eprintln!("{message}");
                    ExitCode::from(1)
                }
            };
        }
        Ok(CleanupCommand::Clean) => {}
        Err(message) => {
            eprintln!("{message}");
            eprintln!("{}", cleanup_help());
            return ExitCode::from(2);
        }
    }

    match run() {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(1)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupCommand {
    #[cfg(windows)]
    Interactive,
    Preview,
    Clean,
    Help,
    Version,
}

fn cleanup_command(args: &[String]) -> Result<CleanupCommand, String> {
    if args.is_empty() {
        #[cfg(windows)]
        return Ok(CleanupCommand::Interactive);
        #[cfg(not(windows))]
        return Ok(CleanupCommand::Preview);
    }
    if args == ["--reset-user-data", "--confirm-delete"] {
        return Ok(CleanupCommand::Clean);
    }
    if args == ["--preview"] {
        return Ok(CleanupCommand::Preview);
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(CleanupCommand::Help);
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        return Ok(CleanupCommand::Version);
    }
    Err(format!("unexpected argument `{}`", args[0]))
}

fn cleanup_help() -> &'static str {
    "Usage: moyai-cleanup\n       moyai-cleanup --preview\n       moyai-cleanup --reset-user-data --confirm-delete\n\nWithout arguments on Windows, asks for confirmation in a Japanese dialog before resetting data.\nOn other platforms, shows the affected AppData directories without deleting files.\nReset permanently deletes config, device and human credentials, history, and cached results.\nClose Desktop and Runner and back up needed data before an explicit reset.\nNormal updates and uninstall do not require a reset. Hub data and project folders are not reset.\n\nOptions:\n  --preview                         Show targets without changing files\n  --reset-user-data --confirm-delete Permanently delete the displayed AppData targets\n  -h, --help                        Print help\n  -V, --version                     Print version"
}

fn cleanup_preview(targets: &[Utf8PathBuf]) -> String {
    format!(
        "{}\n\nTargets (no files changed):\n{}",
        cleanup_help(),
        targets
            .iter()
            .map(|path| format!(
                "  {path} [{}]",
                if path.exists() { "present" } else { "absent" }
            ))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn run() -> Result<String, String> {
    let targets = cleanup_targets()?;
    let report = remove_user_data(&targets)?;
    Ok(cleanup_report(&report.removed, &report.missing))
}

struct CleanupReport {
    removed: Vec<Utf8PathBuf>,
    missing: Vec<Utf8PathBuf>,
}

fn remove_user_data(targets: &[Utf8PathBuf]) -> Result<CleanupReport, String> {
    for target in targets {
        assert_cleanup_target(target)?;
    }
    let _reset = moyai::runner::operations::prepare_user_data_reset(targets)
        .map_err(|error| error.message)?;
    remove_checked_targets(targets)
}

fn remove_checked_targets(targets: &[Utf8PathBuf]) -> Result<CleanupReport, String> {
    let mut removed = Vec::new();
    let mut missing = Vec::new();
    for target in targets {
        if !target.exists() {
            missing.push(target.clone());
            continue;
        }
        fs::remove_dir_all(target.as_std_path()).map_err(|error| {
            format!("failed to remove {target}: {error}\nClose all moyAI windows and try again.")
        })?;
        removed.push(target.clone());
    }
    Ok(CleanupReport { removed, missing })
}

#[cfg(windows)]
enum InteractiveOutcome {
    Cancelled,
    Completed(CleanupReport),
}

#[cfg(windows)]
fn interactive_cleanup(
    targets: &[Utf8PathBuf],
    confirm: impl FnOnce(&str) -> Result<bool, String>,
    remove: impl FnOnce(&[Utf8PathBuf]) -> Result<CleanupReport, String>,
) -> Result<InteractiveOutcome, String> {
    if !confirm(&confirmation_text(targets))? {
        return Ok(InteractiveOutcome::Cancelled);
    }
    remove(targets).map(InteractiveOutcome::Completed)
}

#[cfg(windows)]
fn confirmation_text(targets: &[Utf8PathBuf]) -> String {
    format!(
        "このWindowsユーザーのmoyAIの設定、Hub接続情報、会話履歴、保存した結果を完全に削除します。\n削除したデータは元に戻せません。\nこの利用者の共有実行登録も、停止を確認したうえで解除します。\n\n必要なデータをバックアップし、すべてのmoyAI Desktop・CLI・Runnerを終了してから進めてください。\nHub本体のデータとプロジェクトの作業フォルダーは削除しません。\n\n対象の保存先:\n{}\n\n保存データを初期化しますか？",
        display_targets(targets)
    )
}

#[cfg(windows)]
fn display_targets(targets: &[Utf8PathBuf]) -> String {
    targets
        .iter()
        .map(|path| format!("  {path}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(windows)]
fn interactive_result(result: &Result<InteractiveOutcome, String>) -> (String, bool) {
    match result {
        Ok(InteractiveOutcome::Cancelled) => (
            "初期化をキャンセルしました。保存データは削除していません。".into(),
            false,
        ),
        Ok(InteractiveOutcome::Completed(report)) if report.removed.is_empty() => (
            format!(
                "初期化が完了しました。\n対象の保存先に削除するデータはありませんでした。\n\n確認した保存先:\n{}",
                display_targets(&report.missing)
            ),
            false,
        ),
        Ok(InteractiveOutcome::Completed(report)) => {
            let mut message = format!(
                "初期化が完了しました。\n\n削除した保存先:\n{}",
                display_targets(&report.removed)
            );
            if !report.missing.is_empty() {
                message.push_str(&format!(
                    "\n\n見つからなかった保存先（削除なし）:\n{}",
                    display_targets(&report.missing)
                ));
            }
            (message, false)
        }
        Err(error) => (
            format!(
                "初期化を完了できませんでした。\n\n{error}\n\n保存先を確認し、すべてのmoyAI Desktop・CLI・Runnerを終了してからやり直してください。"
            ),
            true,
        ),
    }
}

#[cfg(windows)]
fn run_interactive() -> ExitCode {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK,
        MB_SETFOREGROUND, MB_YESNO,
    };
    let result = cleanup_targets().and_then(|targets| {
        interactive_cleanup(
            &targets,
            |message| {
                message_box(
                    "moyAI 保存データの初期化",
                    message,
                    MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND,
                )
                .map(|answer| answer == IDYES)
            },
            remove_user_data,
        )
    });
    let (message, failed) = interactive_result(&result);
    if let Err(error) = message_box(
        "moyAI 初期化の結果",
        &message,
        MB_OK
            | MB_SETFOREGROUND
            | if failed {
                MB_ICONERROR
            } else {
                MB_ICONINFORMATION
            },
    ) {
        eprintln!("{error}\n{message}");
        return ExitCode::from(1);
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(windows)]
fn message_box(title: &str, message: &str, style: u32) -> Result<i32, String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW;
    let title = title.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let message = message.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let answer = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            style,
        )
    };
    if answer == 0 {
        Err(format!(
            "確認画面を表示できませんでした: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(answer)
    }
}

fn cleanup_targets() -> Result<Vec<Utf8PathBuf>, String> {
    let dirs = ProjectDirs::from("net", "midi-ai-labs", "moyai")
        .ok_or_else(|| "failed to resolve moyAI AppData directory".to_string())?;
    let candidates = [dirs.config_dir(), dirs.data_dir(), dirs.cache_dir()];
    let mut targets = Vec::new();
    for candidate in candidates {
        let path = Utf8PathBuf::from_path_buf(candidate.to_path_buf())
            .map_err(|_| "moyAI AppData directory is not valid UTF-8".to_string())?;
        let root = moyai_appdata_root(&path)
            .ok_or_else(|| format!("failed to resolve moyAI AppData root from {path}"))?;
        if !targets.iter().any(|existing| existing == &root) {
            targets.push(root);
        }
    }
    Ok(targets)
}

fn cleanup_report(removed: &[Utf8PathBuf], missing: &[Utf8PathBuf]) -> String {
    if removed.is_empty() {
        return format!(
            "moyAI cleanup complete: nothing to remove at {}",
            missing
                .iter()
                .map(|path| path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    format!(
        "moyAI cleanup complete: removed {}",
        removed
            .iter()
            .map(|path| path.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn assert_cleanup_target(path: &Utf8Path) -> Result<(), String> {
    let normalized = path.as_str().replace('/', "\\").to_ascii_lowercase();
    if normalized.ends_with("\\midi-ai-labs\\moyai") {
        return Ok(());
    }
    Err(format!(
        "refusing to clean unexpected path `{path}`; expected a midi-ai-labs\\moyai AppData directory"
    ))
}

fn moyai_appdata_root(path: &Utf8Path) -> Option<Utf8PathBuf> {
    path.ancestors()
        .find(|ancestor| {
            ancestor
                .as_str()
                .replace('/', "\\")
                .to_ascii_lowercase()
                .ends_with("\\midi-ai-labs\\moyai")
        })
        .map(Utf8Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_expected_moyai_appdata_leaf() {
        assert!(
            assert_cleanup_target(Utf8Path::new(
                "C:/Users/example/AppData/Roaming/midi-ai-labs/moyai"
            ))
            .is_ok()
        );
    }

    #[test]
    fn rejects_parent_or_unrelated_paths() {
        assert!(assert_cleanup_target(Utf8Path::new("C:/Users/example/AppData/Roaming")).is_err());
        assert!(
            assert_cleanup_target(Utf8Path::new(
                "C:/Users/example/AppData/Roaming/midi-ai-labs"
            ))
            .is_err()
        );
        assert!(
            assert_cleanup_target(Utf8Path::new(
                "C:/Users/example/AppData/Roaming/other/moyai"
            ))
            .is_err()
        );
    }

    #[test]
    fn resolves_root_from_config_subdirectory() {
        assert_eq!(
            moyai_appdata_root(Utf8Path::new(
                "C:/Users/example/AppData/Roaming/midi-ai-labs/moyai/config"
            ))
            .as_deref(),
            Some(Utf8Path::new(
                "C:/Users/example/AppData/Roaming/midi-ai-labs/moyai"
            ))
        );
    }

    #[test]
    fn report_mentions_removed_targets() {
        let removed = vec![Utf8PathBuf::from(
            "C:/Users/example/AppData/Roaming/midi-ai-labs/moyai",
        )];
        let report = cleanup_report(&removed, &[]);
        assert!(report.contains("removed"));
        assert!(report.contains("midi-ai-labs/moyai"));
    }

    #[test]
    fn removing_checked_targets_deletes_user_history_and_preserves_other_data() {
        let fixture_root = Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("project_sandbox/cleanup-bin-tests");
        fs::create_dir_all(&fixture_root).unwrap();
        let temporary = tempfile::tempdir_in(&fixture_root).unwrap();
        let fixture = Utf8Path::from_path(temporary.path()).unwrap();
        let saved = fixture.join("Roaming/midi-ai-labs/moyai");
        let missing = fixture.join("Local/midi-ai-labs/moyai");
        let hub = fixture.join("Hub");
        let workspace = fixture.join("workspace");
        for directory in [
            saved.join("config"),
            saved.join("data"),
            hub.clone(),
            workspace.clone(),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        fs::write(saved.join("config/config.toml"), "saved config").unwrap();
        fs::write(saved.join("data/moyai.sqlite3"), "saved history").unwrap();
        fs::write(hub.join("keep.txt"), "Hub data").unwrap();
        fs::write(workspace.join("keep.txt"), "project data").unwrap();
        let targets = [saved.clone(), missing.clone()];
        for target in &targets {
            assert_cleanup_target(target).unwrap();
        }

        let report = remove_checked_targets(&targets).unwrap();

        assert_eq!(report.removed, [saved.clone()]);
        assert_eq!(report.missing, [missing]);
        assert!(!saved.exists());
        assert_eq!(
            fs::read_to_string(hub.join("keep.txt")).unwrap(),
            "Hub data"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("keep.txt")).unwrap(),
            "project data"
        );
    }

    #[test]
    fn help_and_version_args_do_not_clean() {
        assert_eq!(
            cleanup_command(&["--help".to_string()]),
            Ok(CleanupCommand::Help)
        );
        assert_eq!(
            cleanup_command(&["-V".to_string()]),
            Ok(CleanupCommand::Version)
        );
    }

    #[test]
    fn reset_requires_both_explicit_arguments() {
        #[cfg(windows)]
        assert_eq!(cleanup_command(&[]), Ok(CleanupCommand::Interactive));
        #[cfg(not(windows))]
        assert_eq!(cleanup_command(&[]), Ok(CleanupCommand::Preview));
        assert_eq!(
            cleanup_command(&["--preview".into()]),
            Ok(CleanupCommand::Preview)
        );
        assert!(cleanup_command(&["--reset-user-data".into()]).is_err());
        assert!(cleanup_command(&["--confirm-delete".into()]).is_err());
        assert_eq!(
            cleanup_command(&["--reset-user-data".into(), "--confirm-delete".into()]),
            Ok(CleanupCommand::Clean)
        );
        assert!(
            cleanup_command(&[
                "--reset-user-data".into(),
                "--confirm-delete".into(),
                "extra".into()
            ])
            .is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn cancelling_interactive_reset_never_enters_the_removal_path() {
        let targets = vec![Utf8PathBuf::from(
            "C:/Users/example/AppData/Roaming/midi-ai-labs/moyai",
        )];
        let result = interactive_cleanup(
            &targets,
            |message| {
                assert!(message.contains(targets[0].as_str()));
                assert!(message.contains("会話履歴"));
                assert!(message.contains("元に戻せません"));
                Ok(false)
            },
            |_| panic!("Cancellation must not acquire reset authority or delete files"),
        );
        assert!(matches!(result, Ok(InteractiveOutcome::Cancelled)));
        let (message, failed) = interactive_result(&result);
        assert!(!failed);
        assert!(message.contains("削除していません"));
    }

    #[cfg(windows)]
    #[test]
    fn confirmation_failure_never_enters_the_removal_path() {
        let result = interactive_cleanup(
            &[],
            |_| Err("確認画面を表示できませんでした".into()),
            |_| panic!("A missing confirmation must not authorize deletion"),
        );
        assert!(result.is_err());
        assert!(interactive_result(&result).1);
    }

    #[cfg(windows)]
    #[test]
    fn confirmed_interactive_reset_uses_the_reviewed_targets_and_reports_failure() {
        let targets = vec![Utf8PathBuf::from(
            "C:/Users/example/AppData/Roaming/midi-ai-labs/moyai",
        )];
        let result = interactive_cleanup(
            &targets,
            |_| Ok(true),
            |selected| {
                assert_eq!(selected, targets);
                Err("共有実行の停止を確認できません".into())
            },
        );
        let (message, failed) = interactive_result(&result);
        assert!(failed);
        assert!(message.contains("初期化を完了できませんでした"));
        assert!(message.contains("共有実行の停止を確認できません"));
    }

    #[test]
    fn preview_preserves_existing_data() {
        let directory = tempfile::tempdir().unwrap();
        let saved = directory.path().join("saved-history.txt");
        fs::write(&saved, "keep me").unwrap();
        let target = Utf8PathBuf::from_path_buf(directory.path().to_path_buf()).unwrap();
        let preview = cleanup_preview(&[target]);
        assert!(preview.contains("no files changed"));
        assert_eq!(fs::read_to_string(saved).unwrap(), "keep me");
    }

    #[test]
    fn unknown_args_fail_before_cleanup() {
        assert_eq!(
            cleanup_command(&["--not-a-real-option".to_string()]),
            Err("unexpected argument `--not-a-real-option`".to_string())
        );
    }
}
