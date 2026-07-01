use std::fs;
use std::process::ExitCode;

use camino::{Utf8Path, Utf8PathBuf};
use directories_next::ProjectDirs;

fn main() -> ExitCode {
    match cleanup_command(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(CleanupCommand::Help) => {
            println!("{}", cleanup_help());
            return ExitCode::SUCCESS;
        }
        Ok(CleanupCommand::Version) => {
            println!("moyai-cleanup {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
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
    Clean,
    Help,
    Version,
}

fn cleanup_command(args: &[String]) -> Result<CleanupCommand, String> {
    if args.is_empty() {
        return Ok(CleanupCommand::Clean);
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
    "Usage: moyai-cleanup\n\nRemoves moyAI AppData config, data, and cache directories.\n\nOptions:\n  -h, --help     Print help\n  -V, --version  Print version"
}

fn run() -> Result<String, String> {
    let targets = cleanup_targets()?;
    let mut removed = Vec::new();
    let mut missing = Vec::new();
    for target in targets {
        assert_cleanup_target(&target)?;
        if !target.exists() {
            missing.push(target);
            continue;
        }
        fs::remove_dir_all(target.as_std_path()).map_err(|error| {
            format!("failed to remove {target}: {error}\nClose all moyAI windows and try again.")
        })?;
        removed.push(target);
    }
    Ok(cleanup_report(&removed, &missing))
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
    fn unknown_args_fail_before_cleanup() {
        assert_eq!(
            cleanup_command(&["--not-a-real-option".to_string()]),
            Err("unexpected argument `--not-a-real-option`".to_string())
        );
    }
}
