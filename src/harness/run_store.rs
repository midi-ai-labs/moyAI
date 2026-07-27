use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::harness::{
    HarnessEvent, HarnessEventId, HarnessEventKind, HarnessEventPayload, HarnessRunId,
    artifact::hash_bytes, event_store::insert_event_in_connection,
};
use crate::protocol::{RuntimeEvent, RuntimeEventId, RuntimeEventMsg, TurnId, TurnTerminalOutcome};
use crate::session::{RunEvent, SessionId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessRunStatus {
    Started,
    Pass,
    Fail,
    Blocked,
}

impl HarnessRunStatus {
    pub const fn from_terminal_outcome(outcome: &TurnTerminalOutcome) -> Self {
        match outcome {
            TurnTerminalOutcome::Completed => Self::Pass,
            TurnTerminalOutcome::Interrupted { .. } => Self::Blocked,
            TurnTerminalOutcome::Failed { .. } => Self::Fail,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessRunRecord {
    pub id: HarnessRunId,
    pub session_id: Option<SessionId>,
    #[serde(default)]
    pub protocol_turn_id: Option<TurnId>,
    #[serde(default)]
    pub canonical_terminal_runtime_event_id: Option<RuntimeEventId>,
    pub workspace_root: Utf8PathBuf,
    pub artifact_root: Utf8PathBuf,
    pub mode: String,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub status: HarnessRunStatus,
}

pub trait HarnessRunStore {
    fn upsert_run(&self, run: &HarnessRunRecord) -> Result<(), StorageError>;
    fn get_run(&self, run_id: HarnessRunId) -> Result<Option<HarnessRunRecord>, StorageError>;
}

pub const MAX_HARNESS_TERMINAL_RECONCILIATION_PAGE_SIZE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessTerminalReconciliationPage {
    pub scanned_runs: usize,
    pub terminalized_runs: usize,
    pub next_after_run_id: Option<HarnessRunId>,
}

#[derive(Clone)]
pub struct SqliteHarnessRunStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteHarnessRunStore {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }

    /// Projects a terminal runtime event only after proving that it is the exact
    /// canonical event stored for its session and turn.
    ///
    /// Every matching Started harness is terminalized in the same SQLite
    /// transaction as its `RunTerminalized` event. Replaying the same canonical
    /// event is a validated no-op.
    pub fn project_canonical_terminal_event(
        &self,
        event: &RuntimeEvent,
    ) -> Result<usize, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = load_canonical_terminal_by_id(&transaction, event.id)?.ok_or_else(|| {
            StorageError::Message(format!(
                "runtime event {} is not canonical durable protocol state",
                event.id
            ))
        })?;
        require_same_runtime_event(event, &stored)?;
        let projected = project_canonical_terminal_in_transaction(&transaction, &stored)?;
        transaction.commit()?;
        Ok(projected)
    }

    /// Reconciles the unique canonical terminal for a mapped session/turn.
    ///
    /// `Ok(0)` means that no canonical terminal exists yet or the exact
    /// projection was already durable.
    pub fn project_canonical_terminal_for_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<usize, StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(event) = load_canonical_terminal_for_turn(&transaction, session_id, turn_id)?
        else {
            transaction.commit()?;
            return Ok(0);
        };
        let projected = project_canonical_terminal_in_transaction(&transaction, &event)?;
        transaction.commit()?;
        Ok(projected)
    }

    /// Reconciles one bounded startup page of mapped Started harness runs.
    ///
    /// Callers continue with `next_after_run_id` until it is `None`. Runs whose
    /// canonical turn has not terminalized remain Started and are reconsidered
    /// on the next startup scan; legacy rows without a turn identity are never
    /// guessed.
    pub fn reconcile_started_canonical_terminals_page(
        &self,
        after_run_id: Option<HarnessRunId>,
        limit: usize,
    ) -> Result<HarnessTerminalReconciliationPage, StorageError> {
        if !(1..=MAX_HARNESS_TERMINAL_RECONCILIATION_PAGE_SIZE).contains(&limit) {
            return Err(StorageError::Message(format!(
                "harness terminal reconciliation page size must be between 1 and {MAX_HARNESS_TERMINAL_RECONCILIATION_PAGE_SIZE}"
            )));
        }
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let started_status = serde_json::to_string(&HarnessRunStatus::Started)?;
        let mut statement = transaction.prepare(
            "SELECT id, session_id, protocol_turn_id
             FROM harness_runs
             WHERE status = ?1
               AND completed_at_ms IS NULL
               AND canonical_terminal_runtime_event_id IS NULL
               AND session_id IS NOT NULL
               AND protocol_turn_id IS NOT NULL
               AND (?2 IS NULL OR id > ?2)
             ORDER BY id ASC
             LIMIT ?3",
        )?;
        let rows = statement
            .query_map(
                params![
                    started_status,
                    after_run_id.map(|id| id.to_string()),
                    limit as i64,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);

        let scanned_runs = rows.len();
        let next_after_run_id = if scanned_runs == limit {
            rows.last()
                .map(|(run_id, _, _)| {
                    run_id.parse::<HarnessRunId>().map_err(|error| {
                        StorageError::Message(format!("invalid harness run id `{run_id}`: {error}"))
                    })
                })
                .transpose()?
        } else {
            None
        };
        let mut turns = HashSet::new();
        for (_, session_id, turn_id) in &rows {
            turns.insert((
                session_id.parse::<SessionId>().map_err(|error| {
                    StorageError::Message(format!(
                        "invalid harness session id `{session_id}`: {error}"
                    ))
                })?,
                turn_id.parse::<TurnId>().map_err(|error| {
                    StorageError::Message(format!(
                        "invalid harness protocol turn id `{turn_id}`: {error}"
                    ))
                })?,
            ));
        }
        let mut terminalized_runs = 0usize;
        for (session_id, turn_id) in turns {
            if let Some(event) =
                load_canonical_terminal_for_turn(&transaction, session_id, turn_id)?
            {
                terminalized_runs = terminalized_runs.saturating_add(
                    project_canonical_terminal_in_transaction(&transaction, &event)?,
                );
            }
        }
        transaction.commit()?;
        Ok(HarnessTerminalReconciliationPage {
            scanned_runs,
            terminalized_runs,
            next_after_run_id,
        })
    }
}

impl HarnessRunStore for SqliteHarnessRunStore {
    fn upsert_run(&self, run: &HarnessRunRecord) -> Result<(), StorageError> {
        if run.protocol_turn_id.is_some() && run.session_id.is_none() {
            return Err(StorageError::Message(format!(
                "harness run {} cannot persist a protocol turn without a session",
                run.id
            )));
        }
        if run.canonical_terminal_runtime_event_id.is_some() {
            return Err(StorageError::Message(format!(
                "harness run {} canonical terminal linkage is owned by canonical projection",
                run.id
            )));
        }
        if run.protocol_turn_id.is_some()
            && (run.status != HarnessRunStatus::Started || run.completed_at_ms.is_some())
        {
            return Err(StorageError::Message(format!(
                "mapped harness run {} can only terminalize through canonical projection",
                run.id
            )));
        }
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let changed = connection.execute(
            "INSERT INTO harness_runs
             (id, session_id, protocol_turn_id, canonical_terminal_runtime_event_id,
              workspace_root, artifact_root, mode, started_at_ms, completed_at_ms, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                 protocol_turn_id = COALESCE(
                     harness_runs.protocol_turn_id,
                     excluded.protocol_turn_id
                 ),
                 canonical_terminal_runtime_event_id = COALESCE(
                     harness_runs.canonical_terminal_runtime_event_id,
                     excluded.canonical_terminal_runtime_event_id
                 ),
                 workspace_root = excluded.workspace_root,
                 artifact_root = excluded.artifact_root,
                 mode = excluded.mode,
                 started_at_ms = excluded.started_at_ms,
                 completed_at_ms = CASE
                     WHEN harness_runs.canonical_terminal_runtime_event_id IS NULL
                     THEN excluded.completed_at_ms
                     ELSE harness_runs.completed_at_ms
                 END,
                 status = CASE
                     WHEN harness_runs.canonical_terminal_runtime_event_id IS NULL
                     THEN excluded.status
                     ELSE harness_runs.status
                 END
             WHERE harness_runs.session_id IS excluded.session_id
               AND (
                   harness_runs.protocol_turn_id IS NULL
                   OR excluded.protocol_turn_id IS NULL
                   OR harness_runs.protocol_turn_id = excluded.protocol_turn_id
               )
               AND (
                   harness_runs.canonical_terminal_runtime_event_id IS NULL
                   OR excluded.canonical_terminal_runtime_event_id IS NULL
                   OR harness_runs.canonical_terminal_runtime_event_id =
                      excluded.canonical_terminal_runtime_event_id
               )",
            params![
                run.id.to_string(),
                run.session_id.map(|id| id.to_string()),
                run.protocol_turn_id.map(|id| id.to_string()),
                run.canonical_terminal_runtime_event_id
                    .map(|id| id.to_string()),
                run.workspace_root.as_str(),
                run.artifact_root.as_str(),
                run.mode,
                run.started_at_ms,
                run.completed_at_ms,
                serde_json::to_string(&run.status)?,
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::Message(format!(
                "harness run {} cannot change its durable session, turn, or canonical terminal identity",
                run.id
            )));
        }
        Ok(())
    }

    fn get_run(&self, run_id: HarnessRunId) -> Result<Option<HarnessRunRecord>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection
            .query_row(
                "SELECT session_id, protocol_turn_id,
                        canonical_terminal_runtime_event_id,
                        workspace_root, artifact_root, mode, started_at_ms,
                        completed_at_ms, status
                 FROM harness_runs WHERE id = ?1",
                params![run_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(
                    session_id,
                    protocol_turn_id,
                    canonical_terminal_runtime_event_id,
                    workspace_root,
                    artifact_root,
                    mode,
                    started_at_ms,
                    completed_at_ms,
                    status_json,
                )| {
                    Ok(HarnessRunRecord {
                        id: run_id,
                        session_id: parse_optional_id(session_id, "harness session id")?,
                        protocol_turn_id: parse_optional_id(
                            protocol_turn_id,
                            "harness protocol turn id",
                        )?,
                        canonical_terminal_runtime_event_id: parse_optional_id(
                            canonical_terminal_runtime_event_id,
                            "canonical terminal runtime event id",
                        )?,
                        workspace_root: workspace_root.into(),
                        artifact_root: artifact_root.into(),
                        mode,
                        started_at_ms,
                        completed_at_ms,
                        status: serde_json::from_str(&status_json)?,
                    })
                },
            )
            .transpose()
    }
}

fn parse_optional_id<T>(value: Option<String>, label: &str) -> Result<Option<T>, StorageError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .map(|value| {
            value.parse::<T>().map_err(|error| {
                StorageError::Message(format!("invalid {label} `{value}`: {error}"))
            })
        })
        .transpose()
}

fn load_canonical_terminal_by_id(
    connection: &Connection,
    event_id: RuntimeEventId,
) -> Result<Option<RuntimeEvent>, StorageError> {
    load_canonical_terminal(
        connection,
        "runtime_event.id = ?1",
        &[&event_id.to_string()],
    )
}

fn load_canonical_terminal_for_turn(
    connection: &Connection,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<Option<RuntimeEvent>, StorageError> {
    load_canonical_terminal(
        connection,
        "runtime_event.session_id = ?1 AND runtime_event.turn_id = ?2",
        &[&session_id.to_string(), &turn_id.to_string()],
    )
}

fn load_canonical_terminal(
    connection: &Connection,
    predicate: &str,
    query_params: &[&dyn rusqlite::ToSql],
) -> Result<Option<RuntimeEvent>, StorageError> {
    let sql = format!(
        "SELECT runtime_event.id, runtime_event.session_id, runtime_event.turn_id,
                runtime_event.sequence_no, runtime_event.msg_json,
                runtime_event.payload_sha256, runtime_event.created_at_ms
         FROM protocol_runtime_events AS runtime_event
         INNER JOIN protocol_item_append_order AS append_order
           ON append_order.session_id = runtime_event.session_id
          AND append_order.scope_kind = 'turn'
          AND append_order.turn_id = runtime_event.turn_id
          AND append_order.sequence_no = runtime_event.sequence_no
          AND append_order.source_kind = 'runtime_event'
          AND append_order.source_id = runtime_event.id
         WHERE {predicate}
           AND json_extract(runtime_event.msg_json, '$.kind') = 'turn_terminal'"
    );
    let raw = connection
        .query_row(&sql, query_params, |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .optional()?;
    let Some((event_id, session_id, turn_id, sequence_no, msg_json, payload_sha256, created_at_ms)) =
        raw
    else {
        return Ok(None);
    };
    if hash_bytes(msg_json.as_bytes()) != payload_sha256 {
        return Err(StorageError::Message(format!(
            "canonical runtime event {event_id} has a payload hash mismatch"
        )));
    }
    let msg: RuntimeEventMsg = serde_json::from_str(&msg_json)?;
    if !matches!(msg, RuntimeEventMsg::TurnTerminal { .. }) {
        return Err(StorageError::Message(format!(
            "canonical runtime event {event_id} is not a terminal event"
        )));
    }
    Ok(Some(RuntimeEvent {
        id: event_id.parse::<RuntimeEventId>().map_err(|error| {
            StorageError::Message(format!(
                "invalid canonical runtime event id `{event_id}`: {error}"
            ))
        })?,
        session_id: session_id.parse::<SessionId>().map_err(|error| {
            StorageError::Message(format!(
                "invalid canonical runtime event session id `{session_id}`: {error}"
            ))
        })?,
        turn_id: turn_id.parse::<TurnId>().map_err(|error| {
            StorageError::Message(format!(
                "invalid canonical runtime event turn id `{turn_id}`: {error}"
            ))
        })?,
        sequence_no,
        created_at_ms,
        msg,
    }))
}

fn require_same_runtime_event(
    provided: &RuntimeEvent,
    stored: &RuntimeEvent,
) -> Result<(), StorageError> {
    let same = provided.id == stored.id
        && provided.session_id == stored.session_id
        && provided.turn_id == stored.turn_id
        && provided.sequence_no == stored.sequence_no
        && provided.created_at_ms == stored.created_at_ms
        && serde_json::to_value(&provided.msg)? == serde_json::to_value(&stored.msg)?;
    if same {
        return Ok(());
    }
    Err(StorageError::Message(format!(
        "runtime event {} does not match its canonical durable protocol row",
        provided.id
    )))
}

fn project_canonical_terminal_in_transaction(
    transaction: &Transaction<'_>,
    event: &RuntimeEvent,
) -> Result<usize, StorageError> {
    let RuntimeEventMsg::TurnTerminal { terminal } = &event.msg else {
        return Err(StorageError::Message(format!(
            "runtime event {} is not terminal",
            event.id
        )));
    };
    let expected_status = HarnessRunStatus::from_terminal_outcome(&terminal.outcome);
    let expected_status_json = serde_json::to_string(&expected_status)?;
    let started_status_json = serde_json::to_string(&HarnessRunStatus::Started)?;
    let mut statement = transaction.prepare(
        "SELECT id, status, completed_at_ms, canonical_terminal_runtime_event_id
         FROM harness_runs
         WHERE session_id = ?1 AND protocol_turn_id = ?2
         ORDER BY id ASC",
    )?;
    let rows = statement
        .query_map(
            params![event.session_id.to_string(), event.turn_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    let terminal_payload =
        HarnessEventPayload::generic(serde_json::to_value(RunEvent::TurnTerminal {
            session_id: event.session_id,
            terminal: terminal.clone(),
        })?);
    let mut projected = 0usize;
    for (run_id, status_json, completed_at_ms, canonical_terminal_id) in rows {
        match canonical_terminal_id {
            Some(existing_terminal_id)
                if existing_terminal_id == event.id.to_string()
                    && status_json == expected_status_json
                    && completed_at_ms == Some(event.created_at_ms) =>
            {
                validate_existing_terminal_projection(transaction, &run_id, &terminal_payload)?;
            }
            Some(existing_terminal_id) => {
                return Err(StorageError::Message(format!(
                    "harness run {run_id} is linked to divergent canonical terminal {existing_terminal_id}"
                )));
            }
            None if status_json == started_status_json && completed_at_ms.is_none() => {
                let sequence_no = transaction.query_row(
                    "SELECT COALESCE(MAX(sequence_no), -1) + 1
                     FROM harness_events WHERE run_id = ?1",
                    [&run_id],
                    |row| row.get::<_, i64>(0),
                )?;
                let parsed_run_id = run_id.parse::<HarnessRunId>().map_err(|error| {
                    StorageError::Message(format!("invalid harness run id `{run_id}`: {error}"))
                })?;
                insert_event_in_connection(
                    transaction,
                    &HarnessEvent {
                        id: HarnessEventId::new(),
                        run_id: parsed_run_id,
                        sequence_no,
                        created_at_ms: event.created_at_ms,
                        kind: HarnessEventKind::RunTerminalized,
                        payload: terminal_payload.clone(),
                        contract_refs: Vec::new(),
                        artifact_refs: Vec::new(),
                        parent_event_id: None,
                    },
                )?;
                let updated = transaction.execute(
                    "UPDATE harness_runs
                     SET completed_at_ms = ?1, status = ?2,
                         canonical_terminal_runtime_event_id = ?3
                     WHERE id = ?4 AND status = ?5
                       AND completed_at_ms IS NULL
                       AND canonical_terminal_runtime_event_id IS NULL",
                    params![
                        event.created_at_ms,
                        expected_status_json,
                        event.id.to_string(),
                        run_id,
                        started_status_json,
                    ],
                )?;
                if updated != 1 {
                    return Err(StorageError::Message(format!(
                        "harness run {run_id} changed while projecting canonical terminal {}",
                        event.id
                    )));
                }
                projected = projected.saturating_add(1);
            }
            None => {
                return Err(StorageError::Message(format!(
                    "harness run {run_id} is terminal without canonical terminal linkage"
                )));
            }
        }
    }
    Ok(projected)
}

fn validate_existing_terminal_projection(
    transaction: &Transaction<'_>,
    run_id: &str,
    expected_payload: &HarnessEventPayload,
) -> Result<(), StorageError> {
    let expected_kind = serde_json::to_string(&HarnessEventKind::RunTerminalized)?;
    let expected_payload = serde_json::to_string(expected_payload)?;
    let matches = transaction.query_row(
        "SELECT COUNT(*)
         FROM harness_events
         WHERE run_id = ?1 AND kind = ?2 AND payload_json = ?3",
        params![run_id, expected_kind, expected_payload],
        |row| row.get::<_, i64>(0),
    )?;
    if matches == 1 {
        return Ok(());
    }
    Err(StorageError::Message(format!(
        "harness run {run_id} does not own exactly one terminal event matching canonical truth"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AccessMode;
    use crate::harness::HarnessEventStore;
    use crate::protocol::TurnInterruptionCause;
    use crate::session::{
        DurableTurnTerminal, NewSession, ProjectId, ProjectRepository, RunMetrics,
        SessionRepository,
    };
    use crate::storage::{SqliteStore, StoragePaths};

    async fn test_store() -> (tempfile::TempDir, SqliteStore, SessionId) {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir =
            Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data path");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let store = SqliteStore::open(&paths).expect("open store");
        store.migrate().expect("migrate store");
        let project_id = ProjectId::new();
        store
            .project_repo()
            .upsert_project(project_id, &data_dir, "project", "none")
            .await
            .expect("project");
        let session = store
            .session_repo()
            .create_session(NewSession {
                project_id,
                title: "session".to_string(),
                cwd: data_dir,
                model: "model".to_string(),
                base_url: "http://localhost:1234".to_string(),
                access_mode: AccessMode::Default,
            })
            .await
            .expect("session");
        (temp, store, session.id)
    }

    fn started_run(id: HarnessRunId, session_id: SessionId, turn_id: TurnId) -> HarnessRunRecord {
        HarnessRunRecord {
            id,
            session_id: Some(session_id),
            protocol_turn_id: Some(turn_id),
            canonical_terminal_runtime_event_id: None,
            workspace_root: "C:/workspace".into(),
            artifact_root: Utf8PathBuf::from(format!("C:/artifacts/{id}")),
            mode: "native_runtime".to_string(),
            started_at_ms: 10,
            completed_at_ms: None,
            status: HarnessRunStatus::Started,
        }
    }

    fn terminal_event(
        session_id: SessionId,
        turn_id: TurnId,
        outcome: TurnTerminalOutcome,
    ) -> RuntimeEvent {
        RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no: 0,
            created_at_ms: 42,
            msg: RuntimeEventMsg::TurnTerminal {
                terminal: Box::new(DurableTurnTerminal {
                    outcome,
                    final_response_id: None,
                    tool_call_count: 3,
                    failed_tool_count: 1,
                    change_count: 2,
                    metrics: RunMetrics::default(),
                }),
            },
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn canonical_terminal_projects_every_matching_started_run_once() {
        let (_temp, store, session_id) = test_store().await;
        let turn_id = TurnId::new();
        let other_turn_id = TurnId::new();
        let run_store = store.harness_run_store();
        let event_store = store.harness_event_store();
        let matching_ids = [HarnessRunId::new(), HarnessRunId::new()];
        for run_id in matching_ids {
            run_store
                .upsert_run(&started_run(run_id, session_id, turn_id))
                .expect("matching run");
        }
        let other_run_id = HarnessRunId::new();
        run_store
            .upsert_run(&started_run(other_run_id, session_id, other_turn_id))
            .expect("other run");
        let terminal = terminal_event(session_id, turn_id, TurnTerminalOutcome::Completed);
        store
            .protocol_event_store()
            .seed_runtime_event_for_test(&terminal)
            .expect("canonical terminal");

        assert_eq!(
            run_store
                .project_canonical_terminal_event(&terminal)
                .expect("first projection"),
            2
        );
        assert_eq!(
            run_store
                .project_canonical_terminal_event(&terminal)
                .expect("idempotent projection"),
            0
        );

        for run_id in matching_ids {
            let run = run_store
                .get_run(run_id)
                .expect("get run")
                .expect("stored run");
            assert_eq!(run.status, HarnessRunStatus::Pass);
            assert_eq!(run.completed_at_ms, Some(terminal.created_at_ms));
            assert_eq!(run.canonical_terminal_runtime_event_id, Some(terminal.id));
            let events = event_store.list_events(run_id).expect("terminal events");
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].kind, HarnessEventKind::RunTerminalized);
            assert!(events[0].artifact_refs.is_empty());

            let mut stale_writer_state = started_run(run_id, session_id, turn_id);
            stale_writer_state.started_at_ms = run.started_at_ms;
            run_store
                .upsert_run(&stale_writer_state)
                .expect("stale writer cannot erase canonical truth");
            assert_eq!(
                run_store
                    .get_run(run_id)
                    .expect("get preserved run")
                    .expect("preserved run")
                    .canonical_terminal_runtime_event_id,
                Some(terminal.id)
            );
        }
        assert_eq!(
            run_store
                .get_run(other_run_id)
                .expect("get other run")
                .expect("other run")
                .status,
            HarnessRunStatus::Started
        );
        assert!(
            event_store
                .list_events(other_run_id)
                .expect("other events")
                .is_empty()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminal_projection_rolls_back_event_when_run_update_fails() {
        let (_temp, store, session_id) = test_store().await;
        let turn_id = TurnId::new();
        let run_store = store.harness_run_store();
        let run_id = HarnessRunId::new();
        run_store
            .upsert_run(&started_run(run_id, session_id, turn_id))
            .expect("run");
        let terminal = terminal_event(
            session_id,
            turn_id,
            TurnTerminalOutcome::Interrupted {
                cause: TurnInterruptionCause::UserStop,
            },
        );
        store
            .protocol_event_store()
            .seed_runtime_event_for_test(&terminal)
            .expect("canonical terminal");
        run_store
            .connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch(
                "CREATE TRIGGER reject_harness_terminal_link
                 BEFORE UPDATE OF canonical_terminal_runtime_event_id ON harness_runs
                 WHEN NEW.canonical_terminal_runtime_event_id IS NOT NULL
                 BEGIN
                     SELECT RAISE(FAIL, 'terminal link rejected');
                 END;",
            )
            .expect("failure trigger");

        let error = run_store
            .project_canonical_terminal_for_turn(session_id, turn_id)
            .expect_err("projection must fail");
        assert!(error.to_string().contains("terminal link rejected"));
        let run = run_store
            .get_run(run_id)
            .expect("get run")
            .expect("stored run");
        assert_eq!(run.status, HarnessRunStatus::Started);
        assert_eq!(run.completed_at_ms, None);
        assert_eq!(run.canonical_terminal_runtime_event_id, None);
        assert!(
            store
                .harness_event_store()
                .list_events(run_id)
                .expect("events")
                .is_empty()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn projection_rejects_a_noncanonical_or_mutated_terminal() {
        let (_temp, store, session_id) = test_store().await;
        let turn_id = TurnId::new();
        let run_store = store.harness_run_store();
        let run_id = HarnessRunId::new();
        run_store
            .upsert_run(&started_run(run_id, session_id, turn_id))
            .expect("run");
        let terminal = terminal_event(
            session_id,
            turn_id,
            TurnTerminalOutcome::Failed {
                error: "canonical failure".to_string(),
            },
        );
        let missing_error = run_store
            .project_canonical_terminal_event(&terminal)
            .expect_err("unstored event must fail");
        assert!(missing_error.to_string().contains("not canonical"));
        store
            .protocol_event_store()
            .seed_runtime_event_for_test(&terminal)
            .expect("canonical terminal");
        let mut mutated = terminal.clone();
        mutated.created_at_ms += 1;
        let mismatch_error = run_store
            .project_canonical_terminal_event(&mutated)
            .expect_err("mutated event must fail");
        assert!(mismatch_error.to_string().contains("does not match"));
        assert_eq!(
            run_store
                .get_run(run_id)
                .expect("get run")
                .expect("stored run")
                .status,
            HarnessRunStatus::Started
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn startup_reconciliation_pages_through_started_mapped_runs() {
        let (_temp, store, session_id) = test_store().await;
        let run_store = store.harness_run_store();
        let mut run_ids = Vec::new();
        for outcome in [
            TurnTerminalOutcome::Completed,
            TurnTerminalOutcome::Interrupted {
                cause: TurnInterruptionCause::UserStop,
            },
            TurnTerminalOutcome::Failed {
                error: "failed".to_string(),
            },
        ] {
            let turn_id = TurnId::new();
            let run_id = HarnessRunId::new();
            run_ids.push(run_id);
            run_store
                .upsert_run(&started_run(run_id, session_id, turn_id))
                .expect("started run");
            store
                .protocol_event_store()
                .seed_runtime_event_for_test(&terminal_event(session_id, turn_id, outcome))
                .expect("canonical terminal");
        }
        let legacy_run_id = HarnessRunId::new();
        let mut legacy = started_run(legacy_run_id, session_id, TurnId::new());
        legacy.protocol_turn_id = None;
        run_store.upsert_run(&legacy).expect("legacy run");

        let mut after = None;
        let mut scanned = 0usize;
        let mut terminalized = 0usize;
        loop {
            let page = run_store
                .reconcile_started_canonical_terminals_page(after, 1)
                .expect("reconciliation page");
            scanned = scanned.saturating_add(page.scanned_runs);
            terminalized = terminalized.saturating_add(page.terminalized_runs);
            let Some(next) = page.next_after_run_id else {
                break;
            };
            after = Some(next);
        }
        assert_eq!(terminalized, run_ids.len());
        assert!(scanned >= run_ids.len());
        for run_id in run_ids {
            assert_ne!(
                run_store
                    .get_run(run_id)
                    .expect("get run")
                    .expect("run")
                    .status,
                HarnessRunStatus::Started
            );
        }
        assert_eq!(
            run_store
                .get_run(legacy_run_id)
                .expect("get legacy")
                .expect("legacy")
                .status,
            HarnessRunStatus::Started
        );
        assert!(
            run_store
                .reconcile_started_canonical_terminals_page(None, 0)
                .expect_err("zero page size must fail")
                .to_string()
                .contains("page size")
        );
    }
}
