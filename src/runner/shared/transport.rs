use super::super::RunnerError;
use super::{Assignment, AttemptStatus, Job, Report, SharedSettings};
use crate::device_network::{DeviceClient, DeviceIdentityStore, DeviceSettingsStore};
use futures_util::StreamExt;
use serde::de::DeserializeOwned;

#[derive(Debug)]
pub(crate) enum TransportError {
    Unavailable,
    InvalidResponse,
    PreparationReadTimedOut,
    Rejected(reqwest::StatusCode),
}

impl From<TransportError> for RunnerError {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::Unavailable => {
                Self::new("Shared Hub is unavailable; execution state has not been retried")
            }
            TransportError::InvalidResponse => Self::new("Shared Hub returned an invalid response"),
            TransportError::PreparationReadTimedOut => Self::new(
                "Shared Hub input/archive read did not complete within 10 seconds; Agent execution was not started",
            ),
            TransportError::Rejected(status) => Self::new(format!(
                "Shared Hub rejected the request (HTTP {})",
                status.as_u16()
            )),
        }
    }
}

#[derive(Clone)]
pub(crate) struct SharedClient {
    client: DeviceClient,
    network: Option<crate::device_network::DeviceNetworkService>,
}

impl SharedClient {
    pub(crate) async fn model_route(
        &self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::hub::HubTurnRoute, RunnerError> {
        if self.connection_retired()? {
            return Err(RunnerError::new(
                "Hub connection was reset before preparing the model",
            ));
        }
        crate::hub::HubConnection::device_worker_route(
            &self.client.endpoint(),
            self.client.http(),
            cancel,
        )
        .await
        .map_err(|error| RunnerError::new(error.to_string()))
    }

    pub(crate) fn effect_authority(
        &self,
        assignment: Assignment,
    ) -> std::sync::Arc<dyn crate::runtime::ExternalEffectAuthority> {
        std::sync::Arc::new(EffectAuthority {
            client: self.clone(),
            assignment,
            runtime: tokio::runtime::Handle::current(),
        })
    }

    pub(crate) async fn authorize(
        &self,
        assignment: &Assignment,
        approval: Option<String>,
    ) -> Result<(), RunnerError> {
        let result: serde_json::Value = self.request(
            &format!("/v1/shared/runner/attempts/{}/authorize", assignment.attempt_id),
            Some(&serde_json::json!({"generation":assignment.generation,"authority_generation":assignment.authority_generation,"approval_id":approval})),
        ).await?;
        if result
            .get("authorized")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return Err(RunnerError::new("Hub did not authorize this effect"));
        }
        if self.connection_retired()? {
            return Err(RunnerError::new(
                "Hub connection was reset before this effect",
            ));
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn for_test(client: DeviceClient) -> Self {
        Self {
            client,
            network: None,
        }
    }

    pub(crate) fn load(
        settings: &SharedSettings,
        host: &super::super::RunnerHost,
    ) -> Result<Self, RunnerError> {
        let config_path = crate::config::loader::global_config_path()
            .map_err(|e| RunnerError::new(e.to_string()))?;
        let directory = config_path.with_file_name("device-network");
        let device = DeviceSettingsStore::new(directory.join("device.json"))
            .load()
            .map_err(|e| RunnerError::new(e.to_string()))?;
        if device.hub_id.as_deref() != Some(&settings.hub_id)
            || device.device_id.as_deref() != Some(&settings.device_id)
        {
            return Err(RunnerError::new(
                "Shared settings do not match this account's enrolled Hub/device identity",
            ));
        }
        if crate::device_network::reset::execution_identity_reset(
            &directory,
            &settings.hub_id,
            &settings.device_id,
        )
        .map_err(|e| RunnerError::new(e.to_string()))?
        {
            return Err(RunnerError::new(
                "This Hub connection was reset locally; enroll again and confirm new execution permission",
            ));
        }
        let identity = DeviceIdentityStore::new(directory.join("identity.json"))
            .load()
            .map_err(|e| RunnerError::new(e.to_string()))?
            .ok_or_else(|| RunnerError::new("An enrolled device identity is required"))?;
        let config =
            crate::config::ConfigLoader::load(config_path.parent().unwrap_or(&config_path), None)
                .map_err(|e| RunnerError::new(e.to_string()))?;
        let certificate = device
            .certificate_pem
            .as_deref()
            .ok_or_else(|| RunnerError::new("An enrolled device certificate is required"))?;
        let client = DeviceClient::new(
            &config.device_network,
            Some((&identity, certificate)),
            settings.device_id.clone(),
        )
        .map_err(|e| RunnerError::new(e.to_string()))?;
        let network = crate::device_network::DeviceNetworkService::for_runner_transport(
            directory,
            host.inner.process.clone(),
            config,
            client.clone(),
        )
        .map_err(|e| RunnerError::new(e.to_string()))?;
        Ok(Self {
            client,
            network: Some(network),
        })
    }

    pub(crate) async fn shutdown(&self) {
        if let Some(network) = &self.network {
            network.shutdown().await;
        }
    }

    pub(super) fn connection_retired(&self) -> Result<bool, RunnerError> {
        let Some(network) = &self.network else {
            return Ok(false);
        };
        network
            .connection_locally_retired()
            .map_err(|e| RunnerError::new(e.to_string()))
    }

    pub(crate) async fn assignments(
        &self,
        settings: &SharedSettings,
    ) -> Result<Vec<Assignment>, RunnerError> {
        // A device can host separate configured environments. This poll confirms only the
        // environments owned by this Runner, including an explicitly empty initial mapping.
        let environments = settings
            .environments
            .iter()
            .map(|mapping| mapping.environment_id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        self.request(
            &format!("/v1/shared/runner/assignments?environment_ids={environments}"),
            None,
        )
        .await
        .map_err(Into::into)
    }

    pub(crate) async fn claim(
        &self,
        settings: &SharedSettings,
    ) -> Result<Option<Assignment>, RunnerError> {
        let ids = settings
            .environments
            .iter()
            .map(|mapping| &mapping.environment_id)
            .collect::<Vec<_>>();
        self.request(
            "/v1/shared/runner/claim",
            Some(&serde_json::json!({"environment_ids": ids})),
        )
        .await
        .map_err(Into::into)
    }

    pub(crate) async fn attempt(&self, id: &str) -> Result<AttemptStatus, RunnerError> {
        if !crate::device_network::stable_id(id) {
            return Err(RunnerError::new("Invalid Hub attempt ID"));
        }
        self.request(&format!("/v1/shared/runner/attempts/{id}"), None)
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn report(&self, report: &Report) -> Result<Job, TransportError> {
        self.request(
            "/v1/shared/runner/report",
            Some(&serde_json::to_value(report).map_err(|_| TransportError::InvalidResponse)?),
        )
        .await
    }

    pub(crate) async fn consume_approval(
        &self,
        entry: &super::journal::Entry,
        id: &str,
    ) -> Result<Option<super::protocol::ApprovalConsumeResult>, RunnerError> {
        if !crate::device_network::stable_id(id) {
            return Err(RunnerError::new("Invalid approval identity"));
        }
        self.request(&format!("/v1/shared/runner/approvals/{id}/consume"), Some(&serde_json::json!({"attempt_id":entry.assignment.attempt_id,"generation":entry.assignment.generation})))
            .await.map_err(Into::into)
    }

    pub(super) async fn request<T: DeserializeOwned>(
        &self,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T, TransportError> {
        self.request_as(path, body, None).await
    }

    /// Data-slot pressure is temporary. Retry only this preparation read, never startup or
    /// a control mutation. The outer deadline also cancels a slow response in a later attempt.
    pub(super) async fn preparation_read<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T, TransportError> {
        use std::time::Duration;
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut delay = Duration::from_millis(100);
            loop {
                match self.request(path, None).await {
                    Err(TransportError::Rejected(reqwest::StatusCode::TOO_MANY_REQUESTS)) => {
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(Duration::from_millis(500));
                    }
                    result => return result,
                }
            }
        })
        .await
        .map_err(|_| TransportError::PreparationReadTimedOut)?
    }

    pub(super) async fn request_as<T: DeserializeOwned>(
        &self,
        path: &str,
        body: Option<&serde_json::Value>,
        human: Option<&str>,
    ) -> Result<T, TransportError> {
        if self
            .connection_retired()
            .map_err(|_| TransportError::Unavailable)?
        {
            return Err(TransportError::Rejected(reqwest::StatusCode::FORBIDDEN));
        }
        let url = format!("{}{path}", self.client.endpoint());
        let maximum = if path.contains("/assets") || path.ends_with("/archive") {
            90 * 1024 * 1024
        } else {
            4 * 1024 * 1024
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let lease = self.client.http().acquire().await;
            let request = match body {
                Some(body) => lease.http.post(url).json(body),
                None => lease.http.get(url),
            };
            let request = match human {
                Some(token) => request.bearer_auth(token),
                None => request,
            };
            let response = request
                .send()
                .await
                .map_err(|_| TransportError::Unavailable)?;
            let status = response.status();
            if status.is_redirection()
                || response
                    .content_length()
                    .is_some_and(|size| size > maximum as u64)
            {
                return Err(TransportError::InvalidResponse);
            }
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| TransportError::Unavailable)?;
                if bytes.len().saturating_add(chunk.len()) > maximum {
                    return Err(TransportError::InvalidResponse);
                }
                bytes.extend_from_slice(&chunk);
            }
            if !status.is_success() {
                return Err(if status.is_client_error() {
                    TransportError::Rejected(status)
                } else {
                    TransportError::Unavailable
                });
            }
            serde_json::from_slice(&bytes).map_err(|_| TransportError::InvalidResponse)
        })
        .await
        .map_err(|_| TransportError::Unavailable)?
    }
}

struct EffectAuthority {
    client: SharedClient,
    assignment: Assignment,
    runtime: tokio::runtime::Handle,
}
impl std::fmt::Debug for EffectAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedEffectAuthority")
            .field("attempt", &self.assignment.attempt_id)
            .finish()
    }
}
impl crate::runtime::ExternalEffectAuthority for EffectAuthority {
    fn authorize(&self, approval_id: Option<&str>) -> Result<(), String> {
        let client = self.client.clone();
        let assignment = self.assignment.clone();
        let approval = approval_id.map(str::to_owned);
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        // Agent execution is on its dedicated LocalTaskExecutor. Network progress remains on
        // the host runtime, so the synchronous final-effect fence never nests a Tokio runtime.
        self.runtime.spawn(async move {
            let _ = send.send(client.authorize(&assignment, approval).await);
        });
        receive
            .recv_timeout(std::time::Duration::from_secs(11))
            .map_err(|_| "Hub effect authorization did not complete".to_string())?
            .map_err(|error| error.message)
    }
}
