//! A registered endpoint move proves the existing Hub/device before changing local trust.
use super::*;
use crate::device_network::client::{DeviceAdmission, SelfStatus};

#[cfg(test)]
mod tests;

pub struct PreparedDeviceConfiguration {
    shared: SharedHubConfig,
    previous: SharedHubConfig,
    revision: String,
    generation: String,
    authenticated: Option<(DeviceClient, SelfStatus)>,
    _execution: tokio::sync::OwnedMutexGuard<()>,
}
pub struct PreparedDeviceReset {
    revision: String,
    generation: String,
    _execution: tokio::sync::OwnedMutexGuard<()>,
}
impl PreparedDeviceConfiguration {
    pub fn endpoint_changed(&self) -> bool {
        self.authenticated.is_some()
    }
}

impl DeviceNetworkService {
    pub(crate) fn connection_locally_retired(&self) -> Result<bool, DeviceError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| DeviceError::Unavailable)?;
        let (Some(hub), Some(device)) = (&state.settings.hub_id, &state.settings.device_id) else {
            return Ok(true);
        };
        super::super::reset::execution_identity_reset(&self.inner.directory, hub, device)
    }
    pub fn effective_shared_config(&self) -> SharedHubConfig {
        self.inner.state.lock().unwrap().shared.clone()
    }

    /// Reset only this PC. No old Hub request, remote settlement or process-drain
    /// acknowledgement is a precondition. The durable retirement fences old Runners.
    pub async fn reset_local(
        &self,
        revision: &str,
        generation: &str,
        commit: impl FnOnce(&SharedHubConfig) -> Result<(), DeviceError>,
    ) -> Result<DeviceNetworkProjection, DeviceError> {
        let prepared = self.prepare_reset(revision, generation).await?;
        self.reset_local_prepared(prepared, commit).await
    }

    /// Acquire execution ownership before the Desktop controller, matching import.
    pub async fn prepare_reset(
        &self,
        revision: &str,
        generation: &str,
    ) -> Result<PreparedDeviceReset, DeviceError> {
        self.check_target(revision, generation)?;
        let _execution = self.inner.execution.lane.clone().lock_owned().await;
        self.check_target(revision, generation)?;
        Ok(PreparedDeviceReset {
            revision: revision.into(),
            generation: generation.into(),
            _execution,
        })
    }

    pub async fn reset_local_prepared(
        &self,
        prepared: PreparedDeviceReset,
        commit: impl FnOnce(&SharedHubConfig) -> Result<(), DeviceError>,
    ) -> Result<DeviceNetworkProjection, DeviceError> {
        let _lane = self.inner.lane.lock().await;
        self.check_target(&prepared.revision, &prepared.generation)?;
        let mut reset = super::super::reset::ResetState::load(&self.inner.directory)?;
        {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| DeviceError::Unavailable)?;
            reset.retire(
                state.settings.hub_id.as_deref(),
                state.settings.device_id.as_deref(),
            );
        }
        let installed =
            crate::runner::operations::OperationsStore::open(&self.inner.store.paths().data_dir)
                .map_err(|_| DeviceError::Storage)?
                .installed;
        if let Some(settings) = &installed.settings {
            reset.retire(Some(&settings.hub_id), Some(&settings.device_id));
            reset.execution_review_required = true;
        }
        reset.save(&self.inner.directory)?;
        // From here, even a crash cannot restore the retired registration.
        self.clear_local_registration()?;
        self.inner.shared_work.reset_connection();
        let hub = self
            .inner
            .hub
            .lock()
            .map_err(|_| DeviceError::Unavailable)?
            .clone();
        if let Some(hub) = hub {
            hub.reset_local().await.map_err(|_| DeviceError::Storage)?;
        }
        self.stop_receiver_transport().await;
        commit(&SharedHubConfig::default())?;
        reset.pending = false;
        reset.save(&self.inner.directory)?;
        Ok(self.projection_now())
    }

    fn clear_local_registration(&self) -> Result<(), DeviceError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| DeviceError::Unavailable)?;
        state.cancellation.cancel();
        // The retired poller keeps its cancelled token; later registration on
        // this service must start from a fresh lifecycle before its first HTTP.
        state.cancellation = CancellationToken::new();
        state.client = None;
        state.identity = None;
        state.pending_join = None;
        state.peers.clear();
        state.shared = SharedHubConfig::default();
        state.status = "unconfigured";
        state.receiver_requested = false;
        state.receiver_status = "stopped";
        state.receiver_endpoint = None;
        state.generation += 1;
        self.inner
            .jobs
            .cancel_profile(state.settings.receiver.profile_id);
        self.inner.outgoing.peer_connections.clear();
        let next = DeviceSettings {
            revision: state.settings.revision.clone(),
            ..DeviceSettings::default()
        };
        // Fence the reviewed durable registration before retiring its private key.
        // A Runner certificate renewal may have advanced this revision on disk.
        state.settings = match self.inner.settings.save(&next) {
            Ok(saved) => saved,
            Err(DeviceError::SettingsChanged) => {
                state.settings = self.inner.settings.load()?;
                return Err(DeviceError::SettingsChanged);
            }
            Err(error) => return Err(error),
        };
        self.inner.identity.remove()?;
        // The CSR embeds the retired key even when the next registration uses
        // the same LAN address. Neither enrollment cache can cross this reset.
        for name in ["pending-join.json", "pending-csr.json"] {
            match std::fs::remove_file(self.inner.directory.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(DeviceError::Storage),
            }
        }
        state.error = None;
        Ok(())
    }

    /// Verify the candidate, then stop a proven idle Runner before persistence.
    /// Call without holding the Desktop controller.
    pub async fn prepare_configuration(
        &self,
        mut shared: SharedHubConfig,
        revision: &str,
        generation: &str,
    ) -> Result<PreparedDeviceConfiguration, DeviceError> {
        shared.validate()?;
        // An interrupted reset must finish its local writes before a new import.
        if super::super::reset::ResetState::load(&self.inner.directory)?.pending {
            self.check_target(revision, generation)?;
            return Err(DeviceError::ResetIncomplete);
        }
        let (previous, registered) = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| DeviceError::Unavailable)?;
            state.check(revision, generation)?;
            let registered = if state.settings.device_id.is_some() && state.shared != shared {
                if !state.shared.same_ca(&shared)? {
                    return Err(DeviceError::DifferentHub);
                }
                // Keep the exact persisted CA representation: remembered login and
                // existing consent are bound to its hash, not imported whitespace.
                shared.ca_certificate_pem = state.shared.ca_certificate_pem.clone();
                if state.shared == shared {
                    None
                } else {
                    Some((
                        state.settings.clone(),
                        state.identity.clone().ok_or(DeviceError::InvalidIdentity)?,
                    ))
                }
            } else {
                None
            };
            (state.shared.clone(), registered)
        };
        let authenticated = if let Some((settings, identity)) = registered {
            self.require_idle_model_connection()?;
            let client = DeviceClient::new_from(
                &shared,
                Some((
                    &identity,
                    settings
                        .certificate_pem
                        .as_deref()
                        .ok_or(DeviceError::InvalidIdentity)?,
                )),
                settings
                    .device_id
                    .clone()
                    .ok_or(DeviceError::InvalidIdentity)?,
                settings.receiver.bind_ip,
            )?;
            // TLS pins the existing CA and verifies the Hub role and new hostname.
            // This GET neither re-registers the device nor changes its presence.
            let own = client.self_status().await?;
            if Some(&own.hub_id) != settings.hub_id.as_ref()
                || Some(&own.device_id) != settings.device_id.as_ref()
                || Some(&own.certificate_sha256) != settings.certificate_sha256.as_ref()
                || settings
                    .expires_at_ms
                    .as_deref()
                    .and_then(canonical_revision)
                    != Some(own.expires_at_ms)
                || canonical_revision(&own.revision).is_none()
            {
                return Err(DeviceError::DifferentHub);
            }
            Some((client, own))
        } else {
            None
        };
        self.check_target(revision, generation)?;
        // Existing execution polling and user operations share this lane. Keep it
        // until commit/failure so they cannot auto-start the old configuration.
        let execution = self.inner.execution.lane.clone().lock_owned().await;
        self.check_target(revision, generation)?;
        if authenticated.is_some() {
            self.quiesce_endpoint_change().await?;
        }
        Ok(PreparedDeviceConfiguration {
            shared,
            previous,
            revision: revision.into(),
            generation: generation.into(),
            authenticated,
            _execution: execution,
        })
    }

    fn require_idle_model_connection(&self) -> Result<(), DeviceError> {
        if self
            .inner
            .hub
            .lock()
            .map_err(|_| DeviceError::Unavailable)?
            .as_ref()
            .is_some_and(|hub| hub.has_active_turns())
        {
            return Err(DeviceError::EndpointChangeBusy);
        }
        Ok(())
    }

    /// Commit only a verified candidate whose original target still owns state.
    pub async fn commit_configuration(
        &self,
        prepared: PreparedDeviceConfiguration,
        commit: impl FnOnce(&SharedHubConfig) -> Result<(), DeviceError>,
    ) -> Result<DeviceNetworkProjection, DeviceError> {
        let _lane = self.inner.lane.lock().await;
        if prepared.endpoint_changed() {
            self.require_idle_model_connection()?;
        }
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| DeviceError::Unavailable)?;
        state.check(&prepared.revision, &prepared.generation)?;
        if state.shared != prepared.previous {
            return Err(DeviceError::ConnectionChanged);
        }
        commit(&prepared.shared)?;
        if state.shared != prepared.shared {
            state.pending_join = None;
            self.inner.outgoing.peer_connections.clear();
        }
        state.shared = prepared.shared;
        state.generation += 1;
        state.error = None;
        if let Some((client, own)) = prepared.authenticated {
            state.client = Some(client);
            state.peers.clear();
            state.status = if own.admission == DeviceAdmission::Stopped {
                "stopped"
            } else {
                "active"
            };
        } else {
            state.status = if state.settings.device_id.is_some() {
                "disconnected"
            } else {
                "not_enrolled"
            };
        }
        drop(state);
        Ok(self.projection_now())
    }

    pub async fn reconnect_model_after_endpoint_change(&self) {
        self.adopt_model_session(true).await;
    }
}
