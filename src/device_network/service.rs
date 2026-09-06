use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use super::client::EnrollmentReceipt;
use super::{
    DeviceClient, DeviceError, DeviceIdentity, DeviceIdentityStore, DeviceSettings,
    DeviceSettingsStore, DirectoryPeer, ReceiverSettings, SelectedPeer, SharedHubConfig,
    canonical_revision,
};
use crate::config::{AccessMode, ResolvedConfig};
use crate::mcp_publish::{PublishService, PublishTarget};
use crate::remote_agent::RemoteJobService;
use crate::storage::StoreBundle;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceTargetChoice {
    pub target: PublishTarget,
    pub label: String,
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::storage::{SqliteStore, StoragePaths};
    async fn fixture() -> (tempfile::TempDir, DeviceNetworkService) {
        let temp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(temp.path().to_owned()).unwrap();
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let paths = StoragePaths {
            data_dir: root.join("data"),
            database_path: root.join("data/db.sqlite3"),
            truncation_dir: root.join("data/output"),
        };
        let sqlite = SqliteStore::open(&paths).unwrap();
        sqlite.migrate().unwrap();
        let service = DeviceNetworkService::for_workspace(
            root.join("config/device"),
            workspace,
            StoreBundle::new(sqlite),
            ResolvedConfig::default(),
        )
        .await
        .unwrap();
        (temp, service)
    }
    #[tokio::test]
    async fn receiver_confirmation_is_reused_only_for_the_same_authority() {
        let (_temp, service) = fixture().await;
        let first = service
            .receiver(
                false,
                PublishTarget::Temp {},
                AccessMode::Default,
                crate::hub::HubRouteMode::Direct,
                true,
                false,
                false,
                "0",
                "0",
            )
            .await
            .unwrap();
        assert!(first.receiver.confirmed);
        let same = service
            .receiver(
                false,
                PublishTarget::Temp {},
                AccessMode::Default,
                crate::hub::HubRouteMode::Direct,
                false,
                true,
                true,
                &first.revision,
                &first.generation,
            )
            .await
            .unwrap();
        assert!(same.receiver.confirmed);
        let changed = service
            .receiver(
                false,
                PublishTarget::Temp {},
                AccessMode::FullAccess,
                crate::hub::HubRouteMode::Direct,
                false,
                true,
                true,
                &same.revision,
                &same.generation,
            )
            .await
            .unwrap();
        assert!(!changed.receiver.confirmed);
        assert!(matches!(
            service
                .receiver(
                    true,
                    PublishTarget::Temp {},
                    AccessMode::FullAccess,
                    crate::hub::HubRouteMode::Direct,
                    false,
                    true,
                    true,
                    &changed.revision,
                    &changed.generation
                )
                .await,
            Err(DeviceError::ConfirmationRequired)
        ));
    }
    #[tokio::test]
    async fn receiver_restart_and_hide_do_not_reenable_a_saved_reception_intent() {
        let (_temp, service) = fixture().await;
        let mut settings = service.inner.settings.load().unwrap();
        settings.receiver.enabled = true;
        settings.receiver.confirmed = true;
        let settings = service.inner.settings.save(&settings).unwrap();
        let reopened = DeviceNetworkService::new(
            service.inner.directory.clone(),
            service.inner.store.clone(),
            service.inner.global_config.lock().unwrap().clone(),
            service.inner.jobs.clone(),
            service.inner.publish.clone(),
        );
        assert!(!reopened.projection_now().receiver.enabled);
        assert_eq!(reopened.projection_now().receiver.status, "stopped");
        assert!(!service.inner.directory.join("identity.json").exists());
        {
            let mut state = reopened.inner.state.lock().unwrap();
            state.settings = settings;
            state.receiver_requested = true;
            state.receiver_status = "receiving";
        }
        assert!(reopened.window_hide_requested());
        reopened.window_shown();
        assert!(!reopened.projection_now().receiver.enabled);
        assert!(reopened.projection_now().receiver.confirmed);
        assert!(
            reopened.inner.settings.load().unwrap().receiver.enabled,
            "window lifecycle must not rewrite saved authority"
        );
        {
            let mut state = reopened.inner.state.lock().unwrap();
            state.receiver_requested = true;
            state.receiver_status = "receiving";
        }
        reopened.finish_window_hide(true).await;
        assert!(
            reopened.projection_now().receiver.enabled,
            "late hide completion cannot undo a newly requested reception"
        );
        assert_eq!(reopened.projection_now().receiver.status, "receiving");
    }
    #[tokio::test]
    async fn explicit_reconnect_preserves_identity_and_never_uses_start_on_launch_to_receive() {
        let (_temp, service) = fixture().await;
        let identity = service.inner.identity.load_or_create().unwrap();
        let key = rcgen::KeyPair::from_pem(identity.private_key_pem()).unwrap();
        let cert = rcgen::CertificateParams::new(vec!["127.0.0.1".into()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        {
            let mut state = service.inner.state.lock().unwrap();
            state.shared = SharedHubConfig {
                hub_url: format!("https://{address}"),
                ca_certificate_pem: cert.pem(),
            };
            state.identity = Some(identity);
            let mut saved = state.settings.clone();
            saved.hub_id = Some("hub-fixture".into());
            saved.device_id = Some("device-fixture".into());
            saved.label = "Win19".into();
            saved.certificate_pem = Some(cert.pem());
            saved.certificate_sha256 = Some("a".repeat(64));
            saved.expires_at_ms = Some("1".into());
            saved.receiver.enabled = true;
            saved.receiver.confirmed = true;
            saved.receiver.start_on_launch = true;
            saved.selected_peers.push(SelectedPeer {
                device_id: "device-other".into(),
                profile_id: "profile-other".into(),
            });
            state.settings = service.inner.settings.save(&saved).unwrap();
            state.status = "disconnected";
        }
        let projection = service.refresh().await.unwrap();
        assert_eq!(projection.device_id.as_deref(), Some("device-fixture"));
        assert_eq!(
            projection.enrollment, "error",
            "explicit refresh must attempt the registered connection"
        );
        assert!(!projection.receiver.enabled);
        assert!(service.inner.receiver.lock().await.is_none());
        assert!(
            projection
                .peers
                .iter()
                .any(|peer| peer.device_id == "device-other" && peer.selected)
        );
        service.shutdown().await;
    }
    #[tokio::test]
    async fn failed_receiver_start_keeps_explicit_off_and_retry_available() {
        let (_temp, service) = fixture().await;
        {
            let mut state = service.inner.state.lock().unwrap();
            state.receiver_requested = true;
            state.receiver_status = "starting";
        }
        assert_eq!(
            service.start_receiver().await,
            Err(DeviceError::Unavailable)
        );
        let projection = service.projection_now();
        assert_eq!(projection.receiver.status, "error");
        assert!(projection.receiver.enabled);
        assert!(projection.receiver.can_change);
        let off = service
            .receiver(
                false,
                PublishTarget::Temp {},
                AccessMode::Default,
                crate::hub::HubRouteMode::Hub,
                false,
                false,
                false,
                &projection.revision,
                &projection.generation,
            )
            .await
            .unwrap();
        assert!(!off.receiver.enabled);
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicePeerProjection {
    pub device_id: String,
    pub profile_id: String,
    pub display_name: String,
    pub name: String,
    pub selected: bool,
    pub online: bool,
    pub receiving: bool,
    pub can_use: bool,
    pub reason: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceReceiverProjection {
    pub profile_id: String,
    pub enabled: bool,
    pub status: String,
    pub target: PublishTarget,
    pub access_mode: AccessMode,
    pub model_mode: crate::hub::HubRouteMode,
    pub start_on_launch: bool,
    pub keep_when_hidden: bool,
    pub confirmed: bool,
    pub endpoint: Option<String>,
    pub can_change: bool,
    pub reason: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceNetworkProjection {
    pub revision: String,
    pub generation: String,
    pub hub_url: String,
    pub device_id: Option<String>,
    pub display_name: String,
    pub local_hostname: String,
    pub enrollment: String,
    pub receiver: DeviceReceiverProjection,
    pub targets: Vec<DeviceTargetChoice>,
    pub peers: Vec<DevicePeerProjection>,
    pub can_join: bool,
    pub can_leave: bool,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct DeviceNetworkService {
    pub(super) inner: Arc<DeviceNetworkInner>,
}
#[derive(Clone)]
pub(crate) struct WeakDeviceNetwork {
    inner: Weak<DeviceNetworkInner>,
}
impl WeakDeviceNetwork {
    pub(crate) fn upgrade(&self) -> Option<DeviceNetworkService> {
        self.inner
            .upgrade()
            .map(|inner| DeviceNetworkService { inner })
    }
}

pub(super) struct DeviceNetworkInner {
    pub state: Mutex<DeviceState>,
    pub lane: AsyncMutex<()>,
    pub settings: DeviceSettingsStore,
    pub identity: DeviceIdentityStore,
    pub directory: Utf8PathBuf,
    pub store: StoreBundle,
    pub jobs: RemoteJobService,
    pub publish: PublishService,
    pub protected: Vec<Utf8PathBuf>,
    pub global_config: Mutex<ResolvedConfig>,
    pub hub: Mutex<Option<crate::hub::HubConnection>>,
    pub use_default_model: std::sync::atomic::AtomicBool,
    pub receiver: AsyncMutex<Option<super::receiver::ManagedReceiver>>,
    pub outgoing: super::outgoing::OutgoingOwner,
}
pub(super) struct DeviceState {
    pub settings: DeviceSettings,
    pub shared: SharedHubConfig,
    pub generation: u64,
    pub status: &'static str,
    pub client: Option<DeviceClient>,
    pub identity: Option<DeviceIdentity>,
    pub peers: Vec<DirectoryPeer>,
    pub receiver_status: &'static str,
    pub receiver_endpoint: Option<String>,
    pub receiver_requested: bool,
    pub cancellation: CancellationToken,
    pub error: Option<DeviceError>,
    pub closing: bool,
    pub hidden: bool,
}
impl DeviceState {
    pub fn check(&self, revision: &str, generation: &str) -> Result<(), DeviceError> {
        if self.closing || canonical_revision(generation) != Some(self.generation) {
            return Err(DeviceError::ConnectionChanged);
        }
        if self.settings.revision != revision {
            return Err(DeviceError::SettingsChanged);
        }
        if self.error == Some(DeviceError::SettingsCorrupt) {
            return Err(DeviceError::SettingsCorrupt);
        }
        Ok(())
    }
}

impl std::fmt::Debug for DeviceNetworkService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceNetworkService(<runtime>)")
    }
}

impl DeviceNetworkService {
    pub(crate) fn execution_identity(
        &self,
        authority: &super::VerifiedGrant,
    ) -> Result<serde_json::Value, DeviceError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| DeviceError::Unavailable)?;
        if state.settings.hub_id.as_ref() != Some(&authority.claims().hub_id)
            || state.settings.device_id.as_ref() != Some(&authority.claims().audience_device_id)
            || state.settings.label.trim().is_empty()
        {
            return Err(DeviceError::InvalidIdentity);
        }
        Ok(serde_json::json!({
            "device_id":state.settings.device_id,
            "display_name":state.settings.label,
            "os_hostname":std::env::var("COMPUTERNAME").ok(),
        }))
    }

    /// Host the same receiver runtime without a Desktop window. The caller owns its
    /// isolated store, workspace and shutdown; no GUI singleton contract is bypassed.
    pub async fn for_workspace(
        directory: Utf8PathBuf,
        workspace: Utf8PathBuf,
        store: StoreBundle,
        config: ResolvedConfig,
    ) -> Result<Self, DeviceError> {
        let process_runtime = crate::app::AppBootstrap::create_process_runtime(store.clone())
            .await
            .map_err(|_| DeviceError::Unavailable)?;
        let app = crate::app::AppBootstrap::rebuild_for_directory_as_workspace_root_with_process_runtime_and_config(
            &workspace,
            process_runtime,
            config.clone(),
        )
        .await
        .map_err(|_| DeviceError::InvalidConfiguration)?;
        let jobs = RemoteJobService::new(app.process_runtime.clone())
            .map_err(|_| DeviceError::Unavailable)?;
        let publish = PublishService::new(
            directory.join("manual-publish.json"),
            store.clone(),
            config.clone(),
        );
        Ok(Self::new(directory, store, config, jobs, publish))
    }
    pub fn new(
        directory: Utf8PathBuf,
        store: StoreBundle,
        config: ResolvedConfig,
        jobs: RemoteJobService,
        publish: PublishService,
    ) -> Self {
        let settings_store = DeviceSettingsStore::new(directory.join("device.json"));
        let identity_store = DeviceIdentityStore::new(directory.join("identity.json"));
        let (settings, error) = match settings_store.load() {
            Ok(settings) => (settings, None),
            Err(error) => (DeviceSettings::default(), Some(error)),
        };
        let identity = identity_store.load();
        let error = error.or(identity.as_ref().err().copied());
        let shared = config.device_network.clone();
        let status = if error.is_some() {
            "error"
        } else if !shared.configured() {
            "unconfigured"
        } else if settings.device_id.is_none() {
            "not_enrolled"
        } else {
            "disconnected"
        };
        let state = DeviceState {
            settings,
            shared,
            generation: 0,
            status,
            client: None,
            identity: identity.ok().flatten(),
            peers: vec![],
            receiver_status: "stopped",
            receiver_endpoint: None,
            receiver_requested: false,
            cancellation: CancellationToken::new(),
            error,
            closing: false,
            hidden: false,
        };
        let protected = vec![
            directory.parent().unwrap_or(&directory).to_owned(),
            store.paths().data_dir.clone(),
        ];
        let service = Self {
            inner: Arc::new(DeviceNetworkInner {
                state: Mutex::new(state),
                lane: AsyncMutex::new(()),
                settings: settings_store,
                identity: identity_store,
                directory,
                store: store.clone(),
                jobs,
                publish,
                protected,
                global_config: Mutex::new(config),
                hub: Mutex::new(None),
                use_default_model: std::sync::atomic::AtomicBool::new(false),
                receiver: AsyncMutex::new(None),
                outgoing: super::outgoing::OutgoingOwner::new(store.clone()),
            }),
        };
        store.attach_device_network(service.downgrade());
        service
    }
    pub(crate) fn downgrade(&self) -> WeakDeviceNetwork {
        WeakDeviceNetwork {
            inner: Arc::downgrade(&self.inner),
        }
    }
    pub fn attach_hub_connection(&self, hub: crate::hub::HubConnection) {
        *self.inner.hub.lock().unwrap() = Some(hub);
    }
    pub fn enable_default_model_on_join(&self) {
        self.inner
            .use_default_model
            .store(true, std::sync::atomic::Ordering::Release);
    }
    async fn adopt_model_session(&self, force: bool) {
        let hub = self.inner.hub.lock().unwrap().clone();
        let Some(hub) = hub else {
            return;
        };
        if !force && hub.projection_now().status == crate::hub::HubConnectionStatus::Connected {
            return;
        }
        let Ok(client) = self.client() else {
            return;
        };
        let use_default = {
            let config = self.inner.global_config.lock().unwrap();
            self.inner
                .use_default_model
                .load(std::sync::atomic::Ordering::Acquire)
                || config.model.model.trim().is_empty()
        };
        if hub
            .connect_device(&client.endpoint(), client.http(), use_default)
            .await
            .is_ok()
        {
            self.inner
                .use_default_model
                .store(false, std::sync::atomic::Ordering::Release);
        }
    }
    pub fn projection_now(&self) -> DeviceNetworkProjection {
        let state = self
            .inner
            .state
            .lock()
            .expect("device network state poisoned");
        let active = state.status == "active" && !state.closing;
        let mut peers: Vec<_> = state
            .peers
            .iter()
            .map(|peer| DevicePeerProjection {
                device_id: peer.device_id.clone(),
                profile_id: peer.profile_id.clone(),
                display_name: peer.label.clone(),
                name: peer.name.clone(),
                selected: state.settings.selected_peers.iter().any(|selected| {
                    selected.device_id == peer.device_id && selected.profile_id == peer.profile_id
                }),
                online: active,
                receiving: active,
                can_use: active && peer.mode == "agent",
                reason: if !active {
                    Some("unavailable".into())
                } else if peer.mode != "agent" {
                    Some("read_tools_only".into())
                } else {
                    None
                },
            })
            .collect();
        for selected in &state.settings.selected_peers {
            if !peers.iter().any(|peer| {
                peer.device_id == selected.device_id && peer.profile_id == selected.profile_id
            }) {
                peers.push(DevicePeerProjection {
                    device_id: selected.device_id.clone(),
                    profile_id: selected.profile_id.clone(),
                    display_name: selected.device_id.clone(),
                    name: selected.profile_id.clone(),
                    selected: true,
                    online: false,
                    receiving: false,
                    can_use: false,
                    reason: Some("not_available_or_not_allowed".into()),
                });
            }
        }
        let receiver = &state.settings.receiver;
        DeviceNetworkProjection {
            revision: state.settings.revision.clone(),
            generation: state.generation.to_string(),
            hub_url: state.shared.hub_url.clone(),
            device_id: state.settings.device_id.clone(),
            display_name: state.settings.label.clone(),
            local_hostname: std::env::var("COMPUTERNAME")
                .unwrap_or_else(|_| "moyAI Desktop".into()),
            enrollment: state.status.into(),
            receiver: DeviceReceiverProjection {
                profile_id: receiver.profile_id.0.to_string(),
                enabled: state.receiver_requested,
                status: state.receiver_status.into(),
                target: receiver.target.clone(),
                access_mode: receiver.access_mode,
                model_mode: receiver.model_mode,
                start_on_launch: receiver.start_on_launch,
                keep_when_hidden: receiver.keep_when_hidden,
                confirmed: receiver.confirmed,
                endpoint: state.receiver_endpoint.clone(),
                can_change: !state.closing && state.receiver_status != "starting",
                reason: if !active {
                    Some("connection_required".into())
                } else {
                    state.error.map(|error| error.to_string())
                },
            },
            targets: self
                .inner
                .publish
                .projection_now()
                .targets
                .into_iter()
                .filter(|choice| !matches!(choice.target, PublishTarget::LegacySession { .. }))
                .map(|choice| DeviceTargetChoice {
                    target: choice.target,
                    label: choice.label,
                })
                .collect(),
            peers,
            can_join: !state.closing
                && state.shared.configured()
                && state.settings.device_id.is_none()
                && state.status != "pending",
            can_leave: !state.closing
                && state.status != "pending"
                && state.settings.device_id.is_some(),
            error: state.error.map(|error| error.to_string()),
        }
    }
    pub fn polling_required(&self) -> bool {
        let state = self
            .inner
            .state
            .lock()
            .expect("device network state poisoned");
        state.client.is_some() || state.status == "pending" || state.receiver_status == "starting"
    }
    pub(crate) fn client(&self) -> Result<DeviceClient, DeviceError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| DeviceError::Unavailable)?;
        if state.closing || state.status != "active" {
            return Err(DeviceError::Unavailable);
        }
        state.client.clone().ok_or(DeviceError::Unavailable)
    }
    pub fn update_runtime_config(&self, config: ResolvedConfig) {
        *self
            .inner
            .global_config
            .lock()
            .expect("device config poisoned") = config;
    }
    pub fn check_target(&self, revision: &str, generation: &str) -> Result<(), DeviceError> {
        self.inner
            .state
            .lock()
            .map_err(|_| DeviceError::Unavailable)?
            .check(revision, generation)
    }
    pub async fn configure(
        &self,
        shared: SharedHubConfig,
        revision: &str,
        generation: &str,
    ) -> Result<DeviceNetworkProjection, DeviceError> {
        self.configure_with_commit(shared, revision, generation, || Ok(()))
            .await
    }
    pub async fn configure_with_commit(
        &self,
        shared: SharedHubConfig,
        revision: &str,
        generation: &str,
        commit: impl FnOnce() -> Result<(), DeviceError>,
    ) -> Result<DeviceNetworkProjection, DeviceError> {
        shared.validate()?;
        let _lane = self.inner.lane.lock().await;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| DeviceError::Unavailable)?;
        state.check(revision, generation)?;
        if state.settings.device_id.is_some() && state.shared != shared {
            return Err(DeviceError::ConnectionChanged);
        }
        commit()?;
        state.shared = shared;
        state.generation += 1;
        state.status = if state.settings.device_id.is_some() {
            "disconnected"
        } else {
            "not_enrolled"
        };
        state.error = None;
        drop(state);
        Ok(self.projection_now())
    }
    pub async fn join(
        &self,
        code: String,
        confirmed: bool,
        revision: &str,
        generation: &str,
    ) -> Result<DeviceNetworkProjection, DeviceError> {
        if !confirmed {
            return Err(DeviceError::ConfirmationRequired);
        }
        if code.trim().is_empty() || code.len() > 256 {
            return Err(DeviceError::EnrollmentDenied);
        }
        let _lane = self.inner.lane.lock().await;
        let (shared, identity) = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| DeviceError::Unavailable)?;
            state.check(revision, generation)?;
            if state.settings.device_id.is_some() {
                return Err(DeviceError::ConnectionChanged);
            }
            let identity = self.inner.identity.load_or_create()?;
            state.identity = Some(identity.clone());
            state.status = "pending";
            (state.shared.clone(), identity)
        };
        let result = async {
            let client = DeviceClient::new(&shared, None, String::new())?;
            let ip = client.route_ip().await?;
            let csr = self.stable_csr(&identity, ip)?;
            let receipt = client.enroll(code.trim(), &csr).await?;
            self.adopt_receipt(receipt, &shared, &identity, generation)?;
            Ok::<(), DeviceError>(())
        }
        .await;
        drop(code);
        if let Err(error) = result {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| DeviceError::Unavailable)?;
            if state.closing || canonical_revision(generation) != Some(state.generation) {
                return Err(DeviceError::ConnectionChanged);
            }
            state.error = Some(error);
            state.status = "not_enrolled";
            return Err(error);
        }
        drop(_lane);
        self.start_heartbeat();
        self.refresh().await
    }
    fn stable_csr(&self, identity: &DeviceIdentity, ip: Ipv4Addr) -> Result<String, DeviceError> {
        use std::io::Write;
        #[derive(serde::Serialize, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Csr {
            ip: Ipv4Addr,
            csr_pem: String,
        }
        let path = self.inner.directory.join("pending-csr.json");
        if let Ok(bytes) = std::fs::read(&path) {
            if bytes.len() > 65536 {
                return Err(DeviceError::InvalidIdentity);
            }
            let old: Csr =
                serde_json::from_slice(&bytes).map_err(|_| DeviceError::InvalidIdentity)?;
            if old.ip == ip {
                return Ok(old.csr_pem);
            }
        }
        let csr_pem = identity.csr(ip)?;
        let mut file = tempfile::NamedTempFile::new_in(&self.inner.directory)
            .map_err(|_| DeviceError::Storage)?;
        file.write_all(
            &serde_json::to_vec(&Csr {
                ip,
                csr_pem: csr_pem.clone(),
            })
            .map_err(|_| DeviceError::Storage)?,
        )
        .map_err(|_| DeviceError::Storage)?;
        file.as_file()
            .sync_all()
            .map_err(|_| DeviceError::Storage)?;
        file.persist(path).map_err(|_| DeviceError::Storage)?;
        Ok(csr_pem)
    }
    fn adopt_receipt(
        &self,
        receipt: EnrollmentReceipt,
        shared: &SharedHubConfig,
        identity: &DeviceIdentity,
        generation: &str,
    ) -> Result<(), DeviceError> {
        use sha2::{Digest, Sha256};
        use tokio_rustls::rustls::pki_types::{CertificateDer, pem::PemObject};
        let supplied = CertificateDer::from_pem_slice(receipt.ca_certificate_pem.as_bytes())
            .map_err(|_| DeviceError::InvalidResponse)?;
        let trusted = CertificateDer::from_pem_slice(shared.ca_certificate_pem.as_bytes())
            .map_err(|_| DeviceError::InvalidConfiguration)?;
        let certificate = CertificateDer::from_pem_slice(receipt.certificate_pem.as_bytes())
            .map_err(|_| DeviceError::InvalidResponse)?;
        if supplied != trusted
            || format!("{:x}", Sha256::digest(certificate.as_ref())) != receipt.certificate_sha256
        {
            return Err(DeviceError::InvalidResponse);
        }
        crate::mcp_publish::tls::load_mtls_acceptor(
            &receipt.certificate_pem,
            identity.private_key_pem(),
            &shared.ca_certificate_pem,
        )
        .map_err(|_| DeviceError::InvalidIdentity)?;
        let client = DeviceClient::new(
            shared,
            Some((identity, &receipt.certificate_pem)),
            receipt.device_id.clone(),
        )?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| DeviceError::Unavailable)?;
        if state.closing
            || canonical_revision(generation) != Some(state.generation)
            || state
                .settings
                .hub_id
                .as_ref()
                .is_some_and(|id| id != &receipt.hub_id)
            || state
                .settings
                .device_id
                .as_ref()
                .is_some_and(|id| id != &receipt.device_id)
        {
            return Err(DeviceError::ConnectionChanged);
        }
        let mut proposed = state.settings.clone();
        proposed.hub_id = Some(receipt.hub_id);
        proposed.device_id = Some(receipt.device_id);
        proposed.label = receipt.label;
        proposed.certificate_pem = Some(receipt.certificate_pem);
        proposed.certificate_sha256 = Some(receipt.certificate_sha256);
        proposed.expires_at_ms = Some(receipt.expires_at_ms.to_string());
        state.settings = self.inner.settings.save(&proposed)?;
        state.client = Some(client);
        state.status = "active";
        state.error = None;
        state.generation += 1;
        Ok(())
    }
    pub async fn resume(&self) -> Result<DeviceNetworkProjection, DeviceError> {
        self.resume_with_startup(true).await
    }
    async fn resume_with_startup(
        &self,
        startup: bool,
    ) -> Result<DeviceNetworkProjection, DeviceError> {
        let _lane = self.inner.lane.lock().await;
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| DeviceError::Unavailable)?;
            if state.closing {
                return Err(DeviceError::ConnectionChanged);
            }
            let Some(device_id) = state.settings.device_id.clone() else {
                return Ok(self.projection_without_lock(state));
            };
            let identity = state
                .identity
                .as_ref()
                .ok_or(DeviceError::InvalidIdentity)?;
            let certificate = state
                .settings
                .certificate_pem
                .as_deref()
                .ok_or(DeviceError::InvalidIdentity)?;
            state.client = Some(DeviceClient::new(
                &state.shared,
                Some((identity, certificate)),
                device_id,
            )?);
            state.status = "active";
        }
        drop(_lane);
        let result = self.refresh_connected().await;
        self.start_heartbeat();
        if result.is_ok() {
            let settings = self.inner.state.lock().unwrap().settings.clone();
            if startup && settings.receiver.enabled && settings.receiver.start_on_launch {
                let _lane = self.inner.lane.lock().await;
                self.inner.state.lock().unwrap().receiver_requested = true;
                self.start_receiver().await?;
            }
        }
        result
    }
    fn projection_without_lock(
        &self,
        state: std::sync::MutexGuard<'_, DeviceState>,
    ) -> DeviceNetworkProjection {
        drop(state);
        self.projection_now()
    }
    pub async fn refresh(&self) -> Result<DeviceNetworkProjection, DeviceError> {
        let reconnect = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| DeviceError::Unavailable)?;
            !state.closing && state.client.is_none() && state.settings.device_id.is_some()
        };
        if reconnect {
            self.resume_with_startup(false).await
        } else {
            self.refresh_connected().await
        }
    }
    async fn refresh_connected(&self) -> Result<DeviceNetworkProjection, DeviceError> {
        let _lane = self.inner.lane.lock().await;
        let (client, generation, hub_id) = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| DeviceError::Unavailable)?;
            let Some(client) = state.client.clone() else {
                return Ok(self.projection_without_lock(state));
            };
            (client, state.generation, state.settings.hub_id.clone())
        };
        let result = async {
            client.presence().await?;
            let own = client.self_status().await?;
            let directory = client.directory(None).await?;
            if own.device_id != client.device_id
                || Some(&own.hub_id) != hub_id.as_ref()
                || own.hub_id != directory.hub_id
                || own.label.len() > 256
                || own.label.chars().any(char::is_control)
                || own.groups.len() > 256
                || own.groups.iter().any(|group| !super::stable_id(group))
                || canonical_revision(&own.revision).is_none()
                || canonical_revision(&directory.revision) < canonical_revision(&own.revision)
            {
                return Err(DeviceError::InvalidResponse);
            }
            self.cancel_announced_lineages(&own.cancelled_lineages);
            Ok::<_, DeviceError>((directory.peers, own))
        }
        .await;
        let accepted = result.is_ok();
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| DeviceError::Unavailable)?;
            if state.generation != generation || state.closing {
                return Err(DeviceError::ConnectionChanged);
            }
            match result {
                Ok((peers, own)) => {
                    if state.settings.certificate_sha256.as_ref() != Some(&own.certificate_sha256) {
                        state.status = "error";
                        state.error = Some(DeviceError::InvalidIdentity);
                        return Err(DeviceError::InvalidIdentity);
                    }
                    if state
                        .settings
                        .expires_at_ms
                        .as_deref()
                        .and_then(canonical_revision)
                        != Some(own.expires_at_ms)
                    {
                        return Err(DeviceError::InvalidResponse);
                    }
                    if state.settings.label != own.label {
                        let mut settings = state.settings.clone();
                        settings.label = own.label;
                        state.settings = self.inner.settings.save(&settings)?;
                    }
                    state.peers = peers;
                    state.status = "active";
                    state.error = None;
                }
                Err(error) => {
                    state.status = if error == DeviceError::Revoked {
                        "revoked"
                    } else {
                        "error"
                    };
                    state.error = Some(error);
                    self.inner
                        .jobs
                        .set_network_accepting(state.settings.receiver.profile_id, false);
                    state.receiver_status = if state.receiver_endpoint.is_some() {
                        "error"
                    } else {
                        "stopped"
                    };
                }
            }
        }
        if accepted {
            if let Err(error) = self.announce_receiver_with(&client).await {
                let mut state = self.inner.state.lock().unwrap();
                state.error = Some(error);
                state.receiver_status = "error";
                self.inner
                    .jobs
                    .set_network_accepting(state.settings.receiver.profile_id, false);
            }
            self.adopt_model_session(false).await;
        }
        Ok(self.projection_now())
    }
    pub async fn select(
        &self,
        device_id: String,
        profile_id: String,
        enabled: bool,
        revision: &str,
        generation: &str,
    ) -> Result<DeviceNetworkProjection, DeviceError> {
        let _lane = self.inner.lane.lock().await;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| DeviceError::Unavailable)?;
        state.check(revision, generation)?;
        if enabled
            && (state.status != "active"
                || !state.peers.iter().any(|peer| {
                    peer.device_id == device_id
                        && peer.profile_id == profile_id
                        && peer.mode == "agent"
                }))
        {
            return Err(DeviceError::PolicyDenied);
        }
        let mut next = state.settings.clone();
        next.selected_peers
            .retain(|peer| peer.device_id != device_id || peer.profile_id != profile_id);
        if enabled {
            next.selected_peers.push(SelectedPeer {
                device_id,
                profile_id,
            });
        }
        state.settings = self.inner.settings.save(&next)?;
        drop(state);
        Ok(self.projection_now())
    }
    pub async fn receiver(
        &self,
        enabled: bool,
        target: PublishTarget,
        access_mode: AccessMode,
        model_mode: crate::hub::HubRouteMode,
        confirmed: bool,
        start_on_launch: bool,
        keep_when_hidden: bool,
        revision: &str,
        generation: &str,
    ) -> Result<DeviceNetworkProjection, DeviceError> {
        let _lane = self.inner.lane.lock().await;
        let old = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| DeviceError::Unavailable)?;
            state.check(revision, generation)?;
            state.settings.clone()
        };
        let same_authority = old.receiver.target == target
            && old.receiver.access_mode == access_mode
            && old.receiver.model_mode == model_mode;
        let confirmed = confirmed || (same_authority && old.receiver.confirmed);
        if matches!(target, PublishTarget::LegacySession { .. }) || (enabled && !confirmed) {
            return Err(DeviceError::ConfirmationRequired);
        }
        if enabled {
            self.client()?;
        }
        if old.receiver.target != target
            || old.receiver.access_mode != access_mode
            || old.receiver.model_mode != model_mode
        {
            if self.inner.jobs.has_active_profile(old.receiver.profile_id) {
                return Err(DeviceError::ReceiverBusy);
            }
            self.stop_receiver_transport().await;
            if self.inner.receiver.lock().await.is_some() {
                return Err(DeviceError::ReceiverBusy);
            }
            self.inner.jobs.cancel_profile(old.receiver.profile_id);
        }
        crate::mcp_publish::dispatch::validate_target(
            &self.inner.store,
            &target,
            &self.inner.protected,
        )
        .await
        .map_err(|_| DeviceError::InvalidConfiguration)?;
        let mut next = old;
        next.receiver = ReceiverSettings {
            profile_id: next.receiver.profile_id,
            enabled,
            target,
            access_mode,
            model_mode,
            confirmed,
            start_on_launch,
            keep_when_hidden,
        };
        let saved = self.inner.settings.save(&next)?;
        {
            let mut state = self.inner.state.lock().unwrap();
            state.settings = saved;
            state.receiver_requested = enabled;
        }
        if enabled {
            self.start_receiver().await?;
        } else {
            self.pause_receiver().await?;
        }
        Ok(self.projection_now())
    }
    fn start_heartbeat(&self) {
        let (cancel, generation) = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("device network state poisoned");
            state.cancellation.cancel();
            state.cancellation = CancellationToken::new();
            (state.cancellation.clone(), state.generation)
        };
        let weak = self.downgrade();
        tokio::spawn(async move {
            loop {
                tokio::select! { _ = cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(10)) => {} }
                let Some(service) = weak.upgrade() else {
                    break;
                };
                if service.inner.state.lock().unwrap().generation != generation {
                    break;
                }
                let _ = service.refresh_connected().await;
                let _ = service.renew_if_needed().await;
                service.poll_outgoing().await;
            }
        });
    }
    async fn renew_if_needed(&self) -> Result<(), DeviceError> {
        let _lane = self.inner.lane.lock().await;
        let (client, identity, shared, generation, expiry) = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| DeviceError::Unavailable)?;
            let Some(client) = state.client.clone() else {
                return Ok(());
            };
            (
                client,
                state.identity.clone().ok_or(DeviceError::InvalidIdentity)?,
                state.shared.clone(),
                state.generation.to_string(),
                state
                    .settings
                    .expires_at_ms
                    .as_deref()
                    .and_then(canonical_revision)
                    .unwrap_or(0),
            )
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let ip = client.route_ip().await?;
        let changed_ip = self
            .inner
            .receiver
            .lock()
            .await
            .as_ref()
            .is_some_and(|receiver| receiver.ip != ip);
        if expiry > now.saturating_add(7 * 86400 * 1000) && !changed_ip {
            return Ok(());
        }
        let csr = self.stable_csr(&identity, ip)?;
        let receipt = client.renew(&csr).await?;
        self.adopt_receipt(receipt, &shared, &identity, &generation)?;
        self.restart_receiver_certificate().await?;
        self.adopt_model_session(true).await;
        self.start_heartbeat();
        Ok(())
    }
    pub fn begin_shutdown(&self) {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("device network state poisoned");
        state.closing = true;
        state.receiver_requested = false;
        state.generation += 1;
        state.cancellation.cancel();
        self.inner
            .jobs
            .cancel_profile(state.settings.receiver.profile_id);
    }
    pub async fn shutdown(&self) {
        self.begin_shutdown();
        self.stop_receiver_transport().await;
        self.cancel_all_outgoing().await;
    }
    pub async fn window_hidden(&self) {
        let pause = self.window_hide_requested();
        self.finish_window_hide(pause).await;
    }
    pub async fn finish_window_hide(&self, pause: bool) {
        let _lane = self.inner.lane.lock().await;
        let still_hidden = {
            let state = self.inner.state.lock().unwrap();
            state.hidden && !state.receiver_requested
        };
        if pause && still_hidden {
            let _ = self.pause_receiver().await;
        }
    }
    pub fn window_hide_requested(&self) -> bool {
        let mut state = self.inner.state.lock().unwrap();
        state.hidden = true;
        let pause = !state.settings.receiver.keep_when_hidden;
        if pause {
            state.receiver_requested = false;
            state.receiver_status = "paused";
            self.inner
                .jobs
                .set_network_accepting(state.settings.receiver.profile_id, false);
        }
        pause
    }
    pub fn window_shown(&self) {
        self.inner.state.lock().unwrap().hidden = false;
    }
    pub async fn leave(
        &self,
        revision: &str,
        generation: &str,
    ) -> Result<DeviceNetworkProjection, DeviceError> {
        let _lane = self.inner.lane.lock().await;
        self.check_target(revision, generation)?;
        self.pause_receiver().await?;
        self.stop_receiver_transport().await;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| DeviceError::Unavailable)?;
        state.cancellation.cancel();
        state.client = None;
        state.peers.clear();
        state.status = "disconnected";
        state.generation += 1;
        // Disconnect preserves the durable enrolled identity. Re-enrollment is never automatic.
        drop(state);
        Ok(self.projection_now())
    }
}
