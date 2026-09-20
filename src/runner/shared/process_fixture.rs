//! A real host process with compile-time-only isolation of machine resource policy.
//! Production binaries expose no policy override, environment switch, or alternate entry.

#[test]
#[ignore = "Dedicated child process: prepare an isolated enrolled Desktop Runner"]
fn prepare_endpoint_change_configuration() {
    use crate::device_network::{DeviceIdentityStore, DeviceSettings, DeviceSettingsStore};
    use crate::runner::operations::{OperationsStore, ProvisionMode};
    use sha2::{Digest, Sha256};

    let config = crate::config::loader::global_config_path().unwrap();
    let directory = config.with_file_name("device-network");
    let identity = DeviceIdentityStore::new(directory.join("identity.json"))
        .load_or_create()
        .unwrap();
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::default();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
    let ca = ca_params.self_signed(&ca_key).unwrap().pem();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let mut params = rcgen::CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    let certificate = params
        .signed_by(
            &rcgen::KeyPair::from_pem(identity.private_key_pem()).unwrap(),
            &issuer,
        )
        .unwrap();
    let mut device = DeviceSettings::default();
    device.hub_id = Some("hub".into());
    device.device_id = Some("device".into());
    device.certificate_sha256 = Some(format!("{:x}", Sha256::digest(certificate.der())));
    device.certificate_pem = Some(certificate.pem());
    device.expires_at_ms = Some((crate::runner::operations::now_ms() + 86_400_000).to_string());
    DeviceSettingsStore::new(directory.join("device.json"))
        .save(&device)
        .unwrap();
    let mut value: toml::Value =
        toml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    let shared = toml::Value::try_from(crate::device_network::SharedHubConfig {
        // A stopped Hub must not prevent a paused, journal-empty Runner from exiting.
        hub_url: "https://127.0.0.1:1".into(),
        ca_certificate_pem: ca.clone(),
    })
    .unwrap();
    value
        .as_table_mut()
        .unwrap()
        .insert("device_network".into(), shared);
    std::fs::write(&config, toml::to_string(&value).unwrap()).unwrap();
    let data = camino::Utf8PathBuf::from(std::env::var("MOYAI_DATA_DIR").unwrap());
    let mut store = OperationsStore::open(&data).unwrap();
    let mut next = store.installed.clone();
    next.mode = ProvisionMode::Paused;
    next.settings = Some(super::SharedSettings {
        version: 1,
        hub_id: "hub".into(),
        device_id: "device".into(),
        environments: vec![],
        resource_scope: super::ResourceScope::Device,
    });
    next.desktop_binding = Some(format!(
        "hub|device|https://127.0.0.1:1|{:x}",
        Sha256::digest(ca.as_bytes())
    ));
    store.update(next).unwrap();
}

#[test]
#[ignore = "Dedicated child process: exercise the production endpoint-change IPC and exit wait"]
fn endpoint_change_client() {
    let binding = std::env::var("MOYAI_TEST_ENDPOINT_BINDING").unwrap();
    crate::runner::windows::quiesce_desktop_for_endpoint_change(&binding).unwrap();
}

#[test]
#[ignore = "Dedicated child process: MOYAI_TEST_RESOURCE_REGISTRY and optional MOYAI_TEST_SHARED_SETTINGS"]
fn isolated_runner_process() {
    use super::{SharedSettings, SharedWorker};
    use crate::runner::{RunnerHost, RunnerResponse, windows};
    let directory = camino::Utf8PathBuf::from(
        std::env::var("MOYAI_TEST_RESOURCE_REGISTRY")
            .expect("Dedicated Runner fixture requires an explicit isolated registry"),
    );
    crate::runtime::resource_admission::set_test_registry(directory);
    let listener = windows::LocalListener::bind().expect("fixture listener");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let host = runtime.block_on(RunnerHost::open()).expect("fixture host");
    let settings = match std::env::var("MOYAI_TEST_SHARED_SETTINGS") {
        Ok(path) => {
            Some(SharedSettings::load(camino::Utf8Path::new(&path)).expect("fixture settings"))
        }
        Err(_) => host
            .installed_shared_settings()
            .expect("installed fixture settings"),
    };
    let shared = settings.map(|settings| {
        runtime
            .block_on(SharedWorker::start(host.clone(), settings))
            .expect("fixture shared worker")
    });
    println!(
        "{}",
        serde_json::to_string(&RunnerResponse::Identity {
            identity: host.identity()
        })
        .unwrap()
    );
    let result = listener.serve(host.clone(), &runtime);
    host.begin_shutdown().unwrap();
    runtime.block_on(host.wait_stopped());
    if let Some(shared) = shared {
        runtime.block_on(shared.wait()).unwrap();
    }
    result.unwrap();
}
