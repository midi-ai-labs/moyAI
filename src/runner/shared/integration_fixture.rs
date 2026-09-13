//! Real Hub enrollment for isolated integration fixtures. Credentials stay in their existing owners.
use camino::{Utf8Path, Utf8PathBuf};

use crate::device_network::{DeviceNetworkService, DeviceSettingsStore, SharedHubConfig};
use crate::storage::{SqliteStore, StoragePaths, StoreBundle};

use super::super::RunnerError;

pub(super) struct EnrolledFixture {
    pub device_id: String,
    pub http: reqwest::Client,
}

/// The caller supplies a fresh fixture config containing model settings and no prior
/// device-network trust. Enrollment is an actual HTTPS call with the Hub invitation.
pub(super) async fn enroll(
    config_path: &Utf8Path,
    data_dir: &Utf8Path,
    workspace: &Utf8Path,
    shared: SharedHubConfig,
    expected_hub_id: &str,
    invitation_code: &str,
) -> Result<EnrolledFixture, RunnerError> {
    let config = crate::tui::config_editor::save_device_network_config(
        config_path,
        &SharedHubConfig::default(),
        &shared,
        |_| Ok(()),
    )
    .map_err(RunnerError::new)?;
    let paths = StoragePaths {
        data_dir: data_dir.to_owned(),
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
    };
    let sqlite = SqliteStore::open(&paths).map_err(|error| RunnerError::new(error.to_string()))?;
    sqlite
        .migrate()
        .map_err(|error| RunnerError::new(error.to_string()))?;
    let directory = config_path.with_file_name("device-network");
    let service = DeviceNetworkService::for_workspace(
        directory.clone(),
        Utf8PathBuf::from(workspace),
        StoreBundle::new(sqlite),
        config,
    )
    .await
    .map_err(|error| RunnerError::new(error.to_string()))?;
    let initial = service.projection_now();
    let enrollment = service
        .join(
            invitation_code.into(),
            true,
            &initial.revision,
            &initial.generation,
        )
        .await;
    let result = match enrollment {
        Ok(_) => {
            let settings = DeviceSettingsStore::new(directory.join("device.json"))
                .load()
                .map_err(|error| RunnerError::new(error.to_string()));
            settings.and_then(|settings| {
                if settings.hub_id.as_deref() != Some(expected_hub_id) {
                    return Err(RunnerError::new("Fixture joined a different Hub identity"));
                }
                let device_id = settings
                    .device_id
                    .ok_or_else(|| RunnerError::new("Fixture enrollment returned no device ID"))?;
                let http = service
                    .client()
                    .map_err(|error| RunnerError::new(error.to_string()))?
                    .http()
                    .snapshot();
                Ok(EnrolledFixture { device_id, http })
            })
        }
        Err(error) => Err(RunnerError::new(error.to_string())),
    };
    service.shutdown().await;
    result
}
