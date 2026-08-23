use std::fmt::{Display, Formatter};
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::StorageError;
use crate::protocol::HistoryItemId;
use crate::runtime::{Clock, SystemClock};
use crate::session::SessionId;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PermissionRetryFenceKey {
    root_session_id: SessionId,
    family_version: u32,
    family_sha256: String,
}

impl PermissionRetryFenceKey {
    pub fn new(
        root_session_id: SessionId,
        family_version: u32,
        family_sha256: impl Into<String>,
    ) -> Result<Self, StorageError> {
        if family_version == 0 {
            return Err(StorageError::Message(
                "permission retry fence family version must be positive".to_string(),
            ));
        }
        let family_sha256 = family_sha256.into();
        validate_sha256(&family_sha256, "permission retry fence family")?;
        Ok(Self {
            root_session_id,
            family_version,
            family_sha256,
        })
    }

    pub fn root_session_id(&self) -> SessionId {
        self.root_session_id
    }

    pub fn family_version(&self) -> u32 {
        self.family_version
    }

    pub fn family_sha256(&self) -> &str {
        &self.family_sha256
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PermissionReviewId(pub Ulid);

impl PermissionReviewId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for PermissionReviewId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for PermissionReviewId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for PermissionReviewId {
    type Err = ulid::DecodeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_string(value)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionRetryFenceState {
    Reviewing,
    AllowedPending,
    Admitted,
    Denied,
}

impl PermissionRetryFenceState {
    fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "reviewing" => Ok(Self::Reviewing),
            "allowed_pending" => Ok(Self::AllowedPending),
            "admitted" => Ok(Self::Admitted),
            "denied" => Ok(Self::Denied),
            _ => Err(StorageError::Message(format!(
                "permission retry fence has invalid state `{value}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionRetryFenceOutcome {
    GuardianDenied,
    InvalidDecision,
    DeadlineExceeded,
    GuardianError,
}

impl PermissionRetryFenceOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GuardianDenied => "guardian_denied",
            Self::InvalidDecision => "invalid_decision",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::GuardianError => "guardian_error",
        }
    }

    fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "guardian_denied" => Ok(Self::GuardianDenied),
            "invalid_decision" => Ok(Self::InvalidDecision),
            "deadline_exceeded" => Ok(Self::DeadlineExceeded),
            "guardian_error" => Ok(Self::GuardianError),
            _ => Err(StorageError::Message(format!(
                "permission retry fence has invalid outcome `{value}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRetryFenceRecord {
    pub key: PermissionRetryFenceKey,
    pub authority_history_item_id: HistoryItemId,
    pub state: PermissionRetryFenceState,
    pub review_id: PermissionReviewId,
    pub identity_sha256: String,
    pub outcome: Option<PermissionRetryFenceOutcome>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionReviewClaim {
    pub key: PermissionRetryFenceKey,
    pub authority_history_item_id: HistoryItemId,
    pub review_id: PermissionReviewId,
    pub identity_sha256: String,
}

#[derive(Debug, Clone)]
pub enum BeginPermissionReview {
    Claimed(PermissionReviewLease),
    Blocked(PermissionRetryFenceRecord),
    AuthorityChanged {
        current_authority_history_item_id: Option<HistoryItemId>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionReviewTransition {
    Applied,
    NotOwned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionEffectAdmission {
    Admitted,
    AuthorityChanged {
        current_authority_history_item_id: Option<HistoryItemId>,
    },
    NotOwned,
}

#[derive(Clone)]
pub struct SqlitePermissionRetryFenceStore {
    connection: Arc<Mutex<Connection>>,
}

#[derive(Clone)]
pub struct PermissionReviewLease {
    inner: Arc<PermissionReviewLeaseInner>,
}

struct PermissionReviewLeaseInner {
    store: SqlitePermissionRetryFenceStore,
    claim: PermissionReviewClaim,
}

impl std::fmt::Debug for PermissionReviewLease {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PermissionReviewLease")
            .field("claim", &self.inner.claim)
            .finish_non_exhaustive()
    }
}

impl PermissionReviewLease {
    fn new(store: SqlitePermissionRetryFenceStore, claim: PermissionReviewClaim) -> Self {
        Self {
            inner: Arc::new(PermissionReviewLeaseInner { store, claim }),
        }
    }

    pub fn claim(&self) -> &PermissionReviewClaim {
        &self.inner.claim
    }

    pub fn mark_allowed_pending(&self) -> Result<PermissionReviewTransition, StorageError> {
        self.inner
            .store
            .mark_allowed_pending(&self.inner.claim, SystemClock.now_ms())
    }

    pub fn mark_denied(
        &self,
        outcome: PermissionRetryFenceOutcome,
    ) -> Result<PermissionReviewTransition, StorageError> {
        self.inner
            .store
            .mark_denied(&self.inner.claim, outcome, SystemClock.now_ms())
    }

    pub fn admit_if_authority_current(&self) -> Result<PermissionEffectAdmission, StorageError> {
        self.inner
            .store
            .admit_if_authority_current(&self.inner.claim, SystemClock.now_ms())
    }

    pub fn release(&self) -> Result<PermissionReviewTransition, StorageError> {
        self.inner.store.release(&self.inner.claim)
    }
}

impl Drop for PermissionReviewLeaseInner {
    fn drop(&mut self) {
        // Drop must neither wait nor panic; a cleanup error deliberately leaves the
        // durable row in place so a later automatic retry remains fail-closed.
        let _ = self.store.release_approved_effect(&self.claim);
    }
}

impl SqlitePermissionRetryFenceStore {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }

    fn try_connection(&self) -> Result<MutexGuard<'_, Connection>, StorageError> {
        match self.connection.try_lock() {
            Ok(connection) => Ok(connection),
            Err(TryLockError::WouldBlock) => Err(StorageError::Message(
                "permission retry fence storage is busy".to_string(),
            )),
            Err(TryLockError::Poisoned(_)) => Err(StorageError::Message(
                "permission retry fence storage mutex is poisoned".to_string(),
            )),
        }
    }

    pub fn begin_review(
        &self,
        key: PermissionRetryFenceKey,
        authority_history_item_id: HistoryItemId,
        identity_sha256: impl Into<String>,
    ) -> Result<BeginPermissionReview, StorageError> {
        self.begin_review_at(
            key,
            authority_history_item_id,
            identity_sha256,
            SystemClock.now_ms(),
        )
    }

    pub fn begin_review_at(
        &self,
        key: PermissionRetryFenceKey,
        authority_history_item_id: HistoryItemId,
        identity_sha256: impl Into<String>,
        now_ms: i64,
    ) -> Result<BeginPermissionReview, StorageError> {
        validate_timestamp(now_ms)?;
        let identity_sha256 = identity_sha256.into();
        validate_sha256(&identity_sha256, "permission retry fence identity")?;
        let mut connection = self.try_connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_authority_history_item_id =
            latest_authority_history_item_id(&transaction, key.root_session_id)?;
        if current_authority_history_item_id != Some(authority_history_item_id) {
            transaction.commit()?;
            return Ok(BeginPermissionReview::AuthorityChanged {
                current_authority_history_item_id,
            });
        }
        if let Some(existing) = record_for_key(&transaction, &key)? {
            if existing.authority_history_item_id == authority_history_item_id {
                transaction.commit()?;
                return Ok(BeginPermissionReview::Blocked(existing));
            }
            transaction.execute(
                "DELETE FROM permission_retry_fences
                 WHERE root_session_id = ?1
                   AND family_version = ?2
                   AND family_sha256 = ?3",
                params![
                    key.root_session_id.to_string(),
                    i64::from(key.family_version),
                    key.family_sha256.as_str(),
                ],
            )?;
        }
        let review_id = PermissionReviewId::new();
        transaction.execute(
            "INSERT INTO permission_retry_fences (
                 root_session_id, family_version, family_sha256,
                 authority_history_item_id, state, review_id, identity_sha256,
                 outcome, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, 'reviewing', ?5, ?6, NULL, ?7, ?7)",
            params![
                key.root_session_id.to_string(),
                i64::from(key.family_version),
                key.family_sha256.as_str(),
                authority_history_item_id.to_string(),
                review_id.to_string(),
                identity_sha256.as_str(),
                now_ms,
            ],
        )?;
        transaction.commit()?;
        let claim = PermissionReviewClaim {
            key,
            authority_history_item_id,
            review_id,
            identity_sha256,
        };
        Ok(BeginPermissionReview::Claimed(PermissionReviewLease::new(
            self.clone(),
            claim,
        )))
    }

    pub fn mark_allowed_pending(
        &self,
        claim: &PermissionReviewClaim,
        now_ms: i64,
    ) -> Result<PermissionReviewTransition, StorageError> {
        self.transition_reviewing(claim, "allowed_pending", None, now_ms)
    }

    pub fn mark_denied(
        &self,
        claim: &PermissionReviewClaim,
        outcome: PermissionRetryFenceOutcome,
        now_ms: i64,
    ) -> Result<PermissionReviewTransition, StorageError> {
        self.transition_reviewing(claim, "denied", Some(outcome), now_ms)
    }

    pub fn admit_if_authority_current(
        &self,
        claim: &PermissionReviewClaim,
        now_ms: i64,
    ) -> Result<PermissionEffectAdmission, StorageError> {
        validate_timestamp(now_ms)?;
        let mut connection = self.try_connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_authority_history_item_id =
            latest_authority_history_item_id(&transaction, claim.key.root_session_id)?;
        if current_authority_history_item_id != Some(claim.authority_history_item_id) {
            delete_owned_non_denied(&transaction, claim)?;
            transaction.commit()?;
            return Ok(PermissionEffectAdmission::AuthorityChanged {
                current_authority_history_item_id,
            });
        }
        let updated = transaction.execute(
            "UPDATE permission_retry_fences
             SET state = 'admitted', updated_at_ms = ?7
             WHERE root_session_id = ?1
               AND family_version = ?2
               AND family_sha256 = ?3
               AND authority_history_item_id = ?4
               AND review_id = ?5
               AND identity_sha256 = ?6
               AND state = 'allowed_pending'",
            params![
                claim.key.root_session_id.to_string(),
                i64::from(claim.key.family_version),
                claim.key.family_sha256.as_str(),
                claim.authority_history_item_id.to_string(),
                claim.review_id.to_string(),
                claim.identity_sha256.as_str(),
                now_ms,
            ],
        )?;
        let already_admitted = if updated == 0 {
            transaction.query_row(
                "SELECT EXISTS (
                     SELECT 1
                     FROM permission_retry_fences
                     WHERE root_session_id = ?1
                       AND family_version = ?2
                       AND family_sha256 = ?3
                       AND authority_history_item_id = ?4
                       AND review_id = ?5
                       AND identity_sha256 = ?6
                       AND state = 'admitted'
                 )",
                params![
                    claim.key.root_session_id.to_string(),
                    i64::from(claim.key.family_version),
                    claim.key.family_sha256.as_str(),
                    claim.authority_history_item_id.to_string(),
                    claim.review_id.to_string(),
                    claim.identity_sha256.as_str(),
                ],
                |row| row.get::<_, bool>(0),
            )?
        } else {
            false
        };
        transaction.commit()?;
        Ok(if updated == 1 || already_admitted {
            PermissionEffectAdmission::Admitted
        } else {
            PermissionEffectAdmission::NotOwned
        })
    }

    pub fn release(
        &self,
        claim: &PermissionReviewClaim,
    ) -> Result<PermissionReviewTransition, StorageError> {
        let connection = self.try_connection()?;
        let deleted = delete_owned_non_denied(&connection, claim)?;
        Ok(if deleted == 1 {
            PermissionReviewTransition::Applied
        } else {
            PermissionReviewTransition::NotOwned
        })
    }

    fn release_approved_effect(
        &self,
        claim: &PermissionReviewClaim,
    ) -> Result<PermissionReviewTransition, StorageError> {
        let connection = self.try_connection()?;
        let deleted = delete_owned_approved_effect(&connection, claim)?;
        Ok(if deleted == 1 {
            PermissionReviewTransition::Applied
        } else {
            PermissionReviewTransition::NotOwned
        })
    }

    pub fn record(
        &self,
        key: &PermissionRetryFenceKey,
    ) -> Result<Option<PermissionRetryFenceRecord>, StorageError> {
        let connection = self.try_connection()?;
        record_for_key(&connection, key)
    }

    fn transition_reviewing(
        &self,
        claim: &PermissionReviewClaim,
        state: &str,
        outcome: Option<PermissionRetryFenceOutcome>,
        now_ms: i64,
    ) -> Result<PermissionReviewTransition, StorageError> {
        validate_timestamp(now_ms)?;
        let connection = self.try_connection()?;
        let updated = connection.execute(
            "UPDATE permission_retry_fences
             SET state = ?7, outcome = ?8, updated_at_ms = ?9
             WHERE root_session_id = ?1
               AND family_version = ?2
               AND family_sha256 = ?3
               AND authority_history_item_id = ?4
               AND review_id = ?5
               AND identity_sha256 = ?6
               AND state = 'reviewing'",
            params![
                claim.key.root_session_id.to_string(),
                i64::from(claim.key.family_version),
                claim.key.family_sha256.as_str(),
                claim.authority_history_item_id.to_string(),
                claim.review_id.to_string(),
                claim.identity_sha256.as_str(),
                state,
                outcome.map(PermissionRetryFenceOutcome::as_str),
                now_ms,
            ],
        )?;
        Ok(if updated == 1 {
            PermissionReviewTransition::Applied
        } else {
            PermissionReviewTransition::NotOwned
        })
    }
}

fn latest_authority_history_item_id(
    connection: &Connection,
    root_session_id: SessionId,
) -> Result<Option<HistoryItemId>, StorageError> {
    let value = connection
        .query_row(
            "SELECT history.id
             FROM protocol_item_append_order AS append_order
             INNER JOIN protocol_history_items AS history
               ON history.session_id = append_order.session_id
              AND history.id = append_order.source_id
             WHERE append_order.session_id = ?1
               AND append_order.source_kind = 'history_item'
               AND json_extract(history.payload_json, '$.kind')
                   IN ('user_turn', 'steer_turn')
             ORDER BY append_order.append_position DESC
             LIMIT 1",
            params![root_session_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    value
        .map(|value| {
            value.parse::<HistoryItemId>().map_err(|error| {
                StorageError::Message(format!(
                    "canonical permission authority item id `{value}` is invalid: {error}"
                ))
            })
        })
        .transpose()
}

fn record_for_key(
    connection: &Connection,
    key: &PermissionRetryFenceKey,
) -> Result<Option<PermissionRetryFenceRecord>, StorageError> {
    let raw = connection
        .query_row(
            "SELECT root_session_id, family_version, family_sha256,
                    authority_history_item_id, state, review_id, identity_sha256,
                    outcome, created_at_ms, updated_at_ms
             FROM permission_retry_fences
             WHERE root_session_id = ?1
               AND family_version = ?2
               AND family_sha256 = ?3",
            params![
                key.root_session_id.to_string(),
                i64::from(key.family_version),
                key.family_sha256.as_str(),
            ],
            raw_record,
        )
        .optional()?;
    raw.map(decode_record).transpose()
}

type RawPermissionRetryFenceRecord = (
    String,
    i64,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    i64,
    i64,
);

fn raw_record(row: &Row<'_>) -> rusqlite::Result<RawPermissionRetryFenceRecord> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

fn decode_record(
    raw: RawPermissionRetryFenceRecord,
) -> Result<PermissionRetryFenceRecord, StorageError> {
    let (
        root_session_id,
        family_version,
        family_sha256,
        authority_history_item_id,
        state,
        review_id,
        identity_sha256,
        outcome,
        created_at_ms,
        updated_at_ms,
    ) = raw;
    let root_session_id = root_session_id.parse::<SessionId>().map_err(|error| {
        StorageError::Message(format!(
            "permission retry fence root session id `{root_session_id}` is invalid: {error}"
        ))
    })?;
    let family_version = u32::try_from(family_version).map_err(|_| {
        StorageError::Message(format!(
            "permission retry fence family version `{family_version}` is invalid"
        ))
    })?;
    let key = PermissionRetryFenceKey::new(root_session_id, family_version, family_sha256)?;
    let authority_history_item_id = authority_history_item_id
        .parse::<HistoryItemId>()
        .map_err(|error| {
            StorageError::Message(format!(
                "permission retry fence authority item id `{authority_history_item_id}` is invalid: {error}"
            ))
        })?;
    let review_id = review_id.parse::<PermissionReviewId>().map_err(|error| {
        StorageError::Message(format!(
            "permission retry fence review id `{review_id}` is invalid: {error}"
        ))
    })?;
    validate_sha256(&identity_sha256, "permission retry fence identity")?;
    let state = PermissionRetryFenceState::parse(&state)?;
    let outcome = outcome
        .as_deref()
        .map(PermissionRetryFenceOutcome::parse)
        .transpose()?;
    validate_timestamp(created_at_ms)?;
    validate_timestamp(updated_at_ms)?;
    if updated_at_ms < created_at_ms {
        return Err(StorageError::Message(
            "permission retry fence update precedes its creation".to_string(),
        ));
    }
    if (state == PermissionRetryFenceState::Denied) != outcome.is_some() {
        return Err(StorageError::Message(
            "permission retry fence state and outcome disagree".to_string(),
        ));
    }
    Ok(PermissionRetryFenceRecord {
        key,
        authority_history_item_id,
        state,
        review_id,
        identity_sha256,
        outcome,
        created_at_ms,
        updated_at_ms,
    })
}

fn delete_owned_non_denied(
    connection: &Connection,
    claim: &PermissionReviewClaim,
) -> Result<usize, StorageError> {
    Ok(connection.execute(
        "DELETE FROM permission_retry_fences
         WHERE root_session_id = ?1
           AND family_version = ?2
           AND family_sha256 = ?3
           AND authority_history_item_id = ?4
           AND review_id = ?5
           AND identity_sha256 = ?6
           AND state IN ('reviewing', 'allowed_pending', 'admitted')",
        params![
            claim.key.root_session_id.to_string(),
            i64::from(claim.key.family_version),
            claim.key.family_sha256.as_str(),
            claim.authority_history_item_id.to_string(),
            claim.review_id.to_string(),
            claim.identity_sha256.as_str(),
        ],
    )?)
}

fn delete_owned_approved_effect(
    connection: &Connection,
    claim: &PermissionReviewClaim,
) -> Result<usize, StorageError> {
    Ok(connection.execute(
        "DELETE FROM permission_retry_fences
         WHERE root_session_id = ?1
           AND family_version = ?2
           AND family_sha256 = ?3
           AND authority_history_item_id = ?4
           AND review_id = ?5
           AND identity_sha256 = ?6
           AND state IN ('allowed_pending', 'admitted')",
        params![
            claim.key.root_session_id.to_string(),
            i64::from(claim.key.family_version),
            claim.key.family_sha256.as_str(),
            claim.authority_history_item_id.to_string(),
            claim.review_id.to_string(),
            claim.identity_sha256.as_str(),
        ],
    )?)
}

fn validate_sha256(value: &str, label: &str) -> Result<(), StorageError> {
    if value.len() != 64
        || !value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(StorageError::Message(format!(
            "{label} SHA-256 must be exactly 64 lowercase hexadecimal characters"
        )));
    }
    Ok(())
}

fn validate_timestamp(value: i64) -> Result<(), StorageError> {
    if value < 0 {
        return Err(StorageError::Message(
            "permission retry fence timestamp must be non-negative".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;
    use std::time::Duration;

    use rusqlite::{Connection, params};

    use super::{
        BeginPermissionReview, PermissionEffectAdmission, PermissionRetryFenceKey,
        PermissionRetryFenceOutcome, PermissionRetryFenceState, PermissionReviewTransition,
        SqlitePermissionRetryFenceStore,
    };
    use crate::protocol::HistoryItemId;
    use crate::session::SessionId;

    const FAMILY_SHA256: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const IDENTITY_SHA256: &str =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn open_test_connection(path: Option<&Path>) -> Connection {
        let connection = match path {
            Some(path) => Connection::open(path).expect("open test database"),
            None => Connection::open_in_memory().expect("open in-memory test database"),
        };
        connection
            .busy_timeout(Duration::from_secs(2))
            .expect("set busy timeout");
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY);
                 CREATE TABLE IF NOT EXISTS protocol_history_items (
                     id TEXT PRIMARY KEY,
                     session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                     payload_json TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS protocol_item_append_order (
                     session_id TEXT NOT NULL,
                     source_kind TEXT NOT NULL,
                     source_id TEXT NOT NULL,
                     append_position INTEGER NOT NULL,
                     PRIMARY KEY(session_id, append_position)
                 );
                 CREATE TABLE IF NOT EXISTS moyai_schema_migrations (
                     version INTEGER PRIMARY KEY NOT NULL,
                     name TEXT NOT NULL
                 );",
            )
            .expect("create permission retry-fence prerequisites");
        if connection
            .query_row(
                "SELECT COUNT(*) FROM moyai_schema_migrations WHERE version = 54",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("query V54 marker")
            == 0
        {
            connection
                .execute_batch(include_str!(
                    "../../migrations/V54__permission_retry_fences.sql"
                ))
                .expect("apply V54 test schema");
        }
        connection
    }

    fn test_store(connection: Connection) -> SqlitePermissionRetryFenceStore {
        SqlitePermissionRetryFenceStore::new(Arc::new(Mutex::new(connection)))
    }

    fn seed_session_and_authority(
        connection: &Connection,
        session_id: SessionId,
        authority_id: HistoryItemId,
        append_position: i64,
        kind: &str,
    ) {
        connection
            .execute(
                "INSERT OR IGNORE INTO sessions(id) VALUES (?1)",
                params![session_id.to_string()],
            )
            .expect("seed root session");
        connection
            .execute(
                "INSERT INTO protocol_history_items(id, session_id, payload_json)
                 VALUES (?1, ?2, json_object('kind', ?3))",
                params![authority_id.to_string(), session_id.to_string(), kind],
            )
            .expect("seed canonical authority item");
        connection
            .execute(
                "INSERT INTO protocol_item_append_order(
                     session_id, source_kind, source_id, append_position
                 ) VALUES (?1, 'history_item', ?2, ?3)",
                params![
                    session_id.to_string(),
                    authority_id.to_string(),
                    append_position,
                ],
            )
            .expect("seed canonical append order");
    }

    fn claimed(result: BeginPermissionReview) -> super::PermissionReviewLease {
        match result {
            BeginPermissionReview::Claimed(lease) => lease,
            other => panic!("expected claimed review, got {other:?}"),
        }
    }

    #[test]
    fn denial_survives_reopen_and_blocks_the_same_authority_generation() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let database_path = directory.path().join("permission-fence.sqlite3");
        let session_id = SessionId::new();
        let authority_id = HistoryItemId::new();
        let key = PermissionRetryFenceKey::new(session_id, 1, FAMILY_SHA256).expect("valid key");

        let connection = open_test_connection(Some(&database_path));
        seed_session_and_authority(&connection, session_id, authority_id, 0, "user_turn");
        let store = test_store(connection);
        let lease = claimed(
            store
                .begin_review_at(key.clone(), authority_id, IDENTITY_SHA256, 100)
                .expect("begin review"),
        );
        assert_eq!(
            store
                .mark_denied(
                    lease.claim(),
                    PermissionRetryFenceOutcome::GuardianDenied,
                    101,
                )
                .expect("persist denial"),
            PermissionReviewTransition::Applied
        );
        drop(lease);
        drop(store);

        let reopened = test_store(open_test_connection(Some(&database_path)));
        let BeginPermissionReview::Blocked(record) = reopened
            .begin_review_at(key, authority_id, IDENTITY_SHA256, 102)
            .expect("retry denied review")
        else {
            panic!("same-generation denial must remain fenced after reopen");
        };
        assert_eq!(record.state, PermissionRetryFenceState::Denied);
        assert_eq!(
            record.outcome,
            Some(PermissionRetryFenceOutcome::GuardianDenied)
        );
    }

    #[test]
    fn unsettled_review_drop_remains_durably_blocked() {
        let connection = open_test_connection(None);
        let session_id = SessionId::new();
        let authority_id = HistoryItemId::new();
        seed_session_and_authority(&connection, session_id, authority_id, 0, "user_turn");
        let store = test_store(connection);
        let key = PermissionRetryFenceKey::new(session_id, 1, FAMILY_SHA256).expect("valid key");
        let lease = claimed(
            store
                .begin_review_at(key.clone(), authority_id, IDENTITY_SHA256, 100)
                .expect("begin review"),
        );

        drop(lease);
        let record = store
            .record(&key)
            .expect("read unsettled fence")
            .expect("unsettled review remains durable");
        assert_eq!(record.state, PermissionRetryFenceState::Reviewing);
        assert!(matches!(
            store
                .begin_review_at(key, authority_id, IDENTITY_SHA256, 101)
                .expect("retry unsettled review"),
            BeginPermissionReview::Blocked(_)
        ));
    }

    #[test]
    fn explicit_release_clears_known_cancelled_review() {
        let connection = open_test_connection(None);
        let session_id = SessionId::new();
        let authority_id = HistoryItemId::new();
        seed_session_and_authority(&connection, session_id, authority_id, 0, "user_turn");
        let store = test_store(connection);
        let key = PermissionRetryFenceKey::new(session_id, 1, FAMILY_SHA256).expect("valid key");
        let lease = claimed(
            store
                .begin_review_at(key.clone(), authority_id, IDENTITY_SHA256, 100)
                .expect("begin review"),
        );

        assert_eq!(
            lease.release().expect("release cancelled review"),
            PermissionReviewTransition::Applied
        );
        drop(lease);
        assert!(store.record(&key).expect("read released fence").is_none());
        let retry = claimed(
            store
                .begin_review_at(key, authority_id, IDENTITY_SHA256, 101)
                .expect("retry after explicit cancellation"),
        );
        assert_eq!(
            retry.release().expect("release retry fixture"),
            PermissionReviewTransition::Applied
        );
    }

    #[test]
    fn busy_mutex_returns_an_error_and_lease_drop_remains_fail_closed() {
        let connection = open_test_connection(None);
        let session_id = SessionId::new();
        let authority_id = HistoryItemId::new();
        seed_session_and_authority(&connection, session_id, authority_id, 0, "user_turn");
        let shared = Arc::new(Mutex::new(connection));
        let store = SqlitePermissionRetryFenceStore::new(shared.clone());
        let key = PermissionRetryFenceKey::new(session_id, 1, FAMILY_SHA256).expect("valid key");
        let lease = claimed(
            store
                .begin_review_at(key.clone(), authority_id, IDENTITY_SHA256, 100)
                .expect("begin review"),
        );
        assert_eq!(
            lease.mark_allowed_pending().expect("record Guardian allow"),
            PermissionReviewTransition::Applied
        );

        let connection = shared.lock().expect("hold sqlite mutex");
        let error = store
            .record(&key)
            .expect_err("busy storage must return without blocking");
        assert!(
            error.to_string().contains("storage is busy"),
            "unexpected busy storage error: {error}"
        );
        drop(lease);
        assert_eq!(
            connection
                .query_row("SELECT state FROM permission_retry_fences", [], |row| {
                    row.get::<_, String>(0)
                })
                .expect("lease remains fail-closed while cleanup is busy"),
            "allowed_pending"
        );
        drop(connection);

        assert_eq!(
            store
                .record(&key)
                .expect("read retained fence")
                .expect("busy Drop must retain the fence")
                .state,
            PermissionRetryFenceState::AllowedPending
        );
    }

    #[test]
    fn poisoned_mutex_returns_an_error_and_lease_drop_does_not_panic() {
        let connection = open_test_connection(None);
        let session_id = SessionId::new();
        let authority_id = HistoryItemId::new();
        seed_session_and_authority(&connection, session_id, authority_id, 0, "user_turn");
        let shared = Arc::new(Mutex::new(connection));
        let store = SqlitePermissionRetryFenceStore::new(shared.clone());
        let key = PermissionRetryFenceKey::new(session_id, 1, FAMILY_SHA256).expect("valid key");
        let lease = claimed(
            store
                .begin_review_at(key, authority_id, IDENTITY_SHA256, 100)
                .expect("begin review"),
        );
        assert_eq!(
            lease.mark_allowed_pending().expect("record Guardian allow"),
            PermissionReviewTransition::Applied
        );

        let poison_target = shared.clone();
        let poison_result = thread::spawn(move || {
            let _connection = poison_target.lock().expect("poison target mutex");
            panic!("intentional permission retry-fence mutex poison");
        })
        .join();
        assert!(poison_result.is_err(), "fixture must poison the mutex");
        let error = store
            .record(&lease.claim().key)
            .expect_err("poisoned storage must return an error");
        assert!(
            error.to_string().contains("mutex is poisoned"),
            "unexpected poisoned storage error: {error}"
        );

        drop(lease);
        let connection = match shared.lock() {
            Ok(_) => panic!("test mutex must remain poisoned"),
            Err(poisoned) => poisoned.into_inner(),
        };
        assert_eq!(
            connection
                .query_row("SELECT state FROM permission_retry_fences", [], |row| {
                    row.get::<_, String>(0)
                })
                .expect("poisoned Drop leaves durable fence"),
            "allowed_pending"
        );
    }

    #[test]
    fn new_authority_replaces_old_fence_and_stale_lease_cannot_delete_it() {
        let connection = open_test_connection(None);
        let session_id = SessionId::new();
        let first_authority = HistoryItemId::new();
        let second_authority = HistoryItemId::new();
        seed_session_and_authority(&connection, session_id, first_authority, 0, "user_turn");
        let shared = Arc::new(Mutex::new(connection));
        let store = SqlitePermissionRetryFenceStore::new(shared.clone());
        let key = PermissionRetryFenceKey::new(session_id, 1, FAMILY_SHA256).expect("valid key");
        let stale_lease = claimed(
            store
                .begin_review_at(key.clone(), first_authority, IDENTITY_SHA256, 100)
                .expect("claim first generation"),
        );
        seed_session_and_authority(
            &shared.lock().expect("sqlite mutex"),
            session_id,
            second_authority,
            1,
            "steer_turn",
        );
        let current_lease = claimed(
            store
                .begin_review_at(key.clone(), second_authority, IDENTITY_SHA256, 101)
                .expect("claim second generation"),
        );

        drop(stale_lease);
        let current = store
            .record(&key)
            .expect("read current fence")
            .expect("second-generation fence");
        assert_eq!(current.authority_history_item_id, second_authority);
        assert_eq!(current.review_id, current_lease.claim().review_id);
    }

    #[test]
    fn later_compaction_history_does_not_advance_authority_or_clear_denial() {
        let connection = open_test_connection(None);
        let session_id = SessionId::new();
        let authority_id = HistoryItemId::new();
        let compaction_id = HistoryItemId::new();
        seed_session_and_authority(&connection, session_id, authority_id, 0, "user_turn");
        let shared = Arc::new(Mutex::new(connection));
        let store = SqlitePermissionRetryFenceStore::new(shared.clone());
        let key = PermissionRetryFenceKey::new(session_id, 1, FAMILY_SHA256).expect("valid key");
        let lease = claimed(
            store
                .begin_review_at(key.clone(), authority_id, IDENTITY_SHA256, 100)
                .expect("begin review"),
        );
        assert_eq!(
            store
                .mark_denied(
                    lease.claim(),
                    PermissionRetryFenceOutcome::DeadlineExceeded,
                    101,
                )
                .expect("persist denial"),
            PermissionReviewTransition::Applied
        );
        drop(lease);
        seed_session_and_authority(
            &shared.lock().expect("sqlite mutex"),
            session_id,
            compaction_id,
            1,
            "compaction",
        );

        let BeginPermissionReview::Blocked(record) = store
            .begin_review_at(key, authority_id, IDENTITY_SHA256, 102)
            .expect("retry after compaction")
        else {
            panic!("compaction must not mint a new permission authority generation");
        };
        assert_eq!(record.authority_history_item_id, authority_id);
        assert_eq!(
            record.outcome,
            Some(PermissionRetryFenceOutcome::DeadlineExceeded)
        );
    }

    #[test]
    fn synchronous_admission_is_cas_bound_to_the_current_authority() {
        let connection = open_test_connection(None);
        let session_id = SessionId::new();
        let first_authority = HistoryItemId::new();
        let second_authority = HistoryItemId::new();
        seed_session_and_authority(&connection, session_id, first_authority, 0, "user_turn");
        let shared = Arc::new(Mutex::new(connection));
        let store = SqlitePermissionRetryFenceStore::new(shared.clone());
        let key = PermissionRetryFenceKey::new(session_id, 1, FAMILY_SHA256).expect("valid key");
        let lease = claimed(
            store
                .begin_review_at(key.clone(), first_authority, IDENTITY_SHA256, 100)
                .expect("begin review"),
        );
        assert_eq!(
            store
                .mark_allowed_pending(lease.claim(), 101)
                .expect("record Guardian allow"),
            PermissionReviewTransition::Applied
        );
        seed_session_and_authority(
            &shared.lock().expect("sqlite mutex"),
            session_id,
            second_authority,
            1,
            "steer_turn",
        );

        assert_eq!(
            store
                .admit_if_authority_current(lease.claim(), 102)
                .expect("synchronous admission CAS"),
            PermissionEffectAdmission::AuthorityChanged {
                current_authority_history_item_id: Some(second_authority),
            }
        );
        assert!(store.record(&key).expect("read released fence").is_none());
    }

    #[test]
    fn synchronous_admission_is_idempotent_for_the_owned_lease() {
        let connection = open_test_connection(None);
        let session_id = SessionId::new();
        let authority_id = HistoryItemId::new();
        seed_session_and_authority(&connection, session_id, authority_id, 0, "user_turn");
        let store = test_store(connection);
        let key = PermissionRetryFenceKey::new(session_id, 1, FAMILY_SHA256).expect("valid key");
        let lease = claimed(
            store
                .begin_review_at(key.clone(), authority_id, IDENTITY_SHA256, 100)
                .expect("begin review"),
        );
        assert_eq!(
            store
                .mark_allowed_pending(lease.claim(), 101)
                .expect("record Guardian allow"),
            PermissionReviewTransition::Applied
        );
        assert_eq!(
            store
                .admit_if_authority_current(lease.claim(), 102)
                .expect("first synchronous admission"),
            PermissionEffectAdmission::Admitted
        );
        assert_eq!(
            store
                .admit_if_authority_current(lease.claim(), 103)
                .expect("idempotent synchronous admission"),
            PermissionEffectAdmission::Admitted
        );
        assert_eq!(
            store
                .record(&key)
                .expect("read admitted fence")
                .expect("owned admitted fence")
                .state,
            PermissionRetryFenceState::Admitted
        );
    }

    #[test]
    fn approved_fence_lives_until_the_last_lease_clone_drops() {
        for admit in [false, true] {
            let connection = open_test_connection(None);
            let session_id = SessionId::new();
            let authority_id = HistoryItemId::new();
            seed_session_and_authority(&connection, session_id, authority_id, 0, "user_turn");
            let store = test_store(connection);
            let key =
                PermissionRetryFenceKey::new(session_id, 1, FAMILY_SHA256).expect("valid key");
            let lease = claimed(
                store
                    .begin_review_at(key.clone(), authority_id, IDENTITY_SHA256, 100)
                    .expect("begin review"),
            );
            assert_eq!(
                store
                    .mark_allowed_pending(lease.claim(), 101)
                    .expect("record Guardian allow"),
                PermissionReviewTransition::Applied
            );
            if admit {
                assert_eq!(
                    store
                        .admit_if_authority_current(lease.claim(), 102)
                        .expect("admit effect"),
                    PermissionEffectAdmission::Admitted
                );
            }
            let first_clone = lease.clone();
            let last_clone = lease.clone();
            drop(lease);
            drop(first_clone);
            assert_eq!(
                store
                    .record(&key)
                    .expect("read retained approved fence")
                    .expect("another lease clone owns the approved fence")
                    .state,
                if admit {
                    PermissionRetryFenceState::Admitted
                } else {
                    PermissionRetryFenceState::AllowedPending
                }
            );
            drop(last_clone);
            assert!(
                store
                    .record(&key)
                    .expect("read released approved fence")
                    .is_none(),
                "the final approved lease clone owns cleanup"
            );
        }
    }

    #[test]
    fn concurrent_equivalent_reviews_have_exactly_one_claimant() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let database_path = directory.path().join("permission-fence.sqlite3");
        let session_id = SessionId::new();
        let authority_id = HistoryItemId::new();
        let connection = open_test_connection(Some(&database_path));
        seed_session_and_authority(&connection, session_id, authority_id, 0, "user_turn");
        drop(connection);

        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let database_path = database_path.clone();
            let barrier = barrier.clone();
            workers.push(thread::spawn(move || {
                let store = test_store(open_test_connection(Some(&database_path)));
                let key =
                    PermissionRetryFenceKey::new(session_id, 1, FAMILY_SHA256).expect("valid key");
                let result = store
                    .begin_review_at(key, authority_id, IDENTITY_SHA256, 100)
                    .expect("concurrent begin review");
                barrier.wait();
                matches!(result, BeginPermissionReview::Claimed(_))
            }));
        }
        barrier.wait();
        let claimed_count = workers
            .into_iter()
            .map(|worker| worker.join().expect("review worker"))
            .filter(|claimed| *claimed)
            .count();
        assert_eq!(claimed_count, 1);
    }
}
