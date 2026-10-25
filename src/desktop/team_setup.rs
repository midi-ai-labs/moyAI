//! Launch the independently installed Hub, or the Hub in an unpacked package.
//! No PATH search, download, or transfer of human credentials.

use std::path::{Path, PathBuf};

fn regular_installation_file(path: &Path) -> Result<(), String> {
    let path = camino::Utf8Path::from_path(path).ok_or("Hubの配置先がUTF-8ではありません。")?;
    crate::user_data::assert_regular_tree(path).map_err(|error| {
        format!("Hubの配置先を確認できません。通常のフォルダーへ導入してください。\n{error}")
    })
}

fn resolve_hub(desktop: &Path, installed_root: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(root) = installed_root.filter(|root| root.exists()) {
        use sha2::{Digest, Sha256};
        let receipt = root.join("machine-component.json");
        regular_installation_file(&receipt)?;
        let file = std::fs::File::open(&receipt).map_err(
            |_| "Hubの導入記録を確認できません。Setup-Hub.cmdで導入・更新してください。",
        )?;
        if file.metadata().map_err(|error| error.to_string())?.len() > 1024 * 1024 {
            return Err("Hubの導入記録が不正です。Setup-Hub.cmdで導入・更新してください。".into());
        }
        let receipt: serde_json::Value = serde_json::from_reader(file).map_err(
            |_| "Hubの導入記録を読み取れません。Setup-Hub.cmdで導入・更新してください。",
        )?;
        if receipt["schema"] != 1
            || receipt["product"] != "moyai-machine-component"
            || receipt["component"] != "Hub"
            || receipt["install_root"].as_str().is_none_or(|path| {
                !path
                    .replace('/', "\\")
                    .eq_ignore_ascii_case(&root.to_string_lossy().replace('/', "\\"))
            })
        {
            return Err(
                "Hubの導入記録と配置先が一致しません。Setup-Hub.cmdで導入・更新してください。"
                    .into(),
            );
        }
        let entries = receipt["files"]
            .as_array()
            .ok_or("Hubの導入記録にファイル一覧がありません。")?;
        let executables = entries
            .iter()
            .filter(|entry| entry["path"] == "moyai-hub.exe")
            .collect::<Vec<_>>();
        if executables.len() != 1 {
            return Err(
                "Hub本体の導入記録が不正です。Setup-Hub.cmdで導入・更新してください。".into(),
            );
        }
        let hub = root.join("moyai-hub.exe");
        regular_installation_file(&hub)?;
        let mut image =
            std::fs::File::open(&hub).map_err(|error| format!("Hub本体を開けません: {error}"))?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut image, &mut hasher).map_err(|error| error.to_string())?;
        if executables[0]["sha256"].as_str() != Some(format!("{:x}", hasher.finalize()).as_str()) {
            return Err(
                "Hub本体が導入記録と一致しません。Setup-Hub.cmdで導入・更新してください。".into(),
            );
        }
        return Ok(hub);
    }
    bundled_hub(desktop)
}

#[cfg(windows)]
fn installed_hub_root() -> Result<PathBuf, String> {
    use windows_sys::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath},
    };
    let mut value = std::ptr::null_mut();
    let status = unsafe {
        SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, std::ptr::null_mut(), &mut value)
    };
    if status < 0 || value.is_null() {
        return Err("Program Filesの場所を確認できません。".into());
    }
    let mut length = 0;
    while unsafe { *value.add(length) } != 0 {
        length += 1;
    }
    let path = String::from_utf16(unsafe { std::slice::from_raw_parts(value, length) });
    unsafe {
        CoTaskMemFree(value.cast());
    }
    Ok(PathBuf::from(path.map_err(|error| error.to_string())?).join("moyAI/Hub"))
}

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
    regular_installation_file(&hub)?;
    Ok(hub)
}

pub fn launch_team_setup() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let desktop = std::env::current_exe().map_err(|error| error.to_string())?;
        let hub = resolve_hub(&desktop, Some(&installed_hub_root()?))?;
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

    #[test]
    fn desktop_can_launch_a_separately_installed_hub_and_reject_changed_payload() {
        use sha2::{Digest, Sha256};
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("Hub");
        std::fs::create_dir(&root).unwrap();
        let image = root.join("moyai-hub.exe");
        std::fs::write(&image, b"owned Hub fixture").unwrap();
        let receipt = serde_json::json!({
            "schema":1,"product":"moyai-machine-component","component":"Hub",
            "install_root":root,"files":[{"path":"moyai-hub.exe","sha256":format!("{:x}",Sha256::digest(b"owned Hub fixture"))}]
        });
        std::fs::write(
            root.join("machine-component.json"),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        let desktop = fixture.path().join("Desktop/app/bin/moyai-desktop.exe");
        assert_eq!(resolve_hub(&desktop, Some(&root)).unwrap(), image);
        std::fs::write(&image, b"changed Hub fixture").unwrap();
        assert!(
            resolve_hub(&desktop, Some(&root))
                .unwrap_err()
                .contains("一致しません")
        );
        std::fs::remove_file(root.join("machine-component.json")).unwrap();
        assert!(
            resolve_hub(&desktop, Some(&root))
                .unwrap_err()
                .contains("導入記録")
        );
    }
}
