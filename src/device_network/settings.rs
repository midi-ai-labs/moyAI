use std::fs::{File, OpenOptions};
use std::io::{Read, Write};

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use super::{DeviceError, canonical_revision, stable_id};
use crate::config::AccessMode;
use crate::mcp_publish::{PublishProfileId, PublishTarget};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedPeer {
    pub device_id: String,
    pub profile_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiverSettings {
    pub profile_id: PublishProfileId,
    pub target: PublishTarget,
    pub access_mode: AccessMode,
    pub model_mode: crate::hub::HubRouteMode,
    pub confirmed: bool,
    pub enabled: bool,
    #[serde(default)]
    pub start_on_launch: bool,
    #[serde(default)]
    pub keep_when_hidden: bool,
}
impl Default for ReceiverSettings {
    fn default() -> Self {
        Self {
            profile_id: PublishProfileId(ulid::Ulid::new()),
            target: PublishTarget::Temp {},
            access_mode: AccessMode::Default,
            model_mode: crate::hub::HubRouteMode::Hub,
            confirmed: false,
            enabled: false,
            start_on_launch: false,
            keep_when_hidden: false,
        }
    }
}

/// Device-specific public registration and explicit authority. No key, bearer,
/// gateway URL, prompt or response belongs to this document.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSettings {
    pub schema_version: u32,
    pub revision: String,
    pub hub_id: Option<String>,
    pub device_id: Option<String>,
    pub label: String,
    pub certificate_pem: Option<String>,
    pub certificate_sha256: Option<String>,
    pub expires_at_ms: Option<String>,
    pub selected_peers: Vec<SelectedPeer>,
    pub receiver: ReceiverSettings,
}
impl Default for DeviceSettings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            revision: "0".into(),
            hub_id: None,
            device_id: None,
            label: String::new(),
            certificate_pem: None,
            certificate_sha256: None,
            expires_at_ms: None,
            selected_peers: vec![],
            receiver: ReceiverSettings::default(),
        }
    }
}
impl DeviceSettings {
    fn validate(&self) -> Result<(), DeviceError> {
        let registration = [
            &self.hub_id,
            &self.device_id,
            &self.certificate_pem,
            &self.certificate_sha256,
            &self.expires_at_ms,
        ];
        if self.schema_version != 1
            || canonical_revision(&self.revision).is_none()
            || self.selected_peers.len() > 32
            || self.label.len() > 256
            || self.label.chars().any(char::is_control)
            || (registration.iter().any(|value| value.is_some())
                && registration.iter().any(|value| value.is_none()))
            || self.hub_id.as_ref().is_some_and(|value| !stable_id(value))
            || self
                .device_id
                .as_ref()
                .is_some_and(|value| !stable_id(value))
            || self.certificate_pem.as_ref().is_some_and(|value| {
                value.len() > 65536 || crate::mcp_publish::tls::public_certificate(value).is_err()
            })
            || self.certificate_sha256.as_ref().is_some_and(|value| {
                value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            || self
                .expires_at_ms
                .as_ref()
                .is_some_and(|value| canonical_revision(value).is_none())
            || matches!(self.receiver.target, PublishTarget::LegacySession { .. })
            || (self.receiver.enabled && !self.receiver.confirmed)
        {
            return Err(DeviceError::SettingsCorrupt);
        }
        let mut peers = std::collections::BTreeSet::new();
        for peer in &self.selected_peers {
            if !stable_id(&peer.device_id)
                || !stable_id(&peer.profile_id)
                || !peers.insert((&peer.device_id, &peer.profile_id))
            {
                return Err(DeviceError::SettingsCorrupt);
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct DeviceSettingsStore {
    path: Utf8PathBuf,
}
impl DeviceSettingsStore {
    pub fn new(path: Utf8PathBuf) -> Self {
        Self { path }
    }
    pub fn load(&self) -> Result<DeviceSettings, DeviceError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DeviceSettings::default());
            }
            Err(_) => return Err(DeviceError::Storage),
        };
        let mut bytes = Vec::new();
        file.take(131073)
            .read_to_end(&mut bytes)
            .map_err(|_| DeviceError::Storage)?;
        if bytes.len() > 131072 {
            return Err(DeviceError::SettingsCorrupt);
        }
        let settings: DeviceSettings =
            serde_json::from_slice(&bytes).map_err(|_| DeviceError::SettingsCorrupt)?;
        settings.validate()?;
        Ok(settings)
    }
    pub fn save(&self, proposed: &DeviceSettings) -> Result<DeviceSettings, DeviceError> {
        proposed.validate()?;
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_str().is_empty())
            .ok_or(DeviceError::Storage)?;
        std::fs::create_dir_all(parent).map_err(|_| DeviceError::Storage)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.with_extension("lock"))
            .map_err(|_| DeviceError::Storage)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| DeviceError::StoreBusy)?;
        let current = self.load()?;
        if current.revision != proposed.revision {
            return Err(DeviceError::SettingsChanged);
        }
        let mut saved = proposed.clone();
        saved.revision = canonical_revision(&current.revision)
            .and_then(|value| value.checked_add(1))
            .ok_or(DeviceError::SettingsChanged)?
            .to_string();
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|_| DeviceError::Storage)?;
        file.write_all(&serde_json::to_vec_pretty(&saved).map_err(|_| DeviceError::Storage)?)
            .map_err(|_| DeviceError::Storage)?;
        file.as_file()
            .sync_all()
            .map_err(|_| DeviceError::Storage)?;
        file.persist(&self.path).map_err(|_| DeviceError::Storage)?;
        Ok(saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_fixture() -> (tempfile::TempDir, Utf8PathBuf, DeviceSettingsStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("device.json")).unwrap();
        let store = DeviceSettingsStore::new(path.clone());
        (dir, path, store)
    }

    #[test]
    fn prior_receiver_settings_default_background_options_off_without_promoting_authority() {
        let (_dir, path, store) = store_fixture();
        let mut prior = serde_json::to_value(DeviceSettings::default()).unwrap();
        let receiver = prior["receiver"].as_object_mut().unwrap();
        receiver.remove("start_on_launch");
        receiver.remove("keep_when_hidden");
        std::fs::write(&path, serde_json::to_vec(&prior).unwrap()).unwrap();
        let loaded = store.load().unwrap();
        assert!(!loaded.receiver.start_on_launch);
        assert!(!loaded.receiver.keep_when_hidden);
        assert!(!loaded.receiver.confirmed);
        assert!(!loaded.receiver.enabled);
        assert_eq!(loaded.receiver.model_mode, crate::hub::HubRouteMode::Hub);
        assert_eq!(loaded.receiver.target, PublishTarget::Temp {});
        let saved = store.save(&loaded).unwrap();
        assert_eq!(store.load().unwrap().receiver, saved.receiver);
    }

    #[test]
    fn invalid_settings_are_not_repaired_or_overwritten_by_save() {
        let (_dir, path, store) = store_fixture();
        let original = DeviceSettings::default();
        let baseline = serde_json::to_value(&original).unwrap();
        let mut invalid = Vec::new();
        for revision in ["01", "+1", "-1", "18446744073709551616"] {
            let mut value = baseline.clone();
            value["revision"] = revision.into();
            invalid.push(value);
        }
        let mut future = baseline.clone();
        future["schema_version"] = 2.into();
        invalid.push(future);
        let mut partial = baseline.clone();
        partial["device_id"] = "device-a".into();
        invalid.push(partial);
        let mut unconfirmed = baseline.clone();
        unconfirmed["receiver"]["enabled"] = true.into();
        invalid.push(unconfirmed);
        let mut secret = baseline.clone();
        secret["receiver"]["token"] = "forbidden".into();
        invalid.push(secret);
        let mut duplicate = baseline.clone();
        duplicate["selected_peers"] = serde_json::json!([
            {"device_id":"device-a","profile_id":"profile-a"},{"device_id":"device-a","profile_id":"profile-a"}]);
        invalid.push(duplicate);
        let mut invalid_id = baseline.clone();
        invalid_id["selected_peers"] = serde_json::json!([
            {"device_id":"device-a\n","profile_id":"profile-a"}]);
        invalid.push(invalid_id);
        let mut overflow = baseline;
        overflow["selected_peers"] = serde_json::Value::Array((0..33).map(|index|
            serde_json::json!({"device_id":format!("device-{index}"),"profile_id":"profile"})).collect());
        invalid.push(overflow);
        for value in invalid {
            let bytes = serde_json::to_vec(&value).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            assert!(matches!(store.load(), Err(DeviceError::SettingsCorrupt)));
            assert!(matches!(
                store.save(&original),
                Err(DeviceError::SettingsCorrupt)
            ));
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        let oversized = vec![b' '; 131073];
        std::fs::write(&path, &oversized).unwrap();
        assert!(matches!(store.load(), Err(DeviceError::SettingsCorrupt)));
        assert_eq!(std::fs::read(&path).unwrap(), oversized);
    }

    #[test]
    fn selection_identity_is_device_and_profile_pair_and_store_lock_is_exclusive() {
        let (_dir, path, store) = store_fixture();
        let mut settings = store.load().unwrap();
        settings.selected_peers = vec![
            SelectedPeer {
                device_id: "device-a".into(),
                profile_id: "one".into(),
            },
            SelectedPeer {
                device_id: "device-a".into(),
                profile_id: "two".into(),
            },
        ];
        let saved = store.save(&settings).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))
            .unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        assert!(matches!(store.save(&saved), Err(DeviceError::StoreBusy)));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        fs2::FileExt::unlock(&lock).unwrap();
        assert_eq!(store.save(&saved).unwrap().selected_peers.len(), 2);
    }

    #[test]
    fn public_registration_rejects_invalid_or_private_certificate_payload() {
        let (_dir, path, store) = store_fixture();
        let pair = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let mut settings = DeviceSettings::default();
        settings.hub_id = Some("hub-a".into());
        settings.device_id = Some("device-a".into());
        settings.label = "Win00".into();
        settings.certificate_pem = Some(pair.cert.pem());
        settings.certificate_sha256 = Some("a".repeat(64));
        settings.expires_at_ms = Some("18446744073709551615".into());
        settings.validate().unwrap();
        let mut accepted = Vec::new();
        for (name, pem) in [
            ("empty", String::new()),
            (
                "invalid_der",
                "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----".into(),
            ),
            (
                "private_mixed",
                format!("{}\n{}", pair.cert.pem(), pair.signing_key.serialize_pem()),
            ),
        ] {
            settings.certificate_pem = Some(pem);
            std::fs::write(&path, serde_json::to_vec(&settings).unwrap()).unwrap();
            if store.load().is_ok() {
                accepted.push(name);
            }
        }
        assert!(
            accepted.is_empty(),
            "invalid public registration certificate accepted: {accepted:?}"
        );
    }

    #[test]
    fn explicit_authority_persists_with_cas_and_corruption_does_not_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("device.json")).unwrap();
        let store = DeviceSettingsStore::new(path.clone());
        let mut settings = store.load().unwrap();
        assert!(!path.exists());
        assert!(!settings.receiver.enabled);
        assert!(!settings.receiver.confirmed);
        let old = settings.clone();
        settings.receiver.confirmed = true;
        settings.selected_peers.push(SelectedPeer {
            device_id: "device-b".into(),
            profile_id: "profile-b".into(),
        });
        let saved = store.save(&settings).unwrap();
        assert_eq!(store.load().unwrap().selected_peers, saved.selected_peers);
        assert!(matches!(
            store.save(&old),
            Err(DeviceError::SettingsChanged)
        ));
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("private_key"));
        assert!(!raw.contains("token"));
        std::fs::write(
            &path,
            raw.replace(
                "\"schema_version\": 1",
                "\"schema_version\": 1, \"secret\": \"forbidden\"",
            ),
        )
        .unwrap();
        assert!(matches!(
            store.save(&saved),
            Err(DeviceError::SettingsCorrupt)
        ));
    }
}
