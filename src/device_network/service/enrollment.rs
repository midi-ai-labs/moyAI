use std::io::{Read, Write};

use super::*;
use crate::device_network::client::{JoinRequestReceipt, JoinStatus};

/// Pending proof material is local only. The public projection exposes the request
/// locator and observed route, never the challenge, CSR or private identity.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PendingJoin {
    schema_version: u32,
    shared: SharedHubConfig,
    key_sha256: String,
    pub(super) receipt: JoinRequestReceipt,
    pub(super) ip: Ipv4Addr,
    csr_pem: String,
    #[serde(default)]
    status: JoinStatus,
}

pub(super) fn local_device_label() -> String {
    let value = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default();
    let mut label = String::new();
    for character in value
        .trim()
        .chars()
        .filter(|character| !character.is_control())
    {
        if label.len() + character.len_utf8() > 256 {
            break;
        }
        label.push(character);
    }
    if label.is_empty() {
        "moyAI Desktop".into()
    } else {
        label
    }
}

impl PendingJoin {
    pub(super) fn status(&self) -> &'static str {
        match self.status {
            JoinStatus::Stopped => "stopped",
            JoinStatus::Revoked => "revoked",
            JoinStatus::Expired => "expired",
            _ => "pending",
        }
    }
    fn validate(&self) -> Result<(), DeviceError> {
        self.shared.validate()?;
        if self.schema_version != 1
            || ![
                &self.receipt.hub_id,
                &self.receipt.request_id,
                &self.receipt.challenge,
            ]
            .iter()
            .all(|id| crate::device_network::stable_id(id))
            || self.receipt.expires_at_ms == 0
            || self.key_sha256.len() != 64
            || !self.key_sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || self.ip.is_unspecified()
            || self.ip.is_multicast()
            || self.csr_pem.len() > 16384
            || !self
                .csr_pem
                .starts_with("-----BEGIN CERTIFICATE REQUEST-----")
            || self.csr_pem.contains("PRIVATE KEY")
        {
            return Err(DeviceError::InvalidIdentity);
        }
        Ok(())
    }
    pub(super) fn load(
        directory: &camino::Utf8Path,
        shared: &SharedHubConfig,
        identity: Option<&DeviceIdentity>,
    ) -> Result<Option<Self>, DeviceError> {
        let file = match std::fs::File::open(directory.join("pending-join.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(DeviceError::Storage),
        };
        let mut bytes = Vec::new();
        file.take(131073)
            .read_to_end(&mut bytes)
            .map_err(|_| DeviceError::Storage)?;
        if bytes.len() > 131072 {
            return Err(DeviceError::InvalidIdentity);
        }
        let pending: Self =
            serde_json::from_slice(&bytes).map_err(|_| DeviceError::InvalidIdentity)?;
        pending.validate()?;
        if &pending.shared != shared {
            return Ok(None);
        }
        if identity
            .ok_or(DeviceError::InvalidIdentity)?
            .public_key_sha256()?
            != pending.key_sha256
        {
            return Err(DeviceError::InvalidIdentity);
        }
        Ok(Some(pending))
    }
    fn save(&self, directory: &camino::Utf8Path) -> Result<(), DeviceError> {
        self.validate()?;
        let mut file =
            tempfile::NamedTempFile::new_in(directory).map_err(|_| DeviceError::Storage)?;
        file.write_all(&serde_json::to_vec(self).map_err(|_| DeviceError::Storage)?)
            .map_err(|_| DeviceError::Storage)?;
        file.as_file()
            .sync_all()
            .map_err(|_| DeviceError::Storage)?;
        file.persist(directory.join("pending-join.json"))
            .map_err(|_| DeviceError::Storage)?;
        Ok(())
    }
}

impl DeviceNetworkService {
    /// Explicit import/retry starts one application-owned enrollment poller. It
    /// changes neither receiver authority nor existing Direct model preferences.
    pub async fn request_join(
        &self,
        revision: &str,
        generation: &str,
    ) -> Result<DeviceNetworkProjection, DeviceError> {
        self.check_target(revision, generation)?;
        let registered = self
            .inner
            .state
            .lock()
            .unwrap()
            .settings
            .device_id
            .is_some();
        if registered {
            return self.refresh().await;
        }
        let projection = self.refresh_join(Some((revision, generation))).await?;
        self.start_heartbeat();
        Ok(projection)
    }

    pub(super) async fn refresh_join(
        &self,
        expected: Option<(&str, &str)>,
    ) -> Result<DeviceNetworkProjection, DeviceError> {
        let lane = self.inner.lane.lock().await;
        let (shared, identity, pending, generation, cancel, bind_ip) = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| DeviceError::Unavailable)?;
            if let Some((revision, generation)) = expected {
                state.check(revision, generation)?;
            }
            if state.closing
                || state.settings.device_id.is_some()
                || state.status == "revoked"
                || state.error == Some(DeviceError::JoinSuperseded)
            {
                return Ok(self.projection_without_lock(state));
            }
            state.shared.validate()?;
            if matches!(
                state.error,
                Some(DeviceError::InvalidIdentity | DeviceError::SettingsCorrupt)
            ) {
                return Err(state.error.unwrap());
            }
            let identity = self.inner.identity.load_or_create()?;
            state.identity = Some(identity.clone());
            state.status = "pending";
            (
                state.shared.clone(),
                identity,
                state.pending_join.clone(),
                state.generation,
                state.cancellation.clone(),
                state.settings.receiver.bind_ip,
            )
        };
        let result = async {
            let client = DeviceClient::new_from(&shared, None, String::new(), bind_ip)?;
            let ip = client.route_ip().await?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| DeviceError::Unavailable)?
                .as_millis() as u64;
            let pending = match pending.filter(|pending| {
                pending.shared == shared && pending.ip == ip && pending.receipt.expires_at_ms > now
            }) {
                Some(pending) => pending,
                None => {
                    let csr_pem = self.stable_csr(&identity, ip)?;
                    let receipt = client.request_join(&local_device_label(), &csr_pem).await?;
                    let pending = PendingJoin {
                        schema_version: 1,
                        shared: shared.clone(),
                        key_sha256: identity.public_key_sha256()?,
                        receipt,
                        ip,
                        csr_pem,
                        status: JoinStatus::Pending,
                    };
                    pending.validate()?;
                    let mut state = self
                        .inner
                        .state
                        .lock()
                        .map_err(|_| DeviceError::Unavailable)?;
                    if state.closing || state.generation != generation || state.shared != shared {
                        return Err(DeviceError::ConnectionChanged);
                    }
                    pending.save(&self.inner.directory)?;
                    state.pending_join = Some(pending.clone());
                    pending
                }
            };
            let proof = identity.join_proof(
                ip,
                &pending.receipt.hub_id,
                &pending.receipt.request_id,
                &pending.receipt.challenge,
            )?;
            let response = client
                .join_status(&pending.receipt.request_id, &proof)
                .await?;
            if response.hub_id != pending.receipt.hub_id
                || response.request_id != pending.receipt.request_id
                || matches!(response.status, JoinStatus::Approved | JoinStatus::Stopped)
                    != response.certificate.is_some()
            {
                return Err(DeviceError::InvalidResponse);
            }
            if let Some(receipt) = response.certificate {
                if receipt.hub_id != pending.receipt.hub_id
                    || !super::super::identity::certificate_covers_ip(&receipt.certificate_pem, ip)?
                {
                    return Err(DeviceError::InvalidResponse);
                }
                self.adopt_receipt(receipt, &shared, &identity, &generation.to_string(), None)?;
                self.inner.state.lock().unwrap().pending_join = None;
                return Ok(true);
            }
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| DeviceError::Unavailable)?;
            if state.closing || state.generation != generation {
                return Err(DeviceError::ConnectionChanged);
            }
            let mut updated = pending;
            if updated.status != response.status {
                updated.status = response.status;
                updated.save(&self.inner.directory)?;
            }
            state.pending_join = Some(updated);
            state.error = None;
            state.status = match response.status {
                JoinStatus::Pending => "pending",
                JoinStatus::Stopped => "stopped",
                JoinStatus::Revoked => "revoked",
                JoinStatus::Expired => "expired",
                JoinStatus::Approved => unreachable!(),
            };
            Ok(false)
        };
        let outcome = tokio::select! { biased; _ = cancel.cancelled() => Err(DeviceError::ConnectionChanged), result = result => result };
        match outcome {
            Ok(true) => {
                drop(lane);
                // Adoption advances the generation. Replace the pending poller with
                // the existing authenticated heartbeat, then connect model metadata.
                self.start_heartbeat();
                self.refresh_connected().await
            }
            Ok(false) => Ok(self.projection_now()),
            Err(error) => {
                let mut state = self
                    .inner
                    .state
                    .lock()
                    .map_err(|_| DeviceError::Unavailable)?;
                if state.closing || state.generation != generation {
                    return Err(DeviceError::ConnectionChanged);
                }
                state.status = if error == DeviceError::Revoked {
                    "revoked"
                } else {
                    "error"
                };
                state.error = Some(error);
                if error == DeviceError::Revoked {
                    if let Some(pending) = state.pending_join.as_mut() {
                        pending.status = JoinStatus::Revoked;
                        pending.save(&self.inner.directory)?;
                    }
                }
                drop(state);
                // Keep persisted request/key and expose the error. The same poller
                // retries network failures without creating a second identity.
                Ok(self.projection_now())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn pending_join_reopens_with_the_same_key_and_never_adopts_another_hubs_request() {
        let (_temp, service) = super::super::lifecycle_tests::fixture().await;
        let identity = service.inner.identity.load_or_create().unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let shared = SharedHubConfig {
            hub_url: "https://localhost:9471".into(),
            ca_certificate_pem: cert.pem(),
        };
        let pending = PendingJoin {
            schema_version: 1,
            shared: shared.clone(),
            key_sha256: identity.public_key_sha256().unwrap(),
            ip: Ipv4Addr::LOCALHOST,
            receipt: JoinRequestReceipt {
                hub_id: "hub".into(),
                request_id: "request".into(),
                challenge: "challenge".into(),
                expires_at_ms: u64::MAX,
            },
            csr_pem: identity.csr(Ipv4Addr::LOCALHOST).unwrap(),
            status: JoinStatus::Pending,
        };
        pending.save(&service.inner.directory).unwrap();
        let restored = PendingJoin::load(&service.inner.directory, &shared, Some(&identity))
            .unwrap()
            .unwrap();
        assert_eq!(restored.receipt.request_id, pending.receipt.request_id);
        assert_eq!(restored.csr_pem, pending.csr_pem);
        let mut changed = shared.clone();
        changed.hub_url = "https://localhost:9472".into();
        assert!(
            PendingJoin::load(&service.inner.directory, &changed, Some(&identity))
                .unwrap()
                .is_none()
        );
        let other = super::super::lifecycle_tests::fixture().await;
        let other_identity = other.1.inner.identity.load_or_create().unwrap();
        assert!(matches!(
            PendingJoin::load(&service.inner.directory, &shared, Some(&other_identity)),
            Err(DeviceError::InvalidIdentity)
        ));
        let serialized =
            std::fs::read_to_string(service.inner.directory.join("pending-join.json")).unwrap();
        assert!(!serialized.contains("PRIVATE KEY"));
        assert!(
            identity
                .join_proof(Ipv4Addr::LOCALHOST, "hub", "request", "challenge")
                .unwrap()
                .contains("CERTIFICATE REQUEST")
        );
    }
}
