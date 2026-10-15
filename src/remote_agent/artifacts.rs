//! Immutable, bounded file versions belonging to one canonical remote job.
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};

use camino::{Utf8Path, Utf8PathBuf};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ulid::Ulid;

use super::store::{RemoteJobStore, StoredRemoteJob};
use crate::error::StorageError;
use crate::session::ChangeKind;
use crate::storage::StoreBundle;
use crate::workspace::{AccessKind, PathGuard, Workspace};

pub const MAX_ARTIFACT_FILES: usize = 8;
pub const MAX_ARTIFACT_FILE_BYTES: usize = 64 * 1024;
pub const MAX_ARTIFACT_TOTAL_BYTES: usize = 256 * 1024;
// Leave room below the existing 1 MiB MCP frame bound for the request prompt,
// JSON-RPC envelope and the structured-result wrapper, including JSON escaping.
const MAX_ENCODED_BYTES: usize = 768 * 1024;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteInputFile {
    pub path: String,
    pub sha256: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactFile {
    pub path: String,
    pub kind: ChangeKind,
    pub from_path: Option<String>,
    pub base_sha256: Option<String>,
    pub sha256: Option<String>,
    pub byte_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    pub job_id: String,
    pub version: String,
    pub files: Vec<ArtifactFile>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactBundle {
    pub manifest: ArtifactManifest,
    pub files: Vec<RemoteInputFile>,
}

fn invalid() -> StorageError {
    StorageError::Message("artifact version is invalid or unavailable".into())
}
fn digest(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 240
        && !path
            .chars()
            .any(|c| c.is_control() || "\\:*?\"<>|".contains(c))
        && path.split('/').all(|part| {
            let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
            !part.is_empty()
                && part.len() <= 100
                && part != "."
                && part != ".."
                && !part.ends_with([' ', '.'])
                && !matches!(
                    stem.as_str(),
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM1"
                        | "COM2"
                        | "COM3"
                        | "COM4"
                        | "COM5"
                        | "COM6"
                        | "COM7"
                        | "COM8"
                        | "COM9"
                        | "LPT1"
                        | "LPT2"
                        | "LPT3"
                        | "LPT4"
                        | "LPT5"
                        | "LPT6"
                        | "LPT7"
                        | "LPT8"
                        | "LPT9"
                )
        })
}
fn paths_are_distinct<'a>(paths: impl Iterator<Item = &'a str>) -> bool {
    let mut seen = HashSet::new();
    for path in paths {
        let key = path.to_lowercase();
        if !valid_path(path) || !seen.insert(key) {
            return false;
        }
    }
    // A file cannot also be the parent directory of another file.
    seen.iter().all(|path| {
        !path
            .match_indices('/')
            .any(|(i, _)| seen.contains(&path[..i]))
    })
}

pub(crate) fn validate_inputs(files: &[RemoteInputFile]) -> Result<(), StorageError> {
    if files.len() > MAX_ARTIFACT_FILES
        || !paths_are_distinct(files.iter().map(|file| file.path.as_str()))
        || files.iter().map(|file| file.text.len()).sum::<usize>() > MAX_ARTIFACT_TOTAL_BYTES
        || files.iter().any(|file| {
            file.text.len() > MAX_ARTIFACT_FILE_BYTES
                || file.text.contains('\0')
                || !valid_hash(&file.sha256)
                || digest(file.text.as_bytes()) != file.sha256
        })
    {
        return Err(invalid());
    }
    if serde_json::to_vec(files)?.len() > MAX_ENCODED_BYTES {
        return Err(invalid());
    }
    Ok(())
}

impl ArtifactBundle {
    pub(crate) fn validate(&self) -> Result<(), StorageError> {
        validate_inputs(&self.files)?;
        let manifest = &self.manifest;
        if manifest.job_id.parse::<Ulid>().is_err()
            || manifest.files.len() > MAX_ARTIFACT_FILES
            || !paths_are_distinct(manifest.files.iter().map(|file| file.path.as_str()))
            || manifest.version != digest(serde_json::to_vec(&(&manifest.job_id, &manifest.files))?)
        {
            return Err(invalid());
        }
        let mut content_count = 0;
        for file in &manifest.files {
            if file
                .base_sha256
                .as_deref()
                .is_some_and(|hash| !valid_hash(hash))
                || file
                    .from_path
                    .as_deref()
                    .is_some_and(|path| !valid_path(path))
                || (file.kind == ChangeKind::Move) != file.from_path.is_some()
            {
                return Err(invalid());
            }
            if file.kind == ChangeKind::Delete {
                if file.sha256.is_some()
                    || file.byte_length != 0
                    || self.files.iter().any(|data| data.path == file.path)
                {
                    return Err(invalid());
                }
            } else {
                content_count += 1;
                let data = self
                    .files
                    .iter()
                    .find(|data| data.path == file.path)
                    .ok_or_else(invalid)?;
                if file.sha256.as_ref() != Some(&data.sha256)
                    || file.byte_length != data.text.len() as u64
                {
                    return Err(invalid());
                }
            }
        }
        if content_count != self.files.len() {
            return Err(invalid());
        }
        if serde_json::to_vec(self)?.len() > MAX_ENCODED_BYTES {
            return Err(invalid());
        }
        Ok(())
    }
    fn encode(&self) -> Result<String, StorageError> {
        self.validate()?;
        let text = serde_json::to_string(self)?;
        if text.len() > MAX_ENCODED_BYTES {
            return Err(invalid());
        }
        Ok(text)
    }
    fn decode(text: &str) -> Result<Self, StorageError> {
        if text.len() > MAX_ENCODED_BYTES {
            return Err(invalid());
        }
        let bundle: Self = serde_json::from_str(text)?;
        bundle.validate()?;
        Ok(bundle)
    }
}

impl RemoteJobStore {
    pub(crate) fn artifact_bundle(
        &self,
        job: Ulid,
        version: Option<&str>,
    ) -> Result<Option<ArtifactBundle>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let text: Option<String> = connection
            .query_row(
                "SELECT payload_json FROM remote_job_artifacts WHERE job_id=?1",
                params![job.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let bundle = text.as_deref().map(ArtifactBundle::decode).transpose()?;
        if bundle.as_ref().is_some_and(|b| {
            b.manifest.job_id != job.to_string() || version.is_some_and(|v| v != b.manifest.version)
        }) {
            return Err(invalid());
        }
        Ok(bundle)
    }
    fn save_artifact_bundle(&self, bundle: &ArtifactBundle) -> Result<(), StorageError> {
        let encoded = bundle.encode()?;
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute("INSERT INTO remote_job_artifacts(job_id,version,payload_json) VALUES(?1,?2,?3) ON CONFLICT(job_id) DO NOTHING", params![bundle.manifest.job_id,bundle.manifest.version,encoded])?;
        let existing: String = connection.query_row(
            "SELECT payload_json FROM remote_job_artifacts WHERE job_id=?1",
            params![bundle.manifest.job_id],
            |row| row.get(0),
        )?;
        if existing != encoded {
            return Err(invalid());
        }
        Ok(())
    }
    pub(crate) fn cached_artifacts(
        &self,
        reference: Ulid,
        job: &str,
        version: Option<&str>,
    ) -> Result<Option<ArtifactBundle>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let text: Option<String> = connection.query_row("SELECT payload_json FROM device_artifact_cache WHERE reference_id=?1 AND job_id=?2", params![reference.to_string(),job], |row| row.get(0)).optional()?;
        let bundle = text.as_deref().map(ArtifactBundle::decode).transpose()?;
        if bundle.as_ref().is_some_and(|b| {
            b.manifest.job_id != job || version.is_some_and(|v| v != b.manifest.version)
        }) {
            return Err(invalid());
        }
        Ok(bundle)
    }
    pub(crate) fn cache_artifacts(
        &self,
        reference: Ulid,
        bundle: &ArtifactBundle,
    ) -> Result<(), StorageError> {
        let encoded = bundle.encode()?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let matching: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM device_outgoing_references WHERE id=?1 AND json_extract(payload_json,'$.job_id')=?2 AND json_extract(payload_json,'$.state') IN ('completed','failed','interrupted'))", params![reference.to_string(),bundle.manifest.job_id], |row| row.get(0))?;
        if !matching {
            return Err(invalid());
        }
        transaction.execute("INSERT INTO device_artifact_cache(reference_id,job_id,version,payload_json) VALUES(?1,?2,?3,?4) ON CONFLICT(reference_id) DO NOTHING", params![reference.to_string(),bundle.manifest.job_id,bundle.manifest.version,encoded])?;
        let existing: String = transaction.query_row(
            "SELECT payload_json FROM device_artifact_cache WHERE reference_id=?1",
            params![reference.to_string()],
            |row| row.get(0),
        )?;
        if existing != encoded {
            return Err(invalid());
        }
        transaction.commit()?;
        Ok(())
    }
}

/// The persisted source is immutable. This disposable copy grants read access only;
/// it is never installed into the receiver's workspace or global configuration.
pub(crate) struct StagedInputs {
    pub root: Utf8PathBuf,
    files: Vec<ArtifactFile>,
    _directory: tempfile::TempDir,
}
impl StagedInputs {
    pub(crate) fn context(&self) -> String {
        serde_json::json!({"directory":self.root,"files":self.files}).to_string()
    }
}
pub(crate) fn stage_inputs(
    jobs: &RemoteJobStore,
    job: &StoredRemoteJob,
) -> Result<Option<StagedInputs>, StorageError> {
    let text: Option<String> = jobs
        .connection
        .lock()
        .expect("sqlite mutex poisoned")
        .query_row(
            "SELECT payload_json FROM remote_job_inputs WHERE job_id=?1",
            params![job.id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(text) = text else {
        return Ok(None);
    };
    if text.len() > MAX_ENCODED_BYTES {
        return Err(invalid());
    }
    let files: Vec<RemoteInputFile> = serde_json::from_str(&text)?;
    validate_inputs(&files)?;
    let directory = tempfile::Builder::new()
        .prefix("moyai-remote-inputs-")
        .tempdir()
        .map_err(|_| invalid())?;
    let root = Utf8PathBuf::from_path_buf(directory.path().to_path_buf()).map_err(|_| invalid())?;
    write_files(&root, &files)?;
    let files = files
        .into_iter()
        .map(|file| ArtifactFile {
            path: file.path,
            kind: ChangeKind::Add,
            from_path: None,
            base_sha256: None,
            sha256: Some(file.sha256),
            byte_length: file.text.len() as u64,
        })
        .collect();
    Ok(Some(StagedInputs {
        root,
        files,
        _directory: directory,
    }))
}

/// Call after canonical terminal settlement and while the captured workspace is pinned.
pub(crate) fn capture_outputs(
    store: &StoreBundle,
    job: &StoredRemoteJob,
    workspace: &Workspace,
) -> Result<ArtifactManifest, StorageError> {
    let jobs = store.remote_job_store();
    let current = jobs.get(&job.principal_id, job.id)?.ok_or_else(invalid)?;
    if current.session_id != job.session_id
        || current.admitted_turn_id != job.admitted_turn_id
        || current.scope_json != job.scope_json
    {
        return Err(invalid());
    }
    if let Some(bundle) = jobs.artifact_bundle(job.id, None)? {
        return Ok(bundle.manifest);
    }
    let turn = job.admitted_turn_id.ok_or_else(invalid)?;
    let changes = store
        .change_repo()
        .terminal_changes_for_job(job.session_id, turn)?;
    let bundle = snapshot_changes(job.id, workspace, &changes)?;
    jobs.save_artifact_bundle(&bundle)?;
    Ok(bundle.manifest)
}

fn snapshot_changes(
    job: Ulid,
    workspace: &Workspace,
    changes: &[crate::edit::FileChange],
) -> Result<ArtifactBundle, StorageError> {
    let mut outputs = BTreeMap::<String, ArtifactFile>::new();
    for change in changes {
        let mut path = change
            .path_after
            .as_ref()
            .or(change.path_before.as_ref())
            .ok_or_else(invalid)?
            .as_str()
            .replace('\\', "/");
        if !valid_path(&path) {
            return Err(invalid());
        }
        let from_path = if change.kind == ChangeKind::Move {
            Some(
                change
                    .path_before
                    .as_ref()
                    .ok_or_else(invalid)?
                    .as_str()
                    .replace('\\', "/"),
            )
        } else {
            None
        };
        if from_path.as_deref().is_some_and(|path| !valid_path(path)) {
            return Err(invalid());
        }
        let earlier = outputs.remove(from_path.as_deref().unwrap_or(&path));
        let base_sha256 = earlier
            .as_ref()
            .map(|file| file.base_sha256.clone())
            .unwrap_or_else(|| change.before_sha256.clone());
        let origin_path = earlier
            .as_ref()
            .and_then(|file| file.from_path.clone())
            .or(from_path);
        let kind = if change.kind == ChangeKind::Delete {
            ChangeKind::Delete
        } else if origin_path.is_some() && base_sha256.is_some() {
            ChangeKind::Move
        } else if base_sha256.is_none() {
            ChangeKind::Add
        } else {
            ChangeKind::Update
        };
        if kind == ChangeKind::Delete && base_sha256.is_none() {
            continue;
        }
        if kind == ChangeKind::Delete {
            path = origin_path.clone().unwrap_or(path);
        }
        let from_path = if kind == ChangeKind::Move {
            origin_path
        } else {
            None
        };
        outputs.insert(
            path.clone(),
            ArtifactFile {
                path,
                kind,
                from_path,
                base_sha256,
                sha256: change.after_sha256.clone(),
                byte_length: 0,
            },
        );
        if outputs.len() > MAX_ARTIFACT_FILES {
            return Err(invalid());
        }
    }
    let mut files = Vec::new();
    for entry in outputs.values_mut() {
        let absolute = workspace.authority_root().join(&entry.path);
        let guarded = PathGuard::require_path(workspace, &absolute, AccessKind::Read)
            .map_err(|_| invalid())?;
        if !guarded.inside_workspace {
            return Err(invalid());
        }
        if entry.kind == ChangeKind::Delete {
            if absolute.try_exists().map_err(|_| invalid())? {
                return Err(invalid());
            }
            continue;
        }
        let mut file = PathGuard::open_validated_read_file(&guarded).map_err(|_| invalid())?;
        let mut bytes = Vec::new();
        (&mut file)
            .take((MAX_ARTIFACT_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        PathGuard::validate_open_file(&guarded, &file).map_err(|_| invalid())?;
        PathGuard::revalidate(&guarded).map_err(|_| invalid())?;
        if bytes.len() > MAX_ARTIFACT_FILE_BYTES
            || entry.sha256.as_deref() != Some(digest(&bytes).as_str())
        {
            return Err(invalid());
        }
        let text = String::from_utf8(bytes).map_err(|_| invalid())?;
        entry.byte_length = text.len() as u64;
        files.push(RemoteInputFile {
            path: entry.path.clone(),
            sha256: entry.sha256.clone().ok_or_else(invalid)?,
            text,
        });
    }
    let entries: Vec<_> = outputs.into_values().collect();
    let job_id = job.to_string();
    let version = digest(serde_json::to_vec(&(&job_id, &entries))?);
    let bundle = ArtifactBundle {
        manifest: ArtifactManifest {
            job_id,
            version,
            files: entries,
        },
        files,
    };
    bundle.validate()?;
    Ok(bundle)
}

fn write_files(root: &Utf8Path, files: &[RemoteInputFile]) -> Result<(), StorageError> {
    for input in files {
        let path = root.join(&input.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| invalid())?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| invalid())?;
        file.write_all(input.text.as_bytes())
            .map_err(|_| invalid())?;
        file.sync_all().map_err(|_| invalid())?;
    }
    Ok(())
}

/// Export a reviewed version to a new directory through the existing stable mutation owner.
/// Partial failure reports the newly-created directory; existing content is never overwritten.
pub(crate) fn export_bundle(
    bundle: &ArtifactBundle,
    destination: &Utf8Path,
) -> Result<(), StorageError> {
    bundle.validate()?;
    let parent = destination.parent().ok_or_else(invalid)?;
    let workspace = crate::workspace::WorkspaceDiscovery::discover_fixed_root(
        parent,
        &crate::config::ResolvedConfig::default(),
    )
    .map_err(|_| invalid())?;
    let guarded = PathGuard::require_path(&workspace, destination, AccessKind::Edit)
        .map_err(|_| invalid())?;
    let mut files: Vec<(String, String)> = bundle
        .files
        .iter()
        .map(|file| (format!("files/{}", file.path), file.text.clone()))
        .collect();
    files.push((
        "MANIFEST.json".into(),
        serde_json::to_string_pretty(&bundle.manifest)?,
    ));
    crate::tool::write_support::create_new_text_tree(&guarded, &files)
        .map_err(|error| StorageError::Message(error.to_string()))
}

#[cfg(test)]
#[path = "artifact_tests.rs"]
mod tests;
