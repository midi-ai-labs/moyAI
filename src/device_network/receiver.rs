use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::{DeviceError, DeviceNetworkService, VerifiedGrant, WeakDeviceNetwork};
use crate::mcp_publish::dispatch::{
    DeviceRequestAuthenticator, PublishCallError, PublishToolDispatcher,
};
use crate::mcp_publish::transport::PublishHttpServer;
use crate::mcp_publish::{PublishMode, PublishProfile, PublishTarget};
use crate::session::ProjectRepository;
use crate::storage::StoreBundle;

async fn published_target_name(
    store: &StoreBundle,
    target: &PublishTarget,
) -> Result<String, DeviceError> {
    let PublishTarget::Project {
        project_id,
        workspace_root,
    } = target
    else {
        return if matches!(target, PublishTarget::Temp {}) {
            Ok("temp".into())
        } else {
            Err(DeviceError::InvalidConfiguration)
        };
    };
    let store = store.clone();
    let project_id = *project_id;
    let workspace_root = workspace_root.clone();
    tokio::task::spawn_blocking(move || {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|_| DeviceError::Unavailable)?
            .block_on(async move {
                let project = store
                    .project_repo()
                    .get_project(project_id)
                    .await
                    .map_err(|_| DeviceError::InvalidConfiguration)?;
                if project.root_path != workspace_root {
                    return Err(DeviceError::InvalidConfiguration);
                }
                // Public profile names have a smaller bound than local project names.
                let mut label = String::new();
                for character in project.display_name.chars().take(80) {
                    if label.len() + character.len_utf8() > 256 {
                        break;
                    }
                    label.push(character);
                }
                Ok(label)
            })
    })
    .await
    .map_err(|_| DeviceError::Unavailable)?
}

pub(super) struct ManagedReceiver {
    server: PublishHttpServer,
    dispatcher: Arc<dyn PublishToolDispatcher>,
    scope_id: String,
    endpoint: String,
    certificate_sha256: String,
}

async fn start_listener(
    ip: Ipv4Addr,
    port: Option<u16>,
    dispatcher: Arc<dyn PublishToolDispatcher>,
    tls: tokio_rustls::TlsAcceptor,
    auth: Arc<dyn DeviceRequestAuthenticator>,
) -> Result<PublishHttpServer, DeviceError> {
    let result = PublishHttpServer::start_managed(
        SocketAddr::from((ip, port.unwrap_or(7332))),
        dispatcher.clone(),
        4,
        tls.clone(),
        auth.clone(),
    )
    .await;
    match result {
        Ok(server) => Ok(server),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && port.is_none() => {
            PublishHttpServer::start_managed(SocketAddr::from((ip, 0)), dispatcher, 4, tls, auth)
                .await
                .map_err(|_| DeviceError::Unavailable)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            Err(DeviceError::ReceiverPortInUse)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrNotAvailable => {
            Err(DeviceError::ReceiverAddressUnavailable)
        }
        Err(_) => Err(DeviceError::Unavailable),
    }
}

struct ReceiverAuthentication {
    network: WeakDeviceNetwork,
}
#[async_trait]
impl DeviceRequestAuthenticator for ReceiverAuthentication {
    async fn authenticate(
        &self,
        token: &str,
        actor_certificate_sha256: &str,
        action: &str,
    ) -> Result<VerifiedGrant, PublishCallError> {
        let network = self
            .network
            .upgrade()
            .ok_or(PublishCallError::Unavailable)?;
        let client = network
            .client()
            .map_err(|_| PublishCallError::Unavailable)?;
        let grant = client
            .introspect(token, actor_certificate_sha256, action)
            .await
            .map_err(|_| PublishCallError::Unavailable)?;
        let state = network
            .inner
            .state
            .lock()
            .map_err(|_| PublishCallError::Unavailable)?;
        if state.closing
            || state.settings.hub_id.as_ref() != Some(&grant.claims().hub_id)
            || grant.claims().profile_id != state.settings.receiver.profile_id.0.to_string()
            || (action == "execute"
                && (state.receiver_status != "receiving" || !state.receiver_requested))
        {
            return Err(PublishCallError::Unavailable);
        }
        Ok(grant)
    }
}

impl DeviceNetworkService {
    pub(super) async fn start_receiver(&self) -> Result<(), DeviceError> {
        let result = self.start_receiver_inner().await;
        if let Err(error) = result {
            let profile = {
                let mut state = self.inner.state.lock().unwrap();
                if !state.closing {
                    state.receiver_status = "error";
                    state.error = Some(error);
                }
                state.settings.receiver.profile_id
            };
            self.inner.jobs.set_network_accepting(profile, false);
            if self.inner.receiver.lock().await.is_none() {
                self.inner.jobs.cancel_profile(profile);
            }
        }
        result
    }
    async fn start_receiver_inner(&self) -> Result<(), DeviceError> {
        self.refresh_certificate().await?;
        if self.receiver_certificate_changed().await {
            self.restart_receiver_certificate().await?;
        }
        let client = self.client()?;
        let settings = self.inner.state.lock().unwrap().settings.clone();
        if !settings.receiver.enabled || !settings.receiver.confirmed {
            return Err(DeviceError::ConfirmationRequired);
        }
        if self.inner.receiver.lock().await.is_none() {
            self.inner.state.lock().unwrap().receiver_status = "starting";
            let name = published_target_name(&self.inner.store, &settings.receiver.target).await?;
            let mut profile = PublishProfile::new(name, settings.receiver.target.clone());
            profile.id = settings.receiver.profile_id;
            profile.mode = PublishMode::Agent {
                access_mode: settings.receiver.access_mode,
            };
            let mut config = self.inner.global_config.lock().unwrap().clone();
            if let Some(route) = self.receiver_model_route(CancellationToken::new()).await? {
                config = route.runtime_config(&config);
                route.finish().await;
            }
            let dispatcher = self
                .inner
                .jobs
                .dispatcher_network(
                    profile,
                    config,
                    self.inner.protected.clone(),
                    self.downgrade(),
                )
                .await
                .map_err(|_| DeviceError::InvalidConfiguration)?;
            let scope_id = self
                .inner
                .jobs
                .network_scope_id(settings.receiver.profile_id)
                .map_err(|_| DeviceError::InvalidConfiguration)?;
            let receiver = self.bind_receiver(dispatcher, scope_id).await?;
            *self.inner.receiver.lock().await = Some(receiver);
        }
        self.inner
            .jobs
            .set_network_accepting(settings.receiver.profile_id, false);
        if let Err(error) = self.announce_receiver_with(&client).await {
            let mut state = self.inner.state.lock().unwrap();
            state.receiver_status = "error";
            state.error = Some(error);
            return Err(error);
        }
        Ok(())
    }

    async fn bind_receiver(
        &self,
        dispatcher: Arc<dyn PublishToolDispatcher>,
        scope_id: String,
    ) -> Result<ManagedReceiver, DeviceError> {
        let client = self.client()?;
        let ip = client.route_ip().await?;
        let (certificate, identity, ca, port, certificate_sha256) = {
            let state = self.inner.state.lock().unwrap();
            (
                state
                    .settings
                    .certificate_pem
                    .clone()
                    .ok_or(DeviceError::InvalidIdentity)?,
                state.identity.clone().ok_or(DeviceError::InvalidIdentity)?,
                state.shared.ca_certificate_pem.clone(),
                state.settings.receiver.port,
                state
                    .settings
                    .certificate_sha256
                    .clone()
                    .ok_or(DeviceError::InvalidIdentity)?,
            )
        };
        if !super::identity::certificate_covers_ip(&certificate, ip)? {
            return Err(DeviceError::InvalidIdentity);
        }
        let tls = crate::mcp_publish::tls::load_mtls_acceptor(
            &certificate,
            identity.private_key_pem(),
            &ca,
        )
        .map_err(|_| DeviceError::InvalidIdentity)?;
        let auth = Arc::new(ReceiverAuthentication {
            network: self.downgrade(),
        });
        let server = start_listener(ip, port, dispatcher.clone(), tls, auth).await?;
        let endpoint = server.endpoint();
        self.inner.state.lock().unwrap().receiver_endpoint = Some(endpoint.clone());
        Ok(ManagedReceiver {
            server,
            dispatcher,
            scope_id,
            endpoint,
            certificate_sha256,
        })
    }

    pub(super) async fn announce_receiver_with(
        &self,
        client: &super::DeviceClient,
    ) -> Result<(), DeviceError> {
        let owner = self.inner.receiver.lock().await;
        let Some(receiver) = owner.as_ref() else {
            return Ok(());
        };
        let (id, target, enabled, certificate) = {
            let state = self.inner.state.lock().unwrap();
            (
                state.settings.receiver.profile_id,
                state.settings.receiver.target.clone(),
                state.receiver_requested
                    && state.status == "active"
                    && !state.closing
                    && (!state.hidden || state.settings.receiver.keep_when_hidden),
                state.settings.certificate_sha256.clone(),
            )
        };
        if !receiver.server.snapshot().accepting
            || certificate.as_deref() != Some(&receiver.certificate_sha256)
        {
            return Err(DeviceError::Unavailable);
        }
        let name = published_target_name(&self.inner.store, &target).await?;
        client
            .publish(
                &id.0.to_string(),
                &name,
                &receiver.endpoint,
                &receiver.scope_id,
                enabled,
            )
            .await?;
        let mut state = self.inner.state.lock().unwrap();
        // Local OFF/close may win while the Hub acknowledgement is in flight.
        let accepted = enabled
            && state.status == "active"
            && state.receiver_requested
            && !state.closing
            && (!state.hidden || state.settings.receiver.keep_when_hidden);
        self.inner.jobs.set_network_accepting(id, accepted);
        state.receiver_status = if accepted { "receiving" } else { "paused" };
        Ok(())
    }

    pub(super) async fn pause_receiver(&self) -> Result<(), DeviceError> {
        self.inner.state.lock().unwrap().receiver_requested = false;
        let (id, target) = {
            let state = self.inner.state.lock().unwrap();
            (
                state.settings.receiver.profile_id,
                state.settings.receiver.target.clone(),
            )
        };
        self.inner.jobs.set_network_accepting(id, false);
        self.inner.state.lock().unwrap().receiver_status = "paused";
        let receiver = self.inner.receiver.lock().await;
        if let (Some(receiver), Ok(client)) = (receiver.as_ref(), self.client()) {
            let name = published_target_name(&self.inner.store, &target).await?;
            // Listener remains available for authenticated status/cancel of existing jobs.
            client
                .publish(
                    &id.0.to_string(),
                    &name,
                    &receiver.endpoint,
                    &receiver.scope_id,
                    false,
                )
                .await?;
        }
        Ok(())
    }

    pub(super) async fn stop_receiver_transport(&self) {
        let mut owner = self.inner.receiver.lock().await;
        if let Some(receiver) = owner.as_mut() {
            if !receiver.server.stop().await {
                self.inner.state.lock().unwrap().receiver_status = "stopping";
                return;
            }
        }
        owner.take();
        let mut state = self.inner.state.lock().unwrap();
        state.receiver_status = "stopped";
        state.receiver_endpoint = None;
    }

    pub(super) async fn restart_receiver_certificate(&self) -> Result<(), DeviceError> {
        let mut owner = self.inner.receiver.lock().await;
        let Some(old) = owner.as_mut() else {
            return Ok(());
        };
        self.inner.jobs.set_network_accepting(
            self.inner
                .state
                .lock()
                .unwrap()
                .settings
                .receiver
                .profile_id,
            false,
        );
        if !old.server.stop().await {
            return Err(DeviceError::ReceiverBusy);
        }
        let dispatcher = old.dispatcher.clone();
        let scope_id = old.scope_id.clone();
        *owner = Some(self.bind_receiver(dispatcher, scope_id).await?);
        drop(owner);
        self.announce_receiver_with(&self.client()?).await
    }

    pub(super) async fn receiver_certificate_changed(&self) -> bool {
        let owner = self.inner.receiver.lock().await;
        owner.as_ref().is_some_and(|receiver| {
            self.inner
                .state
                .lock()
                .unwrap()
                .settings
                .certificate_sha256
                .as_deref()
                != Some(receiver.certificate_sha256.as_str())
        })
    }

    pub(crate) async fn receiver_model_route(
        &self,
        cancel: CancellationToken,
    ) -> Result<Option<crate::hub::HubTurnRoute>, DeviceError> {
        let mode = self
            .inner
            .state
            .lock()
            .unwrap()
            .settings
            .receiver
            .model_mode;
        if mode == crate::hub::HubRouteMode::Direct {
            return Ok(None);
        }
        let client = self.client()?;
        crate::hub::HubConnection::device_worker_route(&client.endpoint(), client.http(), cancel)
            .await
            .map(Some)
            .map_err(|_| DeviceError::Unavailable)
    }

    pub(super) fn cancel_announced_lineages(&self, lineages: &[super::client::CancelledLineage]) {
        self.inner.jobs.cancel_network_lineages(
            lineages
                .iter()
                .map(|row| (row.origin_device_id.clone(), row.root_task_id.clone()))
                .collect(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{SqliteStore, StoragePaths};
    use camino::Utf8PathBuf;

    struct InactiveDispatcher;
    #[async_trait]
    impl PublishToolDispatcher for InactiveDispatcher {
        fn tool_descriptors(&self) -> Vec<serde_json::Value> {
            vec![]
        }
        async fn call(
            &self,
            _: &str,
            _: serde_json::Value,
            _: CancellationToken,
        ) -> Result<serde_json::Value, PublishCallError> {
            Err(PublishCallError::Unavailable)
        }
    }
    struct DenyAuthentication;
    #[async_trait]
    impl DeviceRequestAuthenticator for DenyAuthentication {
        async fn authenticate(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<VerifiedGrant, PublishCallError> {
            Err(PublishCallError::Unavailable)
        }
    }

    #[tokio::test]
    async fn explicit_receiver_port_conflicts_never_fall_back_but_auto_ports_can_coexist() {
        let pair = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let tls = crate::mcp_publish::tls::load_mtls_acceptor(
            &pair.cert.pem(),
            &pair.signing_key.serialize_pem(),
            &pair.cert.pem(),
        )
        .unwrap();
        let dispatcher: Arc<dyn PublishToolDispatcher> = Arc::new(InactiveDispatcher);
        let auth: Arc<dyn DeviceRequestAuthenticator> = Arc::new(DenyAuthentication);
        let mut first = start_listener(
            Ipv4Addr::LOCALHOST,
            None,
            dispatcher.clone(),
            tls.clone(),
            auth.clone(),
        )
        .await
        .unwrap();
        let port = reqwest::Url::parse(&first.endpoint())
            .unwrap()
            .port()
            .unwrap();
        assert!(matches!(
            start_listener(
                Ipv4Addr::LOCALHOST,
                Some(port),
                dispatcher.clone(),
                tls.clone(),
                auth.clone()
            )
            .await,
            Err(DeviceError::ReceiverPortInUse)
        ));
        let mut second = start_listener(
            Ipv4Addr::LOCALHOST,
            None,
            dispatcher.clone(),
            tls.clone(),
            auth.clone(),
        )
        .await
        .unwrap();
        assert_ne!(first.endpoint(), second.endpoint());
        assert!(first.snapshot().accepting);
        assert!(first.stop().await);
        let mut fixed = start_listener(Ipv4Addr::LOCALHOST, Some(port), dispatcher, tls, auth)
            .await
            .unwrap();
        assert_eq!(
            reqwest::Url::parse(&fixed.endpoint()).unwrap().port(),
            Some(port)
        );
        assert!(fixed.stop().await);
        assert!(second.stop().await);
    }

    #[tokio::test]
    async fn published_target_names_follow_the_registered_project_without_exposing_its_path() {
        let directory = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(directory.path().to_owned()).unwrap();
        let paths = StoragePaths {
            data_dir: root.join("data"),
            database_path: root.join("data/db.sqlite3"),
            truncation_dir: root.join("data/output"),
        };
        let sqlite = SqliteStore::open(&paths).unwrap();
        sqlite.migrate().unwrap();
        let store = StoreBundle::new(sqlite);
        assert_eq!(
            published_target_name(&store, &PublishTarget::Temp {})
                .await
                .unwrap(),
            "temp"
        );
        let project_id = crate::session::ProjectId::new();
        let workspace_root = root.join("private-parent/workspace");
        let target = PublishTarget::Project {
            project_id,
            workspace_root: workspace_root.clone(),
        };
        store
            .project_repo()
            .upsert_project(project_id, &workspace_root, "設計プロジェクト", "none")
            .await
            .unwrap();
        assert_eq!(
            published_target_name(&store, &target).await.unwrap(),
            "設計プロジェクト"
        );

        // Re-announcement reads current display metadata with the same target identity.
        store
            .project_repo()
            .upsert_project(project_id, &workspace_root, "実装プロジェクト", "none")
            .await
            .unwrap();
        assert_eq!(
            published_target_name(&store, &target).await.unwrap(),
            "実装プロジェクト"
        );
        let wrong_root = PublishTarget::Project {
            project_id,
            workspace_root: root.join("other-workspace"),
        };
        assert_eq!(
            published_target_name(&store, &wrong_root).await,
            Err(DeviceError::InvalidConfiguration)
        );

        // The Hub and local profile bounds apply without cutting a UTF-8 character.
        store
            .project_repo()
            .upsert_project(project_id, &workspace_root, &"設計🦉".repeat(100), "none")
            .await
            .unwrap();
        let name = published_target_name(&store, &target).await.unwrap();
        assert!(name.chars().count() <= 80);
        assert!(name.len() <= 256);
        assert!("設計🦉".repeat(100).starts_with(&name));
    }
}
