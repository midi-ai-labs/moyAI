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
impl PreparedDeviceConfiguration {
    pub fn endpoint_changed(&self) -> bool {
        self.authenticated.is_some()
    }
}

impl DeviceNetworkService {
    /// Verify the candidate, then stop a proven idle Runner before persistence.
    /// Call without holding the Desktop controller.
    pub async fn prepare_configuration(
        &self,
        mut shared: SharedHubConfig,
        revision: &str,
        generation: &str,
    ) -> Result<PreparedDeviceConfiguration, DeviceError> {
        shared.validate()?;
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
