use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::Ipv4Addr;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use sha2::{Digest, Sha256};

use super::DeviceError;

/// Private material is serialized only by this local store, never by a DTO.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeviceIdentity {
    schema_version: u32,
    private_key_pem: String,
}

impl std::fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceIdentity(<private>)")
    }
}

impl DeviceIdentity {
    fn generate() -> Result<Self, DeviceError> {
        let key = rcgen::KeyPair::generate().map_err(|_| DeviceError::InvalidIdentity)?;
        Ok(Self {
            schema_version: 1,
            private_key_pem: key.serialize_pem(),
        })
    }
    fn key(&self) -> Result<rcgen::KeyPair, DeviceError> {
        if self.schema_version != 1 || self.private_key_pem.len() > 16384 {
            return Err(DeviceError::InvalidIdentity);
        }
        rcgen::KeyPair::from_pem(&self.private_key_pem).map_err(|_| DeviceError::InvalidIdentity)
    }
    pub(crate) fn csr(&self, ip: Ipv4Addr) -> Result<String, DeviceError> {
        if ip.is_unspecified() || ip.is_multicast() {
            return Err(DeviceError::InvalidConfiguration);
        }
        let params = rcgen::CertificateParams::new(vec![ip.to_string()])
            .map_err(|_| DeviceError::InvalidIdentity)?;
        params
            .serialize_request(&self.key()?)
            .and_then(|request| request.pem())
            .map_err(|_| DeviceError::InvalidIdentity)
    }
    pub(crate) fn private_key_pem(&self) -> &str {
        &self.private_key_pem
    }
    #[cfg(test)]
    fn fingerprint(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(self.key().unwrap().public_key_pem().as_bytes())
        )
    }
}

#[derive(Clone)]
pub(crate) struct DeviceIdentityStore {
    path: Utf8PathBuf,
}

impl DeviceIdentityStore {
    pub(crate) fn new(path: Utf8PathBuf) -> Self {
        Self { path }
    }
    pub(crate) fn load(&self) -> Result<Option<DeviceIdentity>, DeviceError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(DeviceError::Storage),
        };
        let mut bytes = Vec::new();
        file.take(32769)
            .read_to_end(&mut bytes)
            .map_err(|_| DeviceError::Storage)?;
        if bytes.len() > 32768 {
            return Err(DeviceError::InvalidIdentity);
        }
        let identity: DeviceIdentity =
            serde_json::from_slice(&bytes).map_err(|_| DeviceError::InvalidIdentity)?;
        identity.key()?;
        Ok(Some(identity))
    }
    /// Explicit enrollment creates one local key. Corruption is never repaired
    /// by silently replacing a previously enrolled identity.
    pub(crate) fn load_or_create(&self) -> Result<DeviceIdentity, DeviceError> {
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
        if let Some(identity) = self.load()? {
            return Ok(identity);
        }
        let identity = DeviceIdentity::generate()?;
        let mut pending =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| DeviceError::Storage)?;
        let bytes = serde_json::to_vec(&identity).map_err(|_| DeviceError::Storage)?;
        pending
            .write_all(&bytes)
            .map_err(|_| DeviceError::Storage)?;
        pending
            .as_file()
            .sync_all()
            .map_err(|_| DeviceError::Storage)?;
        pending
            .persist_noclobber(&self.path)
            .map_err(|_| DeviceError::Storage)?;
        Ok(identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_future_unknown_and_oversized_identity_files_never_rotate_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("identity.json")).unwrap();
        let store = DeviceIdentityStore::new(path.clone());
        let baseline = serde_json::to_value(store.load_or_create().unwrap()).unwrap();
        let mut future = baseline.clone();
        future["schema_version"] = 2.into();
        let mut unknown = baseline.clone();
        unknown["device_id"] = "foreign-device".into();
        let mut broken_key = baseline.clone();
        broken_key["private_key_pem"] = "not a key".into();
        let mut long_key = baseline;
        long_key["private_key_pem"] = "x".repeat(16385).into();
        let mut payloads = [future, unknown, broken_key, long_key]
            .into_iter()
            .map(|value| serde_json::to_vec(&value).unwrap())
            .collect::<Vec<_>>();
        payloads.push(vec![b' '; 32769]);
        for bytes in payloads {
            std::fs::write(&path, &bytes).unwrap();
            assert!(matches!(
                store.load_or_create(),
                Err(DeviceError::InvalidIdentity)
            ));
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn enrollment_key_creation_respects_store_lock_and_csr_excludes_private_material() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("identity.json")).unwrap();
        let store = DeviceIdentityStore::new(path.clone());
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path.with_extension("lock"))
            .unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        assert!(matches!(
            store.load_or_create(),
            Err(DeviceError::StoreBusy)
        ));
        assert!(!path.exists());
        fs2::FileExt::unlock(&lock).unwrap();
        let identity = store.load_or_create().unwrap();
        for ip in [Ipv4Addr::UNSPECIFIED, Ipv4Addr::new(224, 0, 0, 1)] {
            assert!(matches!(
                identity.csr(ip),
                Err(DeviceError::InvalidConfiguration)
            ));
        }
        for ip in [Ipv4Addr::LOCALHOST, Ipv4Addr::new(192, 168, 1, 19)] {
            let csr = identity.csr(ip).unwrap();
            assert!(csr.contains("CERTIFICATE REQUEST"));
            assert!(!csr.contains("PRIVATE KEY"));
            assert!(!csr.contains(identity.private_key_pem()));
        }
        assert!(!format!("{identity:?}").contains(identity.private_key_pem()));
    }

    #[test]
    fn identity_survives_reopen_and_invalid_data_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("identity.json")).unwrap();
        let store = DeviceIdentityStore::new(path.clone());
        assert!(store.load().unwrap().is_none());
        assert!(!path.exists());
        let first = store.load_or_create().unwrap();
        assert_eq!(
            first.fingerprint(),
            store.load_or_create().unwrap().fingerprint()
        );
        assert!(!format!("{first:?}").contains("PRIVATE KEY"));
        assert!(
            first
                .csr(Ipv4Addr::LOCALHOST)
                .unwrap()
                .contains("CERTIFICATE REQUEST")
        );
        std::fs::write(&path, b"{broken}").unwrap();
        assert!(matches!(
            store.load_or_create(),
            Err(DeviceError::InvalidIdentity)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"{broken}");
    }
    #[test]
    fn identities_on_two_devices_are_distinct() {
        assert_ne!(
            DeviceIdentity::generate().unwrap().fingerprint(),
            DeviceIdentity::generate().unwrap().fingerprint()
        );
    }
}
