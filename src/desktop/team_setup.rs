//! The optional, versioned Hub shipped in the same installation.
//! No PATH search, download, profile relocation, or transfer of human credentials.

use std::path::{Path, PathBuf};

fn bundled_hub(desktop: &Path) -> Result<PathBuf, String> {
    let bin = desktop
        .parent()
        .ok_or("Desktop installation path is unavailable")?;
    let app = bin
        .parent()
        .ok_or("Desktop installation path is unavailable")?;
    if bin.file_name().is_none_or(|name| name != "bin")
        || app.file_name().is_none_or(|name| name != "app")
    {
        return Err("チーム管理を含むmoyAI配布物をセットアップしてください。既存のHubは管理PCから起動できます。".into());
    }
    let root = app
        .parent()
        .ok_or("Desktop installation path is unavailable")?;
    let hub = root.join("hub/bin/moyai-hub.exe");
    if !hub.is_file() {
        return Err("この配布物にチーム管理用Hubがありません。Hub同梱版をセットアップするか、既存の管理PCでHubを起動してください。保存済みデータの初期化は不要です。".into());
    }
    Ok(hub)
}

pub fn launch_team_setup() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let desktop = std::env::current_exe().map_err(|error| error.to_string())?;
        let hub = bundled_hub(&desktop)?;
        let output = std::process::Command::new(&hub)
            .arg("--launch")
            .current_dir(hub.parent().expect("bundled executable parent"))
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW; Hub opens its authenticated browser.
            .output()
            .map_err(|error| format!("チーム管理を起動できませんでした: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "チーム管理を起動できませんでした。既存Hubの状態と導入先を確認してください。\n{}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        Err("このチーム管理の起動入口はWindows配布物で利用できます。".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_only_the_bundled_hub_and_reports_missing_component() {
        let root = tempfile::tempdir().unwrap();
        let desktop = root.path().join("app/bin/moyai-desktop.exe");
        assert!(bundled_hub(&desktop).unwrap_err().contains("同梱版"));
        let hub = root.path().join("hub/bin/moyai-hub.exe");
        std::fs::create_dir_all(hub.parent().unwrap()).unwrap();
        std::fs::write(&hub, b"fixture").unwrap();
        assert_eq!(bundled_hub(&desktop).unwrap(), hub);
        assert!(bundled_hub(&root.path().join("target/debug/moyai-desktop.exe")).is_err());
    }
}
