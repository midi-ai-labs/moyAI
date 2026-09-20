//! File activation carries a public Hub configuration to a native review.
//! It does not grant device admission, human identity, model use, or execution.

use std::io::Read;

use camino::{Utf8Path, Utf8PathBuf};
use sha2::{Digest, Sha256};

use crate::device_network::SharedHubConfig;

const MAX_JOIN_FILE_BYTES: u64 = 256 * 1024;
pub(crate) const ENDPOINT_NOT_SAVED: &str = "接続先は変更していません。実行受付は自動再開しません。最新の設定を確認して再試行するか、元の接続で受付を明示的に再開してください。";

pub(crate) fn configuration_error(error: crate::device_network::DeviceError) -> &'static str {
    use crate::device_network::DeviceError;
    match error {
        DeviceError::DifferentHub => {
            "同じHubであることを確認できません。別Hubや公開CAの変更には対応していません。既存の登録・接続設定は保持しています。"
        }
        DeviceError::EndpointChangeBusy => {
            "実行受付を一時停止し、実行中・状態不明の仕事がなくなるまで待ってから接続先を変更してください。既存の接続設定は保持しています。"
        }
        DeviceError::EndpointChangeRunnerUnconfirmed => {
            "実行機能の安全な終了を確認できません。接続設定は変更していません。実行中・状態不明の仕事と停止状況を確認してください。"
        }
        _ => {
            "接続先の確認に失敗しました。Hubの稼働状況と接続ファイルを確認してください。既存の登録・接続設定は保持しています。"
        }
    }
}

pub fn resolve_launch_path(path: &str, cwd: &Utf8Path) -> Result<Utf8PathBuf, String> {
    if path.is_empty() || path.len() > 32_767 || path.contains('\0') || !cwd.is_absolute() {
        return Err("接続ファイルのパスを確認してください。".into());
    }
    let path = Utf8Path::new(path);
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    })
}

pub(crate) fn from_launch_args(args: &[String], cwd: &str) -> Result<Option<Utf8PathBuf>, String> {
    let mut selected = None;
    let mut args = args.iter().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--" {
            break;
        }
        let path = if argument == "--join-config" {
            Some(
                args.next()
                    .ok_or("--join-config には接続ファイルを指定してください。")?
                    .as_str(),
            )
        } else {
            argument.strip_prefix("--join-config=")
        };
        if let Some(path) = path {
            if selected.is_some() {
                return Err("一度に開ける接続ファイルは一つです。".into());
            }
            selected = Some(resolve_launch_path(path, Utf8Path::new(cwd))?);
        }
    }
    Ok(selected)
}

pub(crate) fn load(path: &Utf8Path) -> Result<SharedHubConfig, String> {
    let file = std::fs::File::open(path)
        .map_err(|_| "接続ファイルを開けません。配布元と保存場所を確認してください。")?;
    let metadata = file
        .metadata()
        .map_err(|_| "接続ファイルを確認できません。")?;
    if !metadata.is_file() || metadata.len() > MAX_JOIN_FILE_BYTES {
        return Err("接続ファイルは256 KiB以内の通常のファイルを指定してください。".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_JOIN_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "接続ファイルを読み取れません。")?;
    if bytes.len() as u64 > MAX_JOIN_FILE_BYTES {
        return Err("接続ファイルが大きすぎます。".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "接続ファイルはUTF-8で保存してください。")?;
    SharedHubConfig::import(&text)
        .map_err(|_| "有効なHub接続情報ではありません。公開URLと公開CAだけを含む接続情報を管理者から受け取ってください。".into())
}

pub(crate) fn review_text(
    shared: &SharedHubConfig,
    current: &SharedHubConfig,
    registered: bool,
) -> Result<String, String> {
    if registered
        && !current
            .same_ca(shared)
            .map_err(|_| "公開CAを確認できません。")?
    {
        return Err(configuration_error(crate::device_network::DeviceError::DifferentHub).into());
    }
    let certificate = crate::mcp_publish::tls::public_certificate(&shared.ca_certificate_pem)
        .map_err(|_| "接続先の公開CAを確認できません。")?;
    let fingerprint = format!("{:x}", Sha256::digest(certificate.as_ref()));
    let change = if registered && current.hub_url != shared.hub_url {
        format!(
            "\n現在の接続先 {} から住所を変更します。同じHub・同じPC登録であることを保存前に確認します。実行PCでは先に受付を一時停止し、仕事と状態不明の実行を解消してください。安全に終了できた実行機能は新しい接続先で再起動し、受付は停止のまま保持します。再開は明示的に行ってください。\n",
            current.hub_url
        )
    } else if current.configured() && current != shared {
        format!(
            "\n現在の接続先 {} から切り替えます。本人ログインと、このHubでのPC参加・project許可を確認してください。\n",
            current.hub_url
        )
    } else {
        String::new()
    };
    let action = if registered {
        "保存済みのPC登録でこのHubへ再接続します。"
    } else {
        "このHubへのPC参加を申請します。"
    };
    Ok(format!(
        "接続先: {}\n公開CAのSHA256: {}\n{}\n信頼できる管理者から受け取った接続先であることを確認してください。OKで{}\n\n取り込むのは公開接続情報です。既存のモデル設定と実行権限は保持します。本人設定とproject参加は、この後の共有仕事画面で案内します。",
        shared.hub_url, fingerprint, change, action
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public_config() -> SharedHubConfig {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["hub.test".into()]).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        SharedHubConfig {
            hub_url: "https://hub.test:9471".into(),
            ca_certificate_pem: params.self_signed(&key).unwrap().pem(),
        }
    }

    #[test]
    fn cold_and_existing_window_arguments_resolve_the_senders_directory() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
        for arguments in [
            vec!["moyai-desktop", "--join-config", "team config.toml"],
            vec!["moyai-desktop", "--join-config=team config.toml"],
        ] {
            let arguments = arguments.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(
                from_launch_args(&arguments, cwd.as_str()).unwrap(),
                Some(cwd.join("team config.toml"))
            );
        }
        assert_eq!(
            from_launch_args(&["moyai-desktop".into()], cwd.as_str()).unwrap(),
            None
        );
        assert!(
            from_launch_args(
                &["moyai-desktop".into(), "--join-config".into()],
                cwd.as_str()
            )
            .is_err()
        );
        assert!(
            from_launch_args(
                &[
                    "moyai-desktop".into(),
                    "--join-config=a".into(),
                    "--join-config=b".into()
                ],
                cwd.as_str()
            )
            .is_err()
        );
    }

    #[test]
    fn activation_load_is_bounded_and_projects_only_public_trust() {
        let temp = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(temp.path().join("team.toml")).unwrap();
        let shared = public_config();
        std::fs::write(&path, format!("[model]\nmodel='foreign-model'\n[permissions]\naccess_mode='full_access'\n[device_network]\n{}", toml::to_string(&shared).unwrap())).unwrap();
        assert_eq!(load(&path).unwrap(), shared);
        let review = review_text(&shared, &SharedHubConfig::default(), false).unwrap();
        assert!(review.contains("https://hub.test:9471"));
        assert!(!review.contains("foreign-model"));
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(MAX_JOIN_FILE_BYTES + 1).unwrap();
        assert!(load(&path).is_err());
        assert!(load(path.parent().unwrap()).is_err());
    }

    #[test]
    fn review_identifies_changed_hub_or_certificate() {
        let shared = public_config();
        let mut current = shared.clone();
        current.hub_url = "https://old-hub.test:9471".into();
        assert!(
            review_text(&shared, &current, false)
                .unwrap()
                .contains("old-hub.test")
        );
        let move_review = review_text(&shared, &current, true).unwrap();
        assert!(move_review.contains("同じHub・同じPC登録"));
        assert!(move_review.contains("受付は停止のまま"));
        current = public_config();
        assert!(
            review_text(&shared, &current, false)
                .unwrap()
                .contains("切り替え")
        );
        assert!(review_text(&shared, &current, true).is_err());
        assert!(
            !review_text(&shared, &shared, false)
                .unwrap()
                .contains("切り替え")
        );
        assert!(
            review_text(&shared, &shared, true)
                .unwrap()
                .contains("再接続")
        );
    }
}
