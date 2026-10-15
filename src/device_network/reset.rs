//! Local retirement survives an interrupted reset and never depends on the old Hub.
use std::io::{Read, Write};

use camino::Utf8Path;
use serde::{Deserialize, Serialize};

use super::DeviceError;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResetState {
    pub pending: bool,
    pub execution_review_required: bool,
    retired: Vec<(String, String)>,
}

impl ResetState {
    pub(crate) fn load(directory: &Utf8Path) -> Result<Self, DeviceError> {
        let file = match std::fs::File::open(directory.join("connection-reset.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(_) => return Err(DeviceError::Storage),
        };
        let mut bytes = Vec::new();
        file.take(262145)
            .read_to_end(&mut bytes)
            .map_err(|_| DeviceError::Storage)?;
        if bytes.len() > 262144 {
            return Err(DeviceError::SettingsCorrupt);
        }
        let value: Self =
            serde_json::from_slice(&bytes).map_err(|_| DeviceError::SettingsCorrupt)?;
        if value
            .retired
            .iter()
            .any(|(hub, device)| !super::stable_id(hub) || !super::stable_id(device))
        {
            return Err(DeviceError::SettingsCorrupt);
        }
        Ok(value)
    }
    pub(crate) fn save(&self, directory: &Utf8Path) -> Result<(), DeviceError> {
        std::fs::create_dir_all(directory).map_err(|_| DeviceError::Storage)?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|_| DeviceError::Storage)?;
        if bytes.len() > 262144 {
            return Err(DeviceError::Storage);
        }
        let mut file =
            tempfile::NamedTempFile::new_in(directory).map_err(|_| DeviceError::Storage)?;
        file.write_all(&bytes).map_err(|_| DeviceError::Storage)?;
        file.as_file()
            .sync_all()
            .map_err(|_| DeviceError::Storage)?;
        file.persist(directory.join("connection-reset.json"))
            .map_err(|_| DeviceError::Storage)?;
        Ok(())
    }
    pub(crate) fn retire(&mut self, hub: Option<&str>, device: Option<&str>) {
        self.pending = true;
        if let (Some(hub), Some(device)) = (hub, device) {
            let pair = (hub.to_owned(), device.to_owned());
            if !self.retired.contains(&pair) {
                self.retired.push(pair);
            }
        }
    }
    pub(crate) fn retired(&self, hub: &str, device: &str) -> bool {
        self.retired
            .iter()
            .any(|pair| pair.0 == hub && pair.1 == device)
    }
}

pub(crate) fn execution_identity_reset(
    directory: &Utf8Path,
    hub: &str,
    device: &str,
) -> Result<bool, DeviceError> {
    let state = ResetState::load(directory)?;
    Ok(state.pending || state.retired(hub, device))
}

pub(crate) fn configured_execution_reset(hub: &str, device: &str) -> Result<bool, DeviceError> {
    let config = crate::config::loader::global_config_path().map_err(|_| DeviceError::Storage)?;
    execution_identity_reset(&config.with_file_name("device-network"), hub, device)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_reset_and_retired_identity_stay_denied_after_new_hub_import() {
        let temp = tempfile::tempdir().unwrap();
        let directory = Utf8Path::from_path(temp.path()).unwrap();
        let mut reset = ResetState::default();
        reset.retire(Some("old-hub"), Some("old-device"));
        reset.execution_review_required = true;
        reset.save(directory).unwrap();
        assert!(execution_identity_reset(directory, "old-hub", "old-device").unwrap());
        reset.pending = false;
        reset.save(directory).unwrap();
        assert!(execution_identity_reset(directory, "old-hub", "old-device").unwrap());
        assert!(!execution_identity_reset(directory, "new-hub", "new-device").unwrap());
        assert!(!execution_identity_reset(directory, "old-hub", "new-device").unwrap());
        assert!(
            ResetState::load(directory)
                .unwrap()
                .execution_review_required
        );
    }
}
