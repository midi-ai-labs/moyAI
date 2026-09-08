use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ulid::Ulid;

use crate::error::StorageError;
use crate::protocol::TurnId;
use crate::runtime::SystemClock;
use crate::session::{NewSession, SessionId};
use crate::storage::session_repo::insert_session_in_transaction;

pub const MAX_REMOTE_PROMPT_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteJobParent {
    pub peer_id: String,
    pub task_id: String,
    pub turn_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteTaskRequest {
    pub request_key: String,
    pub parent: RemoteJobParent,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<super::artifacts::RemoteInputFile>,
}

impl RemoteTaskRequest {
    pub fn validate(&self) -> bool {
        [
            &self.request_key,
            &self.parent.peer_id,
            &self.parent.task_id,
            &self.parent.turn_id,
        ]
        .into_iter()
        .all(|text| {
            !text.is_empty()
                && text.len() <= 128
                && text
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
        }) && !self.prompt.trim().is_empty()
            && self.prompt.len() <= MAX_REMOTE_PROMPT_BYTES
            && !self.prompt.contains('\0')
            && super::artifacts::validate_inputs(&self.inputs).is_ok()
    }

    pub(crate) fn fingerprint(&self, scope_json: &str) -> Result<String, StorageError> {
        let bytes = serde_json::to_vec(&(self, scope_json))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

#[derive(Debug, Clone)]
pub struct StoredRemoteJob {
    pub id: Ulid,
    pub principal_id: String,
    pub profile_id: Ulid,
    pub request_key: String,
    pub request_hash: String,
    pub scope_json: String,
    pub parent: RemoteJobParent,
    pub prompt_preview: String,
    pub session_id: SessionId,
    pub admitted_turn_id: Option<TurnId>,
    pub created_at_ms: i64,
}

/// Public peer metadata and an external-job locator, never a proxy local session.
/// Opaque grants/private keys are deliberately absent from the persisted type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredDeviceReference {
    pub id: Ulid,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub device_id: String,
    pub profile_id: String,
    pub root_task_id: String,
    pub request_key: String,
    pub prompt_hash: String,
    pub parent_grant_id: Option<String>,
    pub parent_job_id: Option<String>,
    pub peer: crate::device_network::DirectoryPeer,
    pub claims: Option<crate::device_network::GrantClaims>,
    pub job_id: Option<String>,
    pub state: String,
    pub stop_status: String,
    pub result: Option<String>,
}

impl StoredDeviceReference {
    fn validate(&self) -> Result<(), StorageError> {
        use crate::device_network::stable_id;
        if [
            &self.device_id,
            &self.profile_id,
            &self.root_task_id,
            &self.request_key,
        ]
        .iter()
        .any(|s| !stable_id(s))
            || self.parent_grant_id.as_ref().is_some_and(|s| !stable_id(s))
            || self.parent_job_id.as_ref().is_some_and(|s| !stable_id(s))
            || self.job_id.as_ref().is_some_and(|s| !stable_id(s))
            || self.parent_grant_id.is_some() != self.parent_job_id.is_some()
            || self.prompt_hash.len() != 64
            || !self
                .prompt_hash
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || self.state.len() > 64
            || self.stop_status.len() > 64
            || !stable_id(&self.state)
            || !stable_id(&self.stop_status)
            || self.result.as_ref().is_some_and(|s| s.len() > 64 * 1024)
            || self.peer.device_id != self.device_id
            || self.peer.profile_id != self.profile_id
        {
            return Err(invalid_reference());
        }
        if let Some(claims) = &self.claims {
            if claims.audience_device_id != self.device_id
                || claims.profile_id != self.profile_id
                || claims.root_task_id != self.root_task_id
                || claims.request_key != self.request_key
                || claims.parent_job_id != self.parent_job_id
                || claims.device_path.len() > 5
            {
                return Err(invalid_reference());
            }
        }
        Ok(())
    }
    fn same_identity(&self, other: &Self) -> bool {
        self.session_id == other.session_id
            && self.turn_id == other.turn_id
            && self.device_id == other.device_id
            && self.profile_id == other.profile_id
            && self.root_task_id == other.root_task_id
            && self.request_key == other.request_key
            && self.prompt_hash == other.prompt_hash
            && self.parent_grant_id == other.parent_grant_id
            && self.parent_job_id == other.parent_job_id
    }
    fn encode(&self) -> Result<String, StorageError> {
        self.validate()?;
        let encoded = serde_json::to_string(self)?;
        if encoded.len() > 128 * 1024 {
            return Err(invalid_reference());
        }
        Ok(encoded)
    }
    pub(super) fn decode(text: &str) -> Result<Self, StorageError> {
        if text.len() > 128 * 1024 {
            return Err(invalid_reference());
        }
        let row: Self = serde_json::from_str(text)?;
        row.validate()?;
        Ok(row)
    }
}

#[derive(Clone)]
pub struct RemoteJobStore {
    pub(super) connection: Arc<Mutex<Connection>>,
}

impl RemoteJobStore {
    pub(crate) fn pending_network_receipts(
        &self,
        limit: usize,
    ) -> Result<Vec<(StoredRemoteJob, String)>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare("SELECT job_id,grant_id FROM remote_network_receipts WHERE settlement_delivered=0 ORDER BY last_attempt_ms,job_id LIMIT ?1")?;
        let receipts = statement
            .query_map(params![limit.min(64) as i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        receipts
            .into_iter()
            .map(|(id, grant)| {
                Ok((
                    query_job(&connection, "id=?1", params![id])?.ok_or_else(invalid_record)?,
                    grant,
                ))
            })
            .collect()
    }

    pub(crate) fn network_receipt_attempt(
        &self,
        job: Ulid,
        grant: &str,
        delivered: bool,
    ) -> Result<(), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute("UPDATE remote_network_receipts SET last_attempt_ms=?3,settlement_delivered=MAX(settlement_delivered,?4) WHERE job_id=?1 AND grant_id=?2", params![job.to_string(), grant, SystemClock::now_ms().max(0), i64::from(delivered)])?;
        Ok(())
    }

    pub(crate) fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }

    pub(crate) fn device_references(
        &self,
        session: Option<SessionId>,
        limit: usize,
    ) -> Result<Vec<StoredDeviceReference>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let (sql, session) = match session {
            Some(id) => (
                "SELECT payload_json FROM device_outgoing_references WHERE session_id=?1 ORDER BY CASE WHEN json_extract(payload_json,'$.state') IN ('completed','failed','interrupted') THEN 1 ELSE 0 END,created_at_ms DESC,id DESC LIMIT ?2",
                Some(id.to_string()),
            ),
            None => (
                "SELECT payload_json FROM device_outgoing_references WHERE ?1 IS NULL ORDER BY CASE WHEN json_extract(payload_json,'$.state') IN ('completed','failed','interrupted') THEN 1 ELSE 0 END,created_at_ms DESC,id DESC LIMIT ?2",
                None,
            ),
        };
        let mut statement = connection.prepare(sql)?;
        let records = statement
            .query_map(params![session, limit.min(64) as i64], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        records
            .iter()
            .map(|text| StoredDeviceReference::decode(text))
            .collect()
    }
    pub(crate) fn device_reference(
        &self,
        id: Ulid,
    ) -> Result<Option<StoredDeviceReference>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        query_reference(&connection, "id=?1", params![id.to_string()])
    }
    pub(crate) fn find_device_reference(
        &self,
        session: SessionId,
        request_key: Option<&str>,
        job_id: Option<&str>,
    ) -> Result<Option<StoredDeviceReference>, StorageError> {
        let (column, value) = match (request_key, job_id) {
            (Some(key), None) => ("request_key", key),
            (None, Some(id)) => ("json_extract(payload_json,'$.job_id')", id),
            _ => return Err(invalid_reference()),
        };
        if !crate::device_network::stable_id(value) {
            return Err(invalid_reference());
        }
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        // Exact session lookup is independent of the bounded recent GUI projection.
        // Different peers can reuse a request key (or job ID); never choose one.
        let mut statement = connection.prepare(&format!(
            "SELECT payload_json FROM device_outgoing_references WHERE session_id=?1 AND {column}=?2 LIMIT 2"
        ))?;
        let mut rows = statement.query(params![session.to_string(), value])?;
        let Some(first) = rows.next()? else {
            return Ok(None);
        };
        let payload: String = first.get(0)?;
        if rows.next()?.is_some() {
            return Err(invalid_reference());
        }
        StoredDeviceReference::decode(&payload).map(Some)
    }
    pub(crate) fn accept_device_reference(
        &self,
        proposed: &StoredDeviceReference,
    ) -> Result<StoredDeviceReference, StorageError> {
        let payload = proposed.encode()?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = query_reference(
            &transaction,
            "session_id=?1 AND turn_id=?2 AND device_id=?3 AND profile_id=?4 AND request_key=?5",
            params![
                proposed.session_id.to_string(),
                proposed.turn_id.to_string(),
                proposed.device_id,
                proposed.profile_id,
                proposed.request_key
            ],
        )? {
            if !existing.same_identity(proposed) {
                return Err(StorageError::Message(
                    "outgoing request key conflicts with accepted content".into(),
                ));
            }
            transaction.commit()?;
            return Ok(existing);
        }
        transaction.execute("INSERT INTO device_outgoing_references(id,session_id,turn_id,device_id,profile_id,root_task_id,request_key,prompt_hash,parent_grant_id,parent_job_id,payload_json,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![proposed.id.to_string(),proposed.session_id.to_string(),proposed.turn_id.to_string(),proposed.device_id,proposed.profile_id,proposed.root_task_id,proposed.request_key,proposed.prompt_hash,proposed.parent_grant_id,proposed.parent_job_id,payload,SystemClock::now_ms().max(0)])?;
        transaction.commit()?;
        Ok(proposed.clone())
    }
    pub(crate) fn update_device_reference(
        &self,
        row: &StoredDeviceReference,
    ) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = query_reference(&transaction, "id=?1", params![row.id.to_string()])?
            .ok_or_else(invalid_reference)?;
        if !current.same_identity(row)
            || current
                .job_id
                .as_ref()
                .is_some_and(|id| row.job_id.as_ref() != Some(id))
        {
            return Err(invalid_reference());
        }
        let mut next = row.clone();
        if current.stop_status != "none" && next.stop_status == "none" {
            // A poll begun before shutdown cannot erase its durable stop intent.
            next.stop_status = current.stop_status;
        }
        let payload = next.encode()?;
        transaction.execute(
            "UPDATE device_outgoing_references SET payload_json=?1 WHERE id=?2",
            params![payload, row.id.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn mark_device_cancellations_unconfirmed(&self) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE device_outgoing_references SET payload_json=json_set(payload_json,'$.stop_status','unconfirmed') WHERE json_extract(payload_json,'$.state') NOT IN ('completed','failed','interrupted')",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn find_request(
        &self,
        principal: &str,
        key: &str,
    ) -> Result<Option<StoredRemoteJob>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        query_job(
            &connection,
            "principal_id = ?1 AND request_key = ?2",
            params![principal, key],
        )
    }

    pub fn get(&self, principal: &str, id: Ulid) -> Result<Option<StoredRemoteJob>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        query_job(
            &connection,
            "principal_id = ?1 AND id = ?2",
            params![principal, id.to_string()],
        )
    }

    pub(crate) fn get_for_profile(
        &self,
        profile: Ulid,
        id: Ulid,
    ) -> Result<Option<StoredRemoteJob>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        query_job(
            &connection,
            "profile_id = ?1 AND id = ?2",
            params![profile.to_string(), id.to_string()],
        )
    }

    pub fn recent(
        &self,
        profile_id: Ulid,
        limit: usize,
    ) -> Result<Vec<StoredRemoteJob>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare("SELECT id FROM remote_agent_jobs WHERE profile_id = ?1 ORDER BY created_at_ms DESC, id DESC LIMIT ?2")?;
        let ids = statement
            .query_map(
                params![profile_id.to_string(), limit.min(64) as i64],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                query_job(&connection, "id = ?1", params![id])?
                    .ok_or_else(|| StorageError::Message("remote job disappeared".into()))
            })
            .collect()
    }

    pub fn job_id_for_session(&self, session_id: SessionId) -> Result<Option<Ulid>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let id: Option<String> = connection
            .query_row(
                "SELECT id FROM remote_agent_jobs WHERE session_id = ?1",
                params![session_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        id.map(|id| id.parse().map_err(|_| invalid_record()))
            .transpose()
    }

    pub fn recent_all(&self, limit: usize) -> Result<Vec<StoredRemoteJob>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id FROM remote_agent_jobs ORDER BY created_at_ms DESC, id DESC LIMIT ?1",
        )?;
        let ids = statement
            .query_map(params![limit.min(64) as i64], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| query_job(&connection, "id = ?1", params![id])?.ok_or_else(invalid_record))
            .collect()
    }

    /// The session and deduplication receipt commit together, before a worker starts.
    /// A replay returns the original receipt and never creates another session.
    pub(crate) fn accept(
        &self,
        principal: &str,
        profile_id: Ulid,
        request: &RemoteTaskRequest,
        scope_json: &str,
        draft: &NewSession,
    ) -> Result<(StoredRemoteJob, bool), StorageError> {
        self.accept_with_network(principal, profile_id, request, scope_json, draft, None)
    }

    pub(crate) fn accept_with_network(
        &self,
        principal: &str,
        profile_id: Ulid,
        request: &RemoteTaskRequest,
        scope_json: &str,
        draft: &NewSession,
        grant_id: Option<&str>,
    ) -> Result<(StoredRemoteJob, bool), StorageError> {
        if !request.validate() {
            return Err(StorageError::Message("invalid remote request".into()));
        }
        let hash = request.fingerprint(scope_json)?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = query_job(
            &transaction,
            "principal_id = ?1 AND request_key = ?2",
            params![principal, request.request_key],
        )? {
            if existing.request_hash != hash || existing.profile_id != profile_id {
                return Err(StorageError::Message(
                    "remote request key conflicts with an accepted request".into(),
                ));
            }
            transaction.commit()?;
            return Ok((existing, false));
        }
        let id = Ulid::new();
        let session_id = SessionId::new();
        let now = SystemClock::now_ms().max(0);
        insert_session_in_transaction(&transaction, session_id, draft, now)?;
        transaction.execute("INSERT INTO remote_agent_jobs (id, principal_id, profile_id, request_key, request_hash, scope_json, parent_json, prompt_preview, session_id, created_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![id.to_string(), principal, profile_id.to_string(), request.request_key, hash, scope_json,
                serde_json::to_string(&request.parent)?, request.prompt.chars().take(256).collect::<String>(), session_id.to_string(), now])?;
        if let Some(grant) = grant_id {
            if !crate::device_network::stable_id(grant) {
                return Err(invalid_record());
            }
            transaction.execute(
                "INSERT INTO remote_network_receipts(job_id,grant_id) VALUES(?1,?2)",
                params![id.to_string(), grant],
            )?;
        }
        if !request.inputs.is_empty() {
            transaction.execute(
                "INSERT INTO remote_job_inputs(job_id,payload_json) VALUES(?1,?2)",
                params![id.to_string(), serde_json::to_string(&request.inputs)?],
            )?;
        }
        let job =
            query_job(&transaction, "id = ?1", params![id.to_string()])?.expect("inserted job");
        transaction.commit()?;
        Ok((job, true))
    }
}

pub(super) fn query_job(
    connection: &Connection,
    predicate: &str,
    parameters: impl rusqlite::Params,
) -> Result<Option<StoredRemoteJob>, StorageError> {
    // Predicates are static SQL chosen only by this module, never caller input.
    let query = format!(
        "SELECT id,principal_id,profile_id,request_key,request_hash,scope_json,parent_json,prompt_preview,session_id,admitted_turn_id,created_at_ms FROM remote_agent_jobs WHERE {predicate}"
    );
    let raw = connection
        .query_row(&query, parameters, |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, i64>(10)?,
            ))
        })
        .optional()?;
    raw.map(|raw| {
        Ok(StoredRemoteJob {
            id: raw.0.parse().map_err(|_| invalid_record())?,
            principal_id: raw.1,
            profile_id: raw.2.parse().map_err(|_| invalid_record())?,
            request_key: raw.3,
            request_hash: raw.4,
            scope_json: raw.5,
            parent: serde_json::from_str(&raw.6)?,
            prompt_preview: raw.7,
            session_id: raw.8.parse().map_err(|_| invalid_record())?,
            admitted_turn_id: raw
                .9
                .map(|id| id.parse().map_err(|_| invalid_record()))
                .transpose()?,
            created_at_ms: raw.10,
        })
    })
    .transpose()
}

fn invalid_record() -> StorageError {
    StorageError::Message("invalid remote job identity".into())
}
fn invalid_reference() -> StorageError {
    StorageError::Message("invalid outgoing remote job reference".into())
}
fn query_reference(
    connection: &Connection,
    predicate: &str,
    params: impl rusqlite::Params,
) -> Result<Option<StoredDeviceReference>, StorageError> {
    let text: Option<String> = connection
        .query_row(
            &format!("SELECT payload_json FROM device_outgoing_references WHERE {predicate}"),
            params,
            |row| row.get(0),
        )
        .optional()?;
    text.as_deref()
        .map(StoredDeviceReference::decode)
        .transpose()
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
