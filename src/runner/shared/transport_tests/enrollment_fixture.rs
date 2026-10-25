//! Publication needs an enrolled identity even when Hub transport is scripted.
//! Give only these publication cases their own profile and child test process.

use std::process::{Command, Stdio};

use camino::{Utf8Path, Utf8PathBuf};
use sha2::{Digest, Sha256};

const CHILD_CASE: &str = "MOYAI_ENROLLED_TRANSPORT_CASE";

/// Returns true only in the isolated child that executes the existing test body.
pub(super) fn enter(case: &str) -> bool {
    let selected = format!("runner::shared::transport_tests::{case}");
    if std::env::var(CHILD_CASE).ok().as_deref() == Some(selected.as_str()) {
        prepare_enrollment();
        return true;
    }

    let temp = tempfile::tempdir().expect("isolated enrolled transport profile");
    let root = Utf8Path::from_path(temp.path()).expect("UTF-8 fixture profile");
    let config = root.join("config/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "").unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", &selected, "--nocapture", "--test-threads=1"])
        .env(CHILD_CASE, &selected)
        .env("MOYAI_CONFIG_PATH", &config)
        .env("MOYAI_DATA_DIR", root.join("data"))
        .env("MOYAI_DESKTOP_PREFS_PATH", root.join("config/desktop.toml"))
        .env("WEBVIEW2_USER_DATA_FOLDER", root.join("cache/webview"))
        .env_remove("MOYAI_DESKTOP_E2E_ROOT")
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    // Keep the profile alive until the exact child has closed its stores and
    // returned a result. The existing cfg(test) registry remains PID-isolated.
    let output = command.output().expect("enrolled transport child");
    assert!(
        output.status.success(),
        "{selected}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    // An outdated --exact filter must not make a zero-test child look successful.
    let enrolled = crate::device_network::DeviceSettingsStore::new(
        config.with_file_name("device-network").join("device.json"),
    )
    .load()
    .unwrap();
    assert_eq!(enrolled.hub_id.as_deref(), Some("hub"));
    assert_eq!(enrolled.device_id.as_deref(), Some("device"));
    false
}

fn prepare_enrollment() {
    use crate::device_network::{DeviceIdentityStore, DeviceSettings, DeviceSettingsStore};
    use crate::runner::shared::{ResourceScope, SharedSettings};

    let config = crate::config::loader::global_config_path().unwrap();
    let root = config.parent().unwrap().parent().unwrap();
    assert!(root.is_absolute());
    assert_eq!(
        crate::storage::StoragePaths::discover().unwrap().data_dir,
        root.join("data")
    );
    assert_eq!(
        std::env::var("MOYAI_DESKTOP_PREFS_PATH")
            .map(Utf8PathBuf::from)
            .unwrap(),
        root.join("config/desktop.toml")
    );
    assert_eq!(
        std::env::var("WEBVIEW2_USER_DATA_FOLDER")
            .map(Utf8PathBuf::from)
            .unwrap(),
        root.join("cache/webview")
    );
    let directory = config.with_file_name("device-network");
    let store = DeviceSettingsStore::new(directory.join("device.json"));
    let settings = SharedSettings {
        version: 1,
        hub_id: "hub".into(),
        device_id: "device".into(),
        environments: Vec::new(),
        resource_scope: ResourceScope::Device,
    };
    let assert_rejected = || {
        let failure = crate::runtime::resource_admission::register(&settings)
            .expect_err("missing or mismatched enrollment must not publish cached settings");
        assert!(
            failure
                .message
                .contains("no longer matches the enrolled Hub/device identity")
        );
    };
    assert_rejected();

    let identity = DeviceIdentityStore::new(directory.join("identity.json"))
        .load_or_create()
        .unwrap();
    let key = rcgen::KeyPair::from_pem(identity.private_key_pem()).unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    let certificate = params.self_signed(&key).unwrap();
    let mut device = store
        .save(&DeviceSettings {
            hub_id: Some("hub".into()),
            device_id: Some("other-device".into()),
            certificate_pem: Some(certificate.pem()),
            certificate_sha256: Some(format!("{:x}", Sha256::digest(certificate.der()))),
            expires_at_ms: Some((crate::runner::operations::now_ms() + 86_400_000).to_string()),
            ..DeviceSettings::default()
        })
        .unwrap();
    assert_rejected();
    device.device_id = Some("device".into());
    let saved = store.save(&device).unwrap();
    assert_eq!(
        saved.device_id.as_deref(),
        Some(settings.device_id.as_str())
    );
}
