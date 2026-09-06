use std::fs::{File, OpenOptions};
use std::io::{Read, Write};

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use super::HubRouteMode;
use super::{HubError, ReviewedHubSelection, bounded_text, valid_id};

const MAX_SETTINGS_BYTES: usize = 128 * 1024;

/// Application-owned preferences only. Runtime credentials and provider allocations cannot
/// be represented by this strict, versioned document. Revision zero means never saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubSettings {
    pub schema_version: u32,
    pub revision: String,
    pub endpoint: String,
    pub label: String,
    pub hub_id: Option<String>,
    pub main_review: Option<ReviewedHubSelection>,
    pub side_chat_review: Option<ReviewedHubSelection>,
    #[serde(default)]
    pub main_mode: HubRouteMode,
    #[serde(default)]
    pub side_chat_mode: HubRouteMode,
}

impl Default for HubSettings {
    fn default() -> Self {
        Self {
            schema_version: 2,
            revision: "0".into(),
            endpoint: String::new(),
            label: "moyAI Desktop".into(),
            hub_id: None,
            main_review: None,
            side_chat_review: None,
            main_mode: HubRouteMode::Direct,
            side_chat_mode: HubRouteMode::Direct,
        }
    }
}

pub(super) fn decimal(value: &str) -> Option<u64> {
    value
        .parse::<u64>()
        .ok()
        .filter(|parsed| parsed.to_string() == value)
}

impl HubSettings {
    fn validate(&self) -> Result<(), HubError> {
        if self.schema_version != 2
            || decimal(&self.revision).is_none()
            || !bounded_text(&self.label, 256)
        {
            return Err(HubError::SettingsInvalid);
        }
        if self.endpoint.is_empty() {
            if self.hub_id.is_some()
                || self.main_review.is_some()
                || self.side_chat_review.is_some()
            {
                return Err(HubError::SettingsInvalid);
            }
        } else {
            let canonical = super::client::validated_endpoint(&self.endpoint)
                .map_err(|_| HubError::SettingsInvalid)?;
            if canonical.as_str() != self.endpoint
                || !self.hub_id.as_ref().is_some_and(|id| valid_id(id))
            {
                return Err(HubError::SettingsInvalid);
            }
        }
        for review in [&self.main_review, &self.side_chat_review]
            .into_iter()
            .flatten()
        {
            if Some(&review.hub_id) != self.hub_id.as_ref() {
                return Err(HubError::SettingsInvalid);
            }
            review
                .selection
                .validate_shape()
                .map_err(|_| HubError::SettingsInvalid)?;
        }
        if (self.main_mode == HubRouteMode::Hub && self.main_review.is_none())
            || (self.side_chat_mode == HubRouteMode::Hub && self.side_chat_review.is_none())
        {
            return Err(HubError::SettingsInvalid);
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct HubSettingsStore {
    path: Utf8PathBuf,
}

impl HubSettingsStore {
    /// The caller supplies an application config path, never a browser-selected file path.
    pub fn new(path: Utf8PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<HubSettings, HubError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(HubSettings::default());
            }
            Err(_) => return Err(HubError::SettingsUnavailable),
        };
        let mut bytes = Vec::new();
        file.take((MAX_SETTINGS_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| HubError::SettingsUnavailable)?;
        if bytes.len() > MAX_SETTINGS_BYTES {
            return Err(HubError::SettingsInvalid);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| HubError::SettingsInvalid)?;
        match value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
        {
            Some(1)
                if value.get("main_mode").is_none() && value.get("side_chat_mode").is_none() => {}
            Some(2)
                if value.get("main_mode").is_some() && value.get("side_chat_mode").is_some() => {}
            _ => return Err(HubError::SettingsInvalid),
        }
        // Deserialize the original bytes so duplicate object fields remain errors.
        let mut settings: HubSettings =
            serde_json::from_slice(&bytes).map_err(|_| HubError::SettingsInvalid)?;
        if settings.schema_version == 1 {
            settings.schema_version = 2;
        }
        settings.validate()?;
        Ok(settings)
    }

    pub fn save(&self, proposed: &HubSettings) -> Result<HubSettings, HubError> {
        proposed.validate()?;
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_str().is_empty())
            .ok_or(HubError::SettingsUnavailable)?;
        std::fs::create_dir_all(parent).map_err(|_| HubError::SettingsUnavailable)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(format!("{}.lock", self.path))
            .map_err(|_| HubError::SettingsUnavailable)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
            if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                HubError::SettingsBusy
            } else {
                HubError::SettingsUnavailable
            }
        })?;
        let current = self.load()?;
        if current.revision != proposed.revision {
            return Err(HubError::SettingsChanged);
        }
        let mut saved = proposed.clone();
        saved.revision = decimal(&current.revision)
            .and_then(|value| value.checked_add(1))
            .ok_or(HubError::SettingsInvalid)?
            .to_string();
        let bytes = serde_json::to_vec_pretty(&saved).map_err(|_| HubError::SettingsInvalid)?;
        if bytes.len() > MAX_SETTINGS_BYTES {
            return Err(HubError::SettingsInvalid);
        }
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| HubError::SettingsUnavailable)?;
        temporary
            .write_all(&bytes)
            .map_err(|_| HubError::SettingsUnavailable)?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| HubError::SettingsUnavailable)?;
        temporary
            .persist(&self.path)
            .map_err(|_| HubError::SettingsUnavailable)?;
        drop(lock);
        Ok(saved)
    }
}
