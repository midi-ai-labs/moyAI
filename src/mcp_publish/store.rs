use std::fs::{File, OpenOptions};
use std::io::{Read, Write};

use camino::Utf8PathBuf;
use serde::Deserialize;

use super::{
    PublishAuthentication, PublishBackgroundPolicy, PublishError, PublishMode, PublishProfile,
    PublishProfileId, PublishProfileSet, PublishTarget, PublishTransport,
};
use crate::session::{ProjectId, SessionId};
use crate::tool::ToolName;

const MAX_DOCUMENT_BYTES: usize = 256 * 1024;

// Frozen schema 1/2 readers. Old credentials remain read-only; only an explicit
// user save writes the current schema under the existing revision lock.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyProfileSet<T> {
    schema_version: u32,
    revision: u64,
    profiles: Vec<LegacyProfile<T>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyTarget {
    project_id: ProjectId,
    root_session_id: SessionId,
    workspace_root: Utf8PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyProfile<T> {
    id: PublishProfileId,
    label: String,
    #[serde(default)]
    enabled: bool,
    bind: std::net::SocketAddr,
    transport: PublishTransport,
    #[serde(default)]
    authentication: PublishAuthentication,
    target: T,
    tools: Vec<ToolName>,
    max_concurrent_calls: u16,
    #[serde(default)]
    background: PublishBackgroundPolicy,
}

impl<T> LegacyProfileSet<T> {
    fn migrate(self, target: impl Fn(T) -> PublishTarget) -> PublishProfileSet {
        PublishProfileSet {
            revision: self.revision,
            profiles: self
                .profiles
                .into_iter()
                .map(|profile| PublishProfile {
                    id: profile.id,
                    label: profile.label,
                    enabled: profile.enabled,
                    bind: profile.bind,
                    transport: profile.transport,
                    tls: None,
                    mode: PublishMode::ReadTools {},
                    authentication: profile.authentication,
                    target: target(profile.target),
                    tools: profile.tools,
                    max_concurrent_calls: profile.max_concurrent_calls,
                    background: profile.background,
                })
                .collect(),
            ..Default::default()
        }
    }
}

fn decode_profiles(bytes: &[u8]) -> Result<PublishProfileSet, PublishError> {
    #[derive(Deserialize)]
    struct Version {
        schema_version: u32,
    }
    let version: Version =
        serde_json::from_slice(bytes).map_err(|_| PublishError::InvalidDocument)?;
    let profiles = match version.schema_version {
        1 => {
            let legacy: LegacyProfileSet<LegacyTarget> =
                serde_json::from_slice(bytes).map_err(|_| PublishError::InvalidDocument)?;
            debug_assert_eq!(legacy.schema_version, 1);
            legacy.migrate(|target| PublishTarget::LegacySession {
                project_id: target.project_id,
                root_session_id: target.root_session_id,
                workspace_root: target.workspace_root,
            })
        }
        2 => {
            let legacy: LegacyProfileSet<PublishTarget> =
                serde_json::from_slice(bytes).map_err(|_| PublishError::InvalidDocument)?;
            debug_assert_eq!(legacy.schema_version, 2);
            legacy.migrate(|target| target)
        }
        super::profiles::SCHEMA_VERSION => {
            serde_json::from_slice(bytes).map_err(|_| PublishError::InvalidDocument)?
        }
        _ => {
            return Err(PublishError::InvalidConfiguration(
                "unsupported publish configuration version",
            ));
        }
    };
    profiles.validate()?;
    Ok(profiles)
}

/// Independent application-owned profile file. The Desktop integration must choose
/// an application config path; never resolve this path from an MCP caller's input.
/// Loading profiles does not start them. Writes use a nonblocking process lock,
/// optimistic revision check, and an atomic replacement in the same directory.
#[derive(Debug, Clone)]
pub struct PublishProfileStore {
    path: Utf8PathBuf,
}

impl PublishProfileStore {
    pub fn new(path: Utf8PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<PublishProfileSet, PublishError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PublishProfileSet::default());
            }
            Err(error) => return Err(PublishError::Storage(error)),
        };
        let mut bytes = Vec::new();
        file.take((MAX_DOCUMENT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(PublishError::Storage)?;
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(PublishError::InvalidDocument);
        }
        decode_profiles(&bytes)
    }

    /// The proposed revision must be the revision returned by load/previous save.
    /// Invalid, stale, or corrupt input never overwrites the last valid document.
    pub fn save(&self, proposed: &PublishProfileSet) -> Result<PublishProfileSet, PublishError> {
        proposed.validate()?;
        self.update(proposed.revision, |_| Ok((proposed.clone(), ())))
            .map(|(saved, ())| saved)
    }

    /// Credential mutations share the profile revision lock. A stale writer must
    /// not rotate or remove another writer's verifier before its CAS is checked.
    pub(super) fn update<T>(
        &self,
        revision: u64,
        change: impl FnOnce(PublishProfileSet) -> Result<(PublishProfileSet, T), PublishError>,
    ) -> Result<(PublishProfileSet, T), PublishError> {
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_str().is_empty())
            .ok_or(PublishError::InvalidConfiguration(
                "profile store must have a parent directory",
            ))?;
        std::fs::create_dir_all(parent).map_err(PublishError::Storage)?;
        let lock_path = Utf8PathBuf::from(format!("{}.lock", self.path));
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(PublishError::Storage)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
            if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                PublishError::StoreBusy
            } else {
                PublishError::Storage(error)
            }
        })?;
        // Keep this file handle (and its OS lock) alive through replacement.
        let current = self.load()?;
        if current.revision != revision {
            return Err(PublishError::StaleRevision);
        }
        let next_revision =
            current
                .revision
                .checked_add(1)
                .ok_or(PublishError::InvalidConfiguration(
                    "publish configuration revision exhausted",
                ))?;
        let (mut saved, result) = change(current)?;
        saved.validate()?;
        saved.revision = next_revision;
        let bytes = serde_json::to_vec_pretty(&saved).map_err(|_| PublishError::InvalidDocument)?;
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(PublishError::InvalidDocument);
        }
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(PublishError::Storage)?;
        temporary.write_all(&bytes).map_err(PublishError::Storage)?;
        temporary
            .as_file()
            .sync_all()
            .map_err(PublishError::Storage)?;
        temporary
            .persist(&self.path)
            .map_err(|error| PublishError::Storage(error.error))?;
        drop(lock);
        Ok((saved, result))
    }
}
