//! Headless use of the existing certificate and presence owner, without starting a receiver.
use super::*;

impl DeviceNetworkService {
    pub(crate) fn for_runner_transport(
        directory: Utf8PathBuf,
        process: crate::app::AppProcessRuntime,
        config: ResolvedConfig,
        client: DeviceClient,
    ) -> Result<Self, DeviceError> {
        let store = process.store();
        let jobs = RemoteJobService::new(process).map_err(|_| DeviceError::Unavailable)?;
        let publish = PublishService::new(
            directory.join("manual-publish.json"),
            store.clone(),
            config.clone(),
        );
        let service = Self::new(directory, store, config, jobs, publish);
        {
            let mut state = service
                .inner
                .state
                .lock()
                .map_err(|_| DeviceError::Unavailable)?;
            if state.settings.device_id.as_deref() != Some(&client.device_id) {
                return Err(DeviceError::InvalidIdentity);
            }
            state.client = Some(client);
            state.status = "active";
        }
        // Renewal rotates the same ManagedHubHttp carried by the Runner client; neither
        // credentials nor renewal rules gain another owner. This does not start a receiver.
        service.start_heartbeat();
        Ok(service)
    }
}
