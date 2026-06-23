use std::fs;
use std::process::ExitCode;

use camino::{Utf8Path, Utf8PathBuf};
use directories_next::ProjectDirs;

fn main() -> ExitCode {
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
}
