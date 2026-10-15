use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::Ipv4Addr;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use sha2::{Digest, Sha256};

use super::DeviceError;

pub(super) fn certificate_covers_ip(pem: &str, ip: Ipv4Addr) -> Result<bool, DeviceError> {
    use tokio_rustls::rustls::{client::verify_server_name, server::ParsedCertificate};
    let certificate = crate::mcp_publish::tls::public_certificate(pem)
        .map_err(|_| DeviceError::InvalidIdentity)?;
    let parsed =
        ParsedCertificate::try_from(&certificate).map_err(|_| DeviceError::InvalidIdentity)?;
    Ok(verify_server_name(&parsed, &std::net::IpAddr::V4(ip).into()).is_ok())
}

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
    pub(super) fn join_proof(
        &self,
        ip: Ipv4Addr,
        hub_id: &str,
        request_id: &str,
        challenge: &str,
    ) -> Result<String, DeviceError> {
        if ![hub_id, request_id, challenge]
            .iter()
            .all(|value| super::stable_id(value))
        {
            return Err(DeviceError::InvalidResponse);
        }
        let mut params = rcgen::CertificateParams::new(vec![ip.to_string()])
            .map_err(|_| DeviceError::InvalidIdentity)?;
        params.distinguished_name.push(
            rcgen::DnType::CommonName,
            format!("moyAI join {hub_id} {request_id} {challenge}"),
        );
        params
            .serialize_request(&self.key()?)
            .and_then(|csr| csr.pem())
            .map_err(|_| DeviceError::InvalidIdentity)
    }
    pub(super) fn public_key_sha256(&self) -> Result<String, DeviceError> {
        use rcgen::PublicKeyData;
        use sha2::{Digest, Sha256};
        Ok(format!(
            "{:x}",
            Sha256::digest(self.key()?.subject_public_key_info())
        ))
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
    /// Explicit local recovery retires this key; a future application gets a new key.
    pub(crate) fn remove(&self) -> Result<(), DeviceError> {
        let _lock = self.lock()?;
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(DeviceError::Storage),
        }
    }
    pub(crate) fn new(path: Utf8PathBuf) -> Self {
        Self { path }
    }
    fn read(&self) -> Result<Option<(DeviceIdentity, bool)>, DeviceError> {
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
        let result = decode_identity(&bytes);
        bytes.fill(0);
        result.map(Some)
    }
    fn lock(&self) -> Result<File, DeviceError> {
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
        Ok(lock)
    }
    fn save(&self, identity: &DeviceIdentity, replacing: bool) -> Result<(), DeviceError> {
        let parent = self.path.parent().ok_or(DeviceError::Storage)?;
        let mut bytes = encode_identity(identity)?;
        let mut pending =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| DeviceError::Storage)?;
        let written = pending.write_all(&bytes);
        bytes.fill(0);
        written.map_err(|_| DeviceError::Storage)?;
        pending
            .as_file()
            .sync_all()
            .map_err(|_| DeviceError::Storage)?;
        if replacing {
            pending
                .persist(&self.path)
                .map_err(|_| DeviceError::Storage)?;
        } else {
            pending
                .persist_noclobber(&self.path)
                .map_err(|_| DeviceError::Storage)?;
        }
        Ok(())
    }
    pub(crate) fn load(&self) -> Result<Option<DeviceIdentity>, DeviceError> {
        let Some((identity, legacy)) = self.read()? else {
            return Ok(None);
        };
        if cfg!(windows) && legacy {
            // Re-read under the same creation lock before upgrading. No caller may use
            // the legacy key until the protected representation has been persisted.
            let _lock = self.lock()?;
            let Some((current, still_legacy)) = self.read()? else {
                return Err(DeviceError::InvalidIdentity);
            };
            if still_legacy {
                self.save(&current, true)?;
            }
            return Ok(Some(current));
        }
        Ok(Some(identity))
    }
    /// Explicit enrollment creates one local key. Corruption or a different Windows
    /// user's protected key is never repaired by silently generating a replacement.
    pub(crate) fn load_or_create(&self) -> Result<DeviceIdentity, DeviceError> {
        let _lock = self.lock()?;
        if let Some((identity, legacy)) = self.read()? {
            if cfg!(windows) && legacy {
                self.save(&identity, true)?;
            }
            return Ok(identity);
        }
        let identity = DeviceIdentity::generate()?;
        self.save(&identity, false)?;
        Ok(identity)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtectedIdentity {
    schema_version: u32,
    protection: String,
    protected_private_key: String,
}

fn decode_identity(bytes: &[u8]) -> Result<(DeviceIdentity, bool), DeviceError> {
    #[derive(Deserialize)]
    struct Version {
        schema_version: u32,
    }
    let version: Version =
        serde_json::from_slice(bytes).map_err(|_| DeviceError::InvalidIdentity)?;
    let identity = match version.schema_version {
        1 => serde_json::from_slice(bytes).map_err(|_| DeviceError::InvalidIdentity)?,
        2 => {
            use base64::Engine as _;
            let envelope: ProtectedIdentity =
                serde_json::from_slice(bytes).map_err(|_| DeviceError::InvalidIdentity)?;
            if envelope.protection != "windows_current_user_dpapi" {
                return Err(DeviceError::InvalidIdentity);
            }
            let protected = base64::engine::general_purpose::STANDARD
                .decode(&envelope.protected_private_key)
                .map_err(|_| DeviceError::InvalidIdentity)?;
            let clear = protect_key(&protected, true)?;
            let private_key_pem =
                String::from_utf8(clear).map_err(|_| DeviceError::InvalidIdentity)?;
            DeviceIdentity {
                schema_version: 1,
                private_key_pem,
            }
        }
        _ => return Err(DeviceError::InvalidIdentity),
    };
    identity.key()?;
    Ok((identity, version.schema_version == 1))
}

fn encode_identity(identity: &DeviceIdentity) -> Result<Vec<u8>, DeviceError> {
    #[cfg(windows)]
    {
        use base64::Engine as _;
        let envelope = ProtectedIdentity {
            schema_version: 2,
            protection: "windows_current_user_dpapi".into(),
            protected_private_key: base64::engine::general_purpose::STANDARD
                .encode(protect_key(identity.private_key_pem.as_bytes(), false)?),
        };
        serde_json::to_vec(&envelope).map_err(|_| DeviceError::Storage)
    }
    #[cfg(not(windows))]
    serde_json::to_vec(identity).map_err(|_| DeviceError::Storage)
}

#[cfg(windows)]
fn protect_key(bytes: &[u8], decrypt: bool) -> Result<Vec<u8>, DeviceError> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    // Current-user protection, never CRYPTPROTECT_LOCAL_MACHINE. A file copied to
    // another Windows account is not sufficient to authenticate this device.
    let domain = b"moyAI device identity v2";
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes
            .len()
            .try_into()
            .map_err(|_| DeviceError::InvalidIdentity)?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: domain.len() as u32,
        pbData: domain.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    unsafe {
        let result = if decrypt {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if result == 0 {
            return Err(DeviceError::IdentityProtectionUnavailable);
        }
        if output.pbData.is_null() || output.cbData == 0 {
            if !output.pbData.is_null() {
                LocalFree(output.pbData.cast());
            }
            return Err(DeviceError::InvalidIdentity);
        }
        let copied = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        if decrypt {
            std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize).fill(0);
        }
        LocalFree(output.pbData.cast());
        Ok(copied)
    }
}
#[cfg(not(windows))]
fn protect_key(_: &[u8], _: bool) -> Result<Vec<u8>, DeviceError> {
    Err(DeviceError::IdentityProtectionUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_scope_uses_the_signed_ip_san_instead_of_a_listener_or_common_name() {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["127.0.0.2".into()]).unwrap();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "127.0.0.1");
        let cert = params.self_signed(&key).unwrap();
        assert!(certificate_covers_ip(&cert.pem(), Ipv4Addr::new(127, 0, 0, 2)).unwrap());
        assert!(!certificate_covers_ip(&cert.pem(), Ipv4Addr::LOCALHOST).unwrap());
        assert_eq!(
            certificate_covers_ip("not a certificate", Ipv4Addr::LOCALHOST),
            Err(DeviceError::InvalidIdentity)
        );
    }

    #[test]
    fn invalid_future_unknown_and_oversized_identity_files_never_rotate_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("identity.json")).unwrap();
        let store = DeviceIdentityStore::new(path.clone());
        let baseline = serde_json::to_value(store.load_or_create().unwrap()).unwrap();
        let mut future = baseline.clone();
        future["schema_version"] = 99.into();
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

    #[test]
    #[cfg(windows)]
    fn windows_key_storage_upgrades_legacy_without_rotating_or_rewriting_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("identity.json")).unwrap();
        let store = DeviceIdentityStore::new(path.clone());
        let legacy = DeviceIdentity::generate().unwrap();
        let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(&path, &legacy_bytes).unwrap();
        let lock = store.lock().unwrap();
        assert_eq!(store.load().unwrap_err(), DeviceError::StoreBusy);
        assert_eq!(std::fs::read(&path).unwrap(), legacy_bytes);
        drop(lock);
        let upgraded = store.load().unwrap().unwrap();
        assert_eq!(upgraded.fingerprint(), legacy.fingerprint());
        let bytes = std::fs::read(&path).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(saved["schema_version"], 2);
        assert_eq!(saved["protection"], "windows_current_user_dpapi");
        assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE KEY"));
        assert!(!String::from_utf8_lossy(&bytes).contains(legacy.private_key_pem()));
        assert_eq!(
            store.load_or_create().unwrap().fingerprint(),
            legacy.fingerprint()
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    #[cfg(windows)]
    fn another_windows_security_context_cannot_decrypt_or_replace_the_registered_key() {
        use windows_sys::Win32::{
            Security::{ImpersonateAnonymousToken, RevertToSelf},
            System::Threading::GetCurrentThread,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("identity.json")).unwrap();
        let store = DeviceIdentityStore::new(path.clone());
        let original = store.load_or_create().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        struct Revert;
        impl Drop for Revert {
            fn drop(&mut self) {
                assert_ne!(unsafe { RevertToSelf() }, 0);
            }
        }
        // No Windows account or system setting is created. Only this test thread
        // impersonates the anonymous OS identity for the DPAPI call.
        assert_ne!(unsafe { ImpersonateAnonymousToken(GetCurrentThread()) }, 0);
        let guard = Revert;
        assert!(matches!(
            decode_identity(&bytes),
            Err(DeviceError::IdentityProtectionUnavailable)
        ));
        drop(guard);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            store.load().unwrap().unwrap().fingerprint(),
            original.fingerprint()
        );
        let mut corrupted: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        corrupted["protected_private_key"] = "YWJj".into();
        let corrupted = serde_json::to_vec(&corrupted).unwrap();
        std::fs::write(&path, &corrupted).unwrap();
        assert!(matches!(
            store.load_or_create(),
            Err(DeviceError::IdentityProtectionUnavailable)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), corrupted);
    }
}
