use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};

use crate::error::StorageError;
use crate::harness::{ArtifactId, ArtifactManifest, HarnessRunId};

pub trait ArtifactStore {
    fn insert_artifact(&self, manifest: &ArtifactManifest) -> Result<(), StorageError>;
    fn list_artifacts(&self, run_id: HarnessRunId) -> Result<Vec<ArtifactManifest>, StorageError>;
}

#[derive(Clone)]
pub struct SqliteArtifactStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteArtifactStore {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }
}

impl ArtifactStore for SqliteArtifactStore {
    fn insert_artifact(&self, manifest: &ArtifactManifest) -> Result<(), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "INSERT OR REPLACE INTO harness_artifacts (id, run_id, kind, relative_path, sha256, size_bytes, tags_json, created_by_event_id, contract_refs_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, strftime('%s','now') * 1000)",
            params![
                manifest.id.to_string(),
                manifest.run_id.to_string(),
                serde_json::to_string(&manifest.kind)?,
                manifest.relative_path.as_str(),
                manifest.sha256,
                manifest.size_bytes as i64,
                serde_json::to_string(&manifest.tags)?,
                manifest.created_by_event.map(|id| id.to_string()),
                serde_json::to_string(&manifest.contract_refs)?,
            ],
        )?;
        Ok(())
    }

    fn list_artifacts(&self, run_id: HarnessRunId) -> Result<Vec<ArtifactManifest>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, kind, relative_path, sha256, size_bytes, tags_json, created_by_event_id, contract_refs_json
             FROM harness_artifacts WHERE run_id = ?1 ORDER BY created_at_ms ASC",
        )?;
        let rows = statement.query_map(params![run_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        let mut artifacts = Vec::new();
        for row in rows {
            let (id, kind, relative_path, sha256, size_bytes, tags_json, created_by, refs_json) =
                row?;
            artifacts.push(ArtifactManifest {
                id: id.parse::<ArtifactId>().map_err(|error| {
                    StorageError::Message(format!("invalid artifact id `{id}`: {error}"))
                })?,
                run_id,
                kind: serde_json::from_str(&kind)?,
                relative_path: relative_path.into(),
                sha256,
                size_bytes: size_bytes as u64,
                tags: serde_json::from_str(&tags_json)?,
                created_by_event: created_by
                    .map(|value| {
                        value
                            .parse()
                            .map_err(|error| StorageError::Message(format!("{error}")))
                    })
                    .transpose()?,
                contract_refs: serde_json::from_str(&refs_json)?,
            });
        }
        Ok(artifacts)
    }
}
