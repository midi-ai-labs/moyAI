use std::io::{Read, Write};

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ulid::Ulid;

use super::{PublishError, PublishProfileId};

/// Application-owned verifier files. Plain bearer tokens are returned once, never saved.
/// Each profile owns one bounded file; credential references must match exactly.
#[derive(Clone)]
pub(super) struct CredentialStore {
    root: Utf8PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Verifier {
    version: u32,
    credential_id: Ulid,
    sha256: [u8; 32],
}

impl CredentialStore {
    pub fn new(root: Utf8PathBuf) -> Self {
        Self { root }
    }

    fn path(&self, profile: PublishProfileId) -> Utf8PathBuf {
        self.root.join(format!("{}.json", profile.0))
    }

    pub fn issue(&self, profile: PublishProfileId) -> Result<(Ulid, String), PublishError> {
        std::fs::create_dir_all(&self.root).map_err(PublishError::Storage)?;
        // Two independently generated ULIDs contain 160 bits of random entropy.
        let token = format!("moyai_{}{}", Ulid::new(), Ulid::new());
        let credential_id = Ulid::new();
        let verifier = Verifier {
            version: 1,
            credential_id,
            sha256: Sha256::digest(token.as_bytes()).into(),
        };
        let bytes = serde_json::to_vec(&verifier).map_err(|_| PublishError::InvalidDocument)?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(&self.root).map_err(PublishError::Storage)?;
        temporary.write_all(&bytes).map_err(PublishError::Storage)?;
        temporary
            .as_file()
            .sync_all()
            .map_err(PublishError::Storage)?;
        temporary
            .persist(self.path(profile))
            .map_err(|error| PublishError::Storage(error.error))?;
        Ok((credential_id, token))
    }

    pub fn verifier(
        &self,
        profile: PublishProfileId,
        credential_id: Ulid,
    ) -> Result<[u8; 32], PublishError> {
        let file = std::fs::File::open(self.path(profile)).map_err(|_| PublishError::Unpaired)?;
        let mut bytes = Vec::new();
        file.take(2049)
            .read_to_end(&mut bytes)
            .map_err(PublishError::Storage)?;
        if bytes.len() > 2048 {
            return Err(PublishError::InvalidDocument);
        }
        let value: Verifier =
            serde_json::from_slice(&bytes).map_err(|_| PublishError::InvalidDocument)?;
        if value.version != 1 || value.credential_id != credential_id {
            return Err(PublishError::Unpaired);
        }
        Ok(value.sha256)
    }

    pub fn revoke(&self, profile: PublishProfileId) -> Result<(), PublishError> {
        match std::fs::remove_file(self.path(profile)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(PublishError::Storage(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_rotation_and_revocation_never_store_plain_tokens() {
        let temp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(temp.path().to_owned()).unwrap();
        let store = CredentialStore::new(root);
        let profile = PublishProfileId(Ulid::new());
        let (first, token) = store.issue(profile).unwrap();
        assert_eq!(
            store.verifier(profile, first).unwrap(),
            <[u8; 32]>::from(Sha256::digest(token.as_bytes()))
        );
        let saved = std::fs::read_to_string(store.path(profile)).unwrap();
        assert!(!saved.contains(&token));
        let (second, new_token) = store.issue(profile).unwrap();
        assert_ne!(token, new_token);
        assert!(store.verifier(profile, first).is_err());
        assert!(
            store
                .verifier(PublishProfileId(Ulid::new()), second)
                .is_err()
        );
        assert!(store.verifier(profile, second).is_ok());
        store.revoke(profile).unwrap();
        assert!(store.verifier(profile, second).is_err());
    }
}
