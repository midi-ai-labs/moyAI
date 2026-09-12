//! Read-only MCP audit projection. Existing local turns and durable remote locators
//! remain the owners; opening this view never contacts a peer or resumes work.

use std::collections::{BTreeMap, HashSet};

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ulid::Ulid;

use super::runtime::{RemoteJobService, RemoteJobState};
use super::store::{RemoteJobStore, StoredDeviceReference, StoredRemoteJob, query_job};
use crate::error::StorageError;
use crate::mcp_publish::dispatch::PublishCallError;
use crate::protocol::{ContentPart, HistoryItemPayload, TurnId, TurnItemPayload};
use crate::session::{SessionId, SessionRepository};

const MAX_PAGE: usize = 100;
const EDGE_ITEMS: usize = 128;
const MAX_ITEM_BYTES: usize = 256 * 1024;
const MAX_READ_BYTES: usize = 2 * 1024 * 1024;
const MAX_MARKDOWN_BYTES: usize = 1024 * 1024;
pub(crate) const HUB_MARKDOWN_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailView {
    Desktop,
    HubRecent,
}

impl DetailView {
    fn orders(self) -> [&'static str; 2] {
        match self {
            Self::Desktop => ["ASC", "DESC"],
            Self::HubRecent => ["DESC", "ASC"],
        }
    }
}

mod recent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpHistoryDirection {
    Instruction,
    Execution,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpHistoryRow {
    pub id: String,
    pub direction: McpHistoryDirection,
    pub created_at_ms: i64,
    pub updated_at_ms: Option<i64>,
    pub title: String,
    pub peer_label: String,
    pub target_label: String,
    pub state: String,
    pub stop_status: String,
    /// Outgoing state is the persisted last response, never an online assertion.
    pub state_source: String,
    /// A result present on this computer; execution completion alone is not delivery.
    pub result_received: bool,
    pub session_id: String,
    pub profile_id: String,
    pub job_id: Option<String>,
    pub root_task_id: String,
    pub device_path: Vec<String>,
    pub can_stop: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpHistoryPage {
    pub rows: Vec<McpHistoryRow>,
    pub next_offset: Option<usize>,
    pub anchor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpHistoryDetail {
    pub row: McpHistoryRow,
    pub markdown: String,
    pub truncated: bool,
}

impl RemoteJobService {
    pub async fn history_page(
        &self,
        direction: McpHistoryDirection,
        offset: usize,
        limit: usize,
        anchor: Option<&str>,
    ) -> Result<McpHistoryPage, PublishCallError> {
        let limit = limit.clamp(1, MAX_PAGE);
        let offset_i64 = i64::try_from(offset).map_err(|_| PublishCallError::InvalidArguments)?;
        let anchor = anchor
            .map(|text| HistoryAnchor::parse(text, direction))
            .transpose()?;
        self.submit(move |service| async move {
            let store = service.history_store().remote_job_store();
            let anchor = match anchor {
                Some(anchor) => Some(anchor),
                None => store.history_anchor(direction).map_err(unavailable)?,
            };
            let Some(anchor) = anchor else {
                return Ok(McpHistoryPage {
                    rows: vec![],
                    next_offset: None,
                    anchor: None,
                });
            };
            let mut rows = Vec::new();
            match direction {
                McpHistoryDirection::Instruction => {
                    for (reference, created) in store
                        .history_references(offset_i64, limit + 1, &anchor)
                        .map_err(unavailable)?
                    {
                        rows.push(instruction_row(&reference, created));
                    }
                }
                McpHistoryDirection::Execution => {
                    for job in store
                        .history_jobs(offset_i64, limit + 1, &anchor)
                        .map_err(unavailable)?
                    {
                        rows.push(service.execution_history_row(&job).await?);
                    }
                }
            }
            let next_offset = (rows.len() > limit).then(|| offset + limit);
            rows.truncate(limit);
            Ok(McpHistoryPage {
                rows,
                next_offset,
                anchor: Some(anchor.encode()),
            })
        })
        .await
    }

    pub async fn history_detail(
        &self,
        direction: McpHistoryDirection,
        id: &str,
    ) -> Result<McpHistoryDetail, PublishCallError> {
        self.history_detail_view(direction, id, DetailView::Desktop)
            .await
    }

    /// Keep the existing wire shape while prioritizing recent, exact-target evidence.
    pub(crate) async fn history_detail_for_hub(
        &self,
        direction: McpHistoryDirection,
        id: &str,
    ) -> Result<McpHistoryDetail, PublishCallError> {
        self.history_detail_view(direction, id, DetailView::HubRecent)
            .await
    }

    async fn history_detail_view(
        &self,
        direction: McpHistoryDirection,
        id: &str,
        view: DetailView,
    ) -> Result<McpHistoryDetail, PublishCallError> {
        let id = id
            .parse::<Ulid>()
            .map_err(|_| PublishCallError::InvalidArguments)?;
        self.submit(move |service| async move {
            let store = service.history_store().remote_job_store();
            let (mut row, evidence, result) = match direction {
                McpHistoryDirection::Instruction => {
                    let (reference, created) = store
                        .history_reference(id)
                        .map_err(unavailable)?
                        .ok_or(PublishCallError::InvalidTarget)?;
                    let row = instruction_row(&reference, created);
                    let evidence = store
                        .history_evidence(
                            reference.session_id,
                            Some(reference.turn_id),
                            Some(&reference),
                            view,
                        )
                        .map_err(unavailable)?;
                    (row, evidence, reference.result)
                }
                McpHistoryDirection::Execution => {
                    let job = store
                        .history_job(id)
                        .map_err(unavailable)?
                        .ok_or(PublishCallError::InvalidTarget)?;
                    let row = service.execution_history_row(&job).await?;
                    let evidence = store
                        .history_evidence(job.session_id, job.admitted_turn_id, None, view)
                        .map_err(unavailable)?;
                    // Final assistant messages and the terminal are already in exact-turn history.
                    (row, evidence, None)
                }
            };
            row.updated_at_ms = evidence.last_observed_at_ms.or(row.updated_at_ms);
            let (markdown, truncated) = match view {
                DetailView::Desktop => render_detail(&row, &evidence, result.as_deref()),
                DetailView::HubRecent => recent::render(&row, &evidence, result.as_deref()),
            };
            Ok(McpHistoryDetail {
                row,
                markdown,
                truncated,
            })
        })
        .await
    }

    async fn execution_history_row(
        &self,
        job: &StoredRemoteJob,
    ) -> Result<McpHistoryRow, PublishCallError> {
        let runtime = self.row(job.clone()).await?;
        let store = self.history_store();
        let terminal = match job.admitted_turn_id {
            Some(turn) => store
                .session_repo()
                .durable_terminal_for_turn(job.session_id, turn)
                .await
                .map_err(unavailable)?,
            None => None,
        };
        let state = if runtime.state == RemoteJobState::Interrupted && terminal.is_none() {
            "unknown".into()
        } else {
            serde_json::to_value(runtime.state)
                .map_err(unavailable)?
                .as_str()
                .unwrap_or("unknown")
                .to_owned()
        };
        let stop_status = match (state.as_str(), terminal.as_ref()) {
            ("interrupted", Some(_)) => "confirmed",
            ("cancelling", _) => "requested",
            ("unknown", _) => "unconfirmed",
            _ => "none",
        }
        .to_owned();
        let updated_at_ms = store
            .remote_job_store()
            .history_last_timestamp(job.session_id, job.admitted_turn_id)
            .map_err(unavailable)?;
        Ok(McpHistoryRow {
            id: job.id.to_string(),
            direction: McpHistoryDirection::Execution,
            created_at_ms: job.created_at_ms,
            updated_at_ms,
            title: short(&job.prompt_preview, 160),
            peer_label: runtime
                .network
                .as_ref()
                .map(|network| network.actor_device_id.clone())
                .unwrap_or_else(|| job.parent.peer_id.clone()),
            target_label: target_label(&job.scope_json),
            state,
            stop_status,
            state_source: "local_runtime".into(),
            result_received: false,
            session_id: job.session_id.to_string(),
            profile_id: job.profile_id.to_string(),
            job_id: Some(job.id.to_string()),
            root_task_id: runtime
                .network
                .as_ref()
                .map(|network| network.root_task_id.clone())
                .unwrap_or_else(|| job.parent.task_id.clone()),
            device_path: runtime
                .network
                .map(|network| network.device_path)
                .unwrap_or_default(),
            can_stop: runtime.can_stop,
        })
    }
}

fn unavailable(_: impl std::fmt::Display) -> PublishCallError {
    PublishCallError::Unavailable
}

fn instruction_row(reference: &StoredDeviceReference, created_at_ms: i64) -> McpHistoryRow {
    McpHistoryRow {
        id: reference.id.to_string(),
        direction: McpHistoryDirection::Instruction,
        created_at_ms,
        // V63 predates an observation timestamp. Never substitute the current clock.
        updated_at_ms: None,
        title: format!("{} へのタスク", short(&reference.peer.label, 100)),
        peer_label: short(&reference.peer.label, 160),
        target_label: short(&reference.peer.name, 160),
        state: reference.state.clone(),
        stop_status: reference.stop_status.clone(),
        state_source: "last_observed".into(),
        result_received: reference.result.is_some(),
        session_id: reference.session_id.to_string(),
        profile_id: reference.profile_id.clone(),
        job_id: reference.job_id.clone(),
        root_task_id: reference.root_task_id.clone(),
        device_path: reference
            .claims
            .as_ref()
            .map(|claims| claims.device_path.clone())
            .unwrap_or_else(|| vec![reference.device_id.clone()]),
        can_stop: !matches!(
            reference.state.as_str(),
            "completed" | "failed" | "interrupted"
        ),
    }
}

fn target_label(scope: &str) -> String {
    let value = serde_json::from_str::<Value>(scope).unwrap_or(Value::Null);
    let receiver = value
        .get("receiver")
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let target = receiver
        .as_ref()
        .unwrap_or(&value)
        .get("target")
        .unwrap_or(&Value::Null);
    if target.get("kind").and_then(Value::as_str) == Some("temp") {
        return "temp".into();
    }
    target
        .get("workspace_root")
        .and_then(Value::as_str)
        .map(|text| short(text, 240))
        .unwrap_or_else(|| "保存済みの受付対象".into())
}

#[derive(Default)]
struct Evidence {
    history: Vec<EvidenceItem>,
    progress: Vec<EvidenceItem>,
    waits: Vec<WaitEvidence>,
    last_observed_at_ms: Option<i64>,
    truncated: bool,
}
struct EvidenceItem {
    sequence: i64,
    at_ms: Option<i64>,
    payload: Option<String>,
}

struct WaitEvidence {
    sequence: i64,
    at_ms: i64,
    turn_id: String,
    is_output: bool,
    text: String,
}

struct HistoryAnchor {
    direction: McpHistoryDirection,
    max_rowid: i64,
}
impl HistoryAnchor {
    fn parse(text: &str, direction: McpHistoryDirection) -> Result<Self, PublishCallError> {
        let (kind, position) = text
            .split_once(':')
            .ok_or(PublishCallError::InvalidArguments)?;
        let max_rowid = position
            .parse::<i64>()
            .map_err(|_| PublishCallError::InvalidArguments)?;
        if max_rowid <= 0 || kind != Self::kind(direction) {
            return Err(PublishCallError::InvalidArguments);
        }
        Ok(Self {
            direction,
            max_rowid,
        })
    }
    fn encode(&self) -> String {
        format!("{}:{}", Self::kind(self.direction), self.max_rowid)
    }
    fn kind(direction: McpHistoryDirection) -> &'static str {
        match direction {
            McpHistoryDirection::Instruction => "instruction",
            McpHistoryDirection::Execution => "execution",
        }
    }
}

impl RemoteJobStore {
    fn history_anchor(
        &self,
        direction: McpHistoryDirection,
    ) -> Result<Option<HistoryAnchor>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let table = match direction {
            McpHistoryDirection::Instruction => "device_outgoing_references",
            McpHistoryDirection::Execution => "remote_agent_jobs",
        };
        // Receipt rows are retained. An append fence also excludes later receipts
        // whose random ULID suffix sorts before existing same-millisecond rows.
        let position: Option<i64> =
            connection.query_row(&format!("SELECT MAX(rowid) FROM {table}"), [], |row| {
                row.get(0)
            })?;
        Ok(position.map(|max_rowid| HistoryAnchor {
            direction,
            max_rowid,
        }))
    }
    fn history_references(
        &self,
        offset: i64,
        limit: usize,
        anchor: &HistoryAnchor,
    ) -> Result<Vec<(StoredDeviceReference, i64)>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare("SELECT payload_json,created_at_ms FROM device_outgoing_references WHERE rowid<=?3 ORDER BY created_at_ms DESC,id DESC LIMIT ?1 OFFSET ?2")?;
        let raw = statement
            .query_map(params![limit as i64, offset, anchor.max_rowid], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        raw.into_iter()
            .map(|(text, created)| Ok((StoredDeviceReference::decode(&text)?, created)))
            .collect()
    }
    fn history_reference(
        &self,
        id: Ulid,
    ) -> Result<Option<(StoredDeviceReference, i64)>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let raw = connection
            .query_row(
                "SELECT payload_json,created_at_ms FROM device_outgoing_references WHERE id=?1",
                params![id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        raw.map(|(text, created)| Ok((StoredDeviceReference::decode(&text)?, created)))
            .transpose()
    }
    fn history_jobs(
        &self,
        offset: i64,
        limit: usize,
        anchor: &HistoryAnchor,
    ) -> Result<Vec<StoredRemoteJob>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare("SELECT id FROM remote_agent_jobs WHERE rowid<=?3 ORDER BY created_at_ms DESC,id DESC LIMIT ?1 OFFSET ?2")?;
        let ids = statement
            .query_map(params![limit as i64, offset, anchor.max_rowid], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                query_job(&connection, "id=?1", params![id])?
                    .ok_or_else(|| StorageError::Message("remote history disappeared".into()))
            })
            .collect()
    }
    fn history_job(&self, id: Ulid) -> Result<Option<StoredRemoteJob>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        query_job(&connection, "id=?1", params![id.to_string()])
    }
    fn history_last_timestamp(
        &self,
        session: SessionId,
        turn: Option<TurnId>,
    ) -> Result<Option<i64>, StorageError> {
        let Some(turn) = turn else {
            return Ok(None);
        };
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        Ok(connection.query_row("SELECT MAX(created_at_ms) FROM protocol_history_items WHERE session_id=?1 AND turn_id=?2", params![session.to_string(),turn.to_string()], |row| row.get(0))?)
    }

    fn history_evidence(
        &self,
        session: SessionId,
        turn: Option<TurnId>,
        reference: Option<&StoredDeviceReference>,
        view: DetailView,
    ) -> Result<Evidence, StorageError> {
        let Some(turn) = turn else {
            return Ok(Evidence::default());
        };
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut evidence = Evidence::default();
        let mut remaining = MAX_READ_BYTES;
        if view == DetailView::HubRecent {
            remaining = MAX_READ_BYTES / 8;
        }
        let call_ids = if let Some(reference) = reference {
            let candidates = evidence_edges(
                &connection,
                session,
                turn,
                "protocol_history_items",
                "json_extract(payload_json,'$.kind')='tool_call'",
                "[]",
                &mut remaining,
                &mut evidence.truncated,
                view,
            )?;
            candidates
                .iter()
                .filter_map(|item| {
                    let payload =
                        serde_json::from_str::<HistoryItemPayload>(item.payload.as_deref()?)
                            .ok()?;
                    if let HistoryItemPayload::ToolCall {
                        call_id,
                        arguments_json,
                        tool_name,
                        ..
                    } = payload
                    {
                        if tool_name == "mcp_call" && matching_call(reference, &arguments_json) {
                            return Some(call_id.to_string());
                        }
                    }
                    None
                })
                .collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };
        let call_ids_json = serde_json::to_string(&call_ids)?;
        // Exact local turn plus exact outgoing call identities: sibling delegations and
        // unrelated local shell/model work in the same instruction turn stay private.
        let history_filter = if reference.is_some() {
            "(json_extract(payload_json,'$.kind') IN ('user_turn','steer_turn') OR json_extract(payload_json,'$.call_id') IN (SELECT value FROM json_each(?3)))"
        } else {
            "1=1"
        };
        if view == DetailView::HubRecent {
            remaining = MAX_READ_BYTES / 2;
        }
        evidence.history = evidence_edges(
            &connection,
            session,
            turn,
            "protocol_history_items",
            history_filter,
            &call_ids_json,
            &mut remaining,
            &mut evidence.truncated,
            view,
        )?;
        let progress_filter = if reference.is_some() {
            "json_extract(payload_json,'$.call_id') IN (SELECT value FROM json_each(?3))"
        } else {
            "json_extract(payload_json,'$.kind') IN ('plan','tool_status','approval_request','warning','error','terminal','durable_feedback')"
        };
        if view == DetailView::HubRecent {
            remaining = MAX_READ_BYTES / 8;
        }
        evidence.progress = evidence_edges(
            &connection,
            session,
            turn,
            "protocol_turn_items",
            progress_filter,
            &call_ids_json,
            &mut remaining,
            &mut evidence.truncated,
            view,
        )?;
        if let Some(reference) = reference {
            if view == DetailView::HubRecent {
                remaining = MAX_READ_BYTES / 4;
            }
            evidence.waits = wait_evidence(
                &connection,
                reference,
                &mut remaining,
                &mut evidence.truncated,
                view,
            )?;
        }
        evidence.last_observed_at_ms = evidence
            .history
            .iter()
            .filter(|item| {
                reference.is_none()
                    || item
                        .payload
                        .as_deref()
                        .and_then(|text| serde_json::from_str::<HistoryItemPayload>(text).ok())
                        .is_some_and(|payload| {
                            matches!(payload, HistoryItemPayload::ToolOutput { .. })
                        })
            })
            .filter_map(|item| item.at_ms)
            .chain(
                evidence
                    .waits
                    .iter()
                    .filter(|item| item.is_output)
                    .map(|item| item.at_ms),
            )
            .max();
        Ok(evidence)
    }
}

/// A later local turn can observe an earlier delegated job. Read only exact
/// wait call identities from the same session, and project their multi-job
/// payloads before rendering: sibling results never enter this export.
fn wait_evidence(
    connection: &rusqlite::Connection,
    reference: &StoredDeviceReference,
    remaining: &mut usize,
    truncated: &mut bool,
    view: DetailView,
) -> Result<Vec<WaitEvidence>, StorageError> {
    let Some(job_id) = reference.job_id.as_deref() else {
        return Ok(Vec::new());
    };
    let base = "FROM protocol_history_items WHERE session_id=?1 AND scope_kind='turn'
        AND json_extract(payload_json,'$.kind')='tool_call'
        AND json_extract(payload_json,'$.tool_name')='wait_remote_tasks'
        AND EXISTS (SELECT 1 FROM json_each(CASE
            WHEN json_valid(json_extract(payload_json,'$.arguments_json'))
            THEN CASE WHEN json_type(json_extract(payload_json,'$.arguments_json'),'$.job_ids')='array'
                THEN json_extract(json_extract(payload_json,'$.arguments_json'),'$.job_ids') ELSE '[]' END
            ELSE '[]' END) requested WHERE requested.type='text' AND requested.value=?2)";
    let count: i64 = connection.query_row(
        &format!("SELECT COUNT(*) {base}"),
        params![reference.session_id.to_string(), job_id],
        |row| row.get(0),
    )?;
    // Each selected call has at most one output. Bound their combined edges to
    // the same record budget as other history; do not join every session row
    // back to every candidate call in a long-running conversation.
    let call_edge = EDGE_ITEMS / 2;
    *truncated |= count > (call_edge * 2) as i64;
    let mut items = BTreeMap::new();
    let mut calls = Vec::new();
    // A large call must not consume the entire budget before its latest output.
    let call_budget = *remaining / 4;
    let mut call_remaining = call_budget;
    for order in view.orders() {
        let mut statement = connection.prepare(&format!(
            "SELECT id,sequence_no,created_at_ms,turn_id,
            CASE WHEN length(CAST(payload_json AS BLOB))<=?3 THEN payload_json ELSE NULL END,
            json_extract(payload_json,'$.call_id') {base}
            ORDER BY created_at_ms {order},turn_id {order},sequence_no {order},id {order} LIMIT ?4"
        ))?;
        let mut rows = statement.query(params![
            reference.session_id.to_string(),
            job_id,
            MAX_ITEM_BYTES as i64,
            call_edge as i64
        ])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let sequence: i64 = row.get(1)?;
            let at_ms: i64 = row.get(2)?;
            let turn_id: String = row.get(3)?;
            let key = (at_ms, turn_id.clone(), sequence, id);
            if items.contains_key(&key) {
                continue;
            }
            let call_id: String = row.get(5)?;
            calls.push((turn_id.clone(), call_id));
            items.insert(
                key,
                WaitEvidence {
                    sequence,
                    at_ms,
                    turn_id,
                    is_output: false,
                    text: bounded_wait_text(
                        row.get(4)?,
                        reference,
                        if view == DetailView::HubRecent {
                            &mut call_remaining
                        } else {
                            &mut *remaining
                        },
                        truncated,
                    ),
                },
            );
        }
    }
    if view == DetailView::HubRecent {
        *remaining = remaining.saturating_sub(call_budget - call_remaining);
    }
    // scope_kind keeps this lookup on the current session/turn index. Only
    // the bounded selected identities are searched, including later turns.
    let mut output_statement = connection.prepare(
        "SELECT id,sequence_no,created_at_ms,
        CASE WHEN length(CAST(payload_json AS BLOB))<=?4 THEN payload_json ELSE NULL END
        FROM protocol_history_items WHERE session_id=?1 AND turn_id=?2 AND scope_kind='turn'
        AND json_extract(payload_json,'$.kind')='tool_output'
        AND json_extract(payload_json,'$.call_id')=?3 ORDER BY sequence_no DESC LIMIT 2",
    )?;
    for (turn_id, call_id) in calls {
        let mut rows = output_statement.query(params![
            reference.session_id.to_string(),
            turn_id,
            call_id,
            MAX_ITEM_BYTES as i64
        ])?;
        if let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let sequence: i64 = row.get(1)?;
            let at_ms: i64 = row.get(2)?;
            let text = bounded_wait_text(row.get(3)?, reference, remaining, truncated);
            items.insert(
                (at_ms, turn_id.clone(), sequence, id),
                WaitEvidence {
                    sequence,
                    at_ms,
                    turn_id,
                    is_output: true,
                    text,
                },
            );
        }
        *truncated |= rows.next()?.is_some();
    }
    Ok(items.into_values().collect())
}

fn bounded_wait_text(
    mut payload: Option<String>,
    reference: &StoredDeviceReference,
    remaining: &mut usize,
    truncated: &mut bool,
) -> String {
    if payload.as_ref().is_some_and(|text| text.len() > *remaining) {
        payload = None;
    }
    *remaining = remaining.saturating_sub(payload.as_ref().map_or(0, String::len));
    let decoded = payload
        .as_deref()
        .and_then(|text| serde_json::from_str::<HistoryItemPayload>(text).ok());
    *truncated |= decoded.as_ref().is_some_and(|payload| matches!(payload,
        HistoryItemPayload::ToolOutput { metadata, .. } if wait_metadata(metadata).get("truncated").and_then(Value::as_bool) == Some(true)));
    let projected = decoded.and_then(|payload| project_wait_payload(payload, reference));
    *truncated |= projected.is_none();
    projected.unwrap_or_else(|| "待機記録はサイズ上限または形式不一致により省略しました。結果本文を別の記録から補っていません。".into())
}

fn wait_metadata(metadata: &Value) -> &Value {
    metadata
        .get("tool_metadata")
        .filter(|value| value.is_object())
        .unwrap_or(metadata)
}

fn project_wait_payload(
    payload: HistoryItemPayload,
    reference: &StoredDeviceReference,
) -> Option<String> {
    let job_id = reference.job_id.as_deref()?;
    match payload {
        HistoryItemPayload::ToolCall {
            call_id,
            arguments_json,
            ..
        } => {
            let arguments: Value = serde_json::from_str(&arguments_json).ok()?;
            let requested = arguments.get("job_ids")?.as_array()?;
            if !requested.iter().any(|value| value.as_str() == Some(job_id)) {
                return None;
            }
            let mut projected = serde_json::json!({"job_ids":[job_id]});
            if let Some(timeout) = arguments.get("timeout_ms").and_then(Value::as_u64) {
                projected["timeout_ms"] = timeout.into();
            }
            Some(format!(
                "ツール呼び出し: wait_remote_tasks\n呼び出しID: {call_id}\n{}",
                serde_json::to_string_pretty(&projected).ok()?
            ))
        }
        HistoryItemPayload::ToolOutput {
            call_id,
            status,
            output_text,
            metadata,
            success,
            ..
        } => {
            let output = serde_json::from_str::<Value>(&output_text).ok();
            let metadata = wait_metadata(&metadata);
            let exact_row = |value: &Value| {
                let matching = value
                    .get("jobs")?
                    .as_array()?
                    .iter()
                    .filter(|row| {
                        row.get("job_id").and_then(Value::as_str) == Some(job_id)
                            && row.get("reference_id").and_then(Value::as_str)
                                == Some(reference.id.to_string().as_str())
                            && row.get("device_id").and_then(Value::as_str)
                                == Some(reference.device_id.as_str())
                            && row.get("profile_id").and_then(Value::as_str)
                                == Some(reference.profile_id.as_str())
                    })
                    .collect::<Vec<_>>();
                (matching.len() == 1).then(|| matching[0].clone())
            };
            let recorded_row = output.as_ref().and_then(exact_row);
            let state_row = recorded_row.clone().or_else(|| exact_row(metadata));
            let mut projected =
                serde_json::json!({"job_id":job_id,"reference_id":reference.id.to_string()});
            for field in ["interrupted", "timed_out"] {
                if let Some(value) = output
                    .as_ref()
                    .and_then(|value| value.get(field))
                    .or_else(|| metadata.get(field))
                    .and_then(Value::as_bool)
                {
                    projected[field] = value.into();
                }
            }
            if let Some(row) = state_row {
                for field in ["device_id", "profile_id", "state", "stop_status"] {
                    if let Some(value) = row.get(field).and_then(Value::as_str) {
                        projected[field] = value.into();
                    }
                }
            }
            let result = recorded_row
                .as_ref()
                .and_then(|row| row.get("result"))
                .and_then(Value::as_str);
            projected["result_body_recorded"] = result.is_some().into();
            if let Some(result) = result {
                projected["result"] = result.into();
            }
            let omission = if output.is_none() {
                "\n待機応答の本文は省略または形式不一致のため、保存済みの対象情報だけを表示します。"
            } else {
                ""
            };
            Some(format!(
                "ツール結果: wait_remote_tasks\n呼び出しID: {call_id}\n状態: {status:?}\n成功: {success:?}\n{}\nこの委任の対象だけを表示しています。結果本文は、この待機応答に保存されている場合だけ表示します。{omission}",
                serde_json::to_string_pretty(&projected).ok()?
            ))
        }
        _ => None,
    }
}

fn matching_call(reference: &StoredDeviceReference, arguments: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(arguments) else {
        return false;
    };
    if value.get("server_id").and_then(Value::as_str)
        != Some(crate::device_network::history_server_id(&reference.peer).as_str())
    {
        return false;
    }
    let Some(args) = value.get("arguments") else {
        return false;
    };
    let key_matches = args
        .get("request_key")
        .and_then(Value::as_str)
        .is_some_and(|key| {
            key == reference.request_key
                || crate::device_network::history_request_key(
                    reference.session_id,
                    reference.turn_id,
                    key,
                ) == reference.request_key
        });
    let job_matches = args
        .get("job_id")
        .and_then(Value::as_str)
        .zip(reference.job_id.as_deref())
        .is_some_and(|(left, right)| left == right);
    key_matches || job_matches
}

/// SQL chooses only the first/last bounded edges of the authorized turn, and
/// refuses to materialize oversized payloads. Truncation is visible in the export.
fn evidence_edges(
    connection: &rusqlite::Connection,
    session: SessionId,
    turn: TurnId,
    table: &str,
    filter: &str,
    calls: &str,
    remaining: &mut usize,
    truncated: &mut bool,
    view: DetailView,
) -> Result<Vec<EvidenceItem>, StorageError> {
    let timestamp = if table == "protocol_history_items" {
        "created_at_ms"
    } else {
        "NULL"
    };
    let base = format!(
        "FROM {table} WHERE session_id=?1 AND turn_id=?2 AND (?3 IS NOT NULL) AND ({filter})"
    );
    let count: i64 = connection.query_row(
        &format!("SELECT COUNT(*) {base}"),
        params![session.to_string(), turn.to_string(), calls],
        |row| row.get(0),
    )?;
    *truncated |= count > (EDGE_ITEMS * 2) as i64;
    let mut items = BTreeMap::new();
    for order in view.orders() {
        let mut statement = connection.prepare(&format!("SELECT sequence_no,{timestamp},CASE WHEN length(CAST(payload_json AS BLOB))<=?4 THEN payload_json ELSE NULL END {base} ORDER BY sequence_no {order} LIMIT ?5"))?;
        let mut rows = statement.query(params![
            session.to_string(),
            turn.to_string(),
            calls,
            MAX_ITEM_BYTES as i64,
            EDGE_ITEMS as i64
        ])?;
        while let Some(row) = rows.next()? {
            let sequence: i64 = row.get(0)?;
            if items.contains_key(&sequence) {
                continue;
            }
            let mut payload: Option<String> = row.get(2)?;
            if payload.as_ref().is_some_and(|text| text.len() > *remaining) {
                payload = None;
            }
            *remaining = remaining.saturating_sub(payload.as_ref().map_or(0, String::len));
            *truncated |= payload.is_none();
            items.insert(
                sequence,
                EvidenceItem {
                    sequence,
                    at_ms: row.get(1)?,
                    payload,
                },
            );
        }
    }
    Ok(items.into_values().collect())
}

fn render_detail(row: &McpHistoryRow, evidence: &Evidence, result: Option<&str>) -> (String, bool) {
    let mut output = format!(
        "# MCP履歴 — {}\n\n",
        if row.direction == McpHistoryDirection::Instruction {
            "MCP指示"
        } else {
            "MCP実行"
        }
    );
    let mut truncated = evidence.truncated;
    let metadata = format!(
        "履歴ID: {}\n指示概要: {}\n相手端末: {}\n対象: {}\n状態: {}\n停止確認: {}\n作成日時 (Unix ms): {}\n最終記録日時 (Unix ms): {}\nローカルセッションID: {}\nジョブID: {}\nルートタスクID: {}\n端末経路: {}",
        row.id,
        row.title,
        row.peer_label,
        row.target_label,
        row.state,
        row.stop_status,
        row.created_at_ms,
        row.updated_at_ms
            .map(|time| time.to_string())
            .unwrap_or_else(|| "記録なし".into()),
        row.session_id,
        row.job_id.as_deref().unwrap_or("未受付・不明"),
        row.root_task_id,
        row.device_path.join(" → ")
    );
    append_block(&mut output, "概要", &metadata, &mut truncated);
    output.push_str(if row.direction == McpHistoryDirection::Instruction {
        "これは指示側に保存された最終確認状態です。現在の接続・稼働状態を保証しません。実行側の完了と、指示側での結果受領は別です。以下は元のユーザー指示と、この委任に対応するMCP呼び出し・応答のみです。相手端末内部の実行ログは相手端末の「MCP実行」で確認してください。\n\n"
    } else {
        "これは実行側のローカル記録です。完了していても指示側が結果を受領したとは限りません。受付設定がOFFでも保存済み履歴は参照できます。記録がない待機理由は推測しません。\n\n"
    });
    if row.direction == McpHistoryDirection::Instruction {
        append_block(
            &mut output,
            "指示側での結果受領",
            if row.result_received {
                "結果を保存済み"
            } else {
                "結果未受領"
            },
            &mut truncated,
        );
    }
    if let Some(result) = result {
        append_block(&mut output, "保存済みの返却結果", result, &mut truncated);
    }
    output.push_str("## 指示・実行ログ\n\n");
    if evidence.history.is_empty() {
        output.push_str("対応するローカルターンの記録はありません。未開始または履歴削除済みの可能性があります。\n\n");
    }
    for item in &evidence.history {
        let heading = format!(
            "記録 {} / Unix ms {}",
            item.sequence,
            item.at_ms
                .map(|time| time.to_string())
                .unwrap_or_else(|| "不明".into())
        );
        let text = item
            .payload
            .as_deref()
            .and_then(|text| serde_json::from_str::<HistoryItemPayload>(text).ok())
            .map(public_history_text)
            .unwrap_or_else(|| "この記録はサイズ上限または形式不一致により省略しました。".into());
        append_block(&mut output, &heading, &text, &mut truncated);
    }
    if !evidence.waits.is_empty() {
        output.push_str("## 待機による状態・結果の取得\n\n同じローカルセッションの待機記録から、この委任の対象だけを表示します。ほかのジョブや端末の応答は含みません。元の保存データは変更していません。\n\n");
        for item in &evidence.waits {
            append_block(
                &mut output,
                &format!(
                    "待機記録 {} / Unix ms {} / ターン {}",
                    item.sequence, item.at_ms, item.turn_id
                ),
                &item.text,
                &mut truncated,
            );
        }
    }
    output.push_str("## 進行・承認・停止の記録\n\n");
    for item in &evidence.progress {
        let text = item
            .payload
            .as_deref()
            .and_then(|text| serde_json::from_str::<TurnItemPayload>(text).ok())
            .map(public_progress_text)
            .unwrap_or_else(|| "この記録はサイズ上限または形式不一致により省略しました。".into());
        append_block(
            &mut output,
            &format!("進行記録 {}", item.sequence),
            &text,
            &mut truncated,
        );
    }
    if truncated {
        output.push_str("\n> 省略あり: 大きな履歴は先頭・末尾各128件、1記録256 KiB、読み取り2 MiB、Markdown約1 MiBまでです。元の保存データは変更していません。\n");
    }
    (output, truncated)
}

fn public_history_text(payload: HistoryItemPayload) -> String {
    match payload {
        HistoryItemPayload::UserTurn { content, .. }
        | HistoryItemPayload::SteerTurn { content, .. } => {
            format!("ユーザー指示\n{}", content_text(content))
        }
        HistoryItemPayload::AssistantMessage { content, .. } => {
            format!("モデル出力\n{}", content_text(content))
        }
        HistoryItemPayload::ToolCall {
            call_id,
            tool_name,
            arguments_json,
            ..
        } => format!(
            "ツール呼び出し: {tool_name}\n呼び出しID: {call_id}\n{}",
            safe_arguments(&arguments_json)
        ),
        HistoryItemPayload::ToolOutput {
            call_id,
            status,
            title,
            output_text,
            success,
            ..
        } => format!(
            "ツール結果: {title}\n呼び出しID: {call_id}\n状態: {status:?}\n成功: {success:?}\n{output_text}"
        ),
        HistoryItemPayload::Error { message } => format!("エラー\n{message}"),
        HistoryItemPayload::ApprovalDecision { call_id, decision } => {
            format!("承認判断: {decision:?}\n呼び出しID: {call_id}")
        }
        HistoryItemPayload::RequestDiagnostics { diagnostics } => format!(
            "モデル要求を準備\nプロバイダー: {}\nモデル: {}\n要求タイムアウト: {} ms\nストリーム待機タイムアウト: {} ms\nメッセージ数: {}\nツール数: {}",
            diagnostics.provider,
            diagnostics.model_name,
            diagnostics.request_timeout_ms,
            diagnostics.stream_idle_timeout_ms,
            diagnostics.provider_message_count,
            diagnostics.tool_count
        ),
        HistoryItemPayload::DurableFeedback { feedback } => {
            format!("{}\n{}", feedback.public_title(), feedback.public_message)
        }
        HistoryItemPayload::FileChange { summary, .. } => format!("ファイル変更\n{summary}"),
        HistoryItemPayload::InterAgentCommunication { communication } => format!(
            "エージェント間通信: {} → {}\n{}",
            communication.author, communication.recipient, communication.content
        ),
        HistoryItemPayload::SubAgentActivity {
            agent_path,
            activity_kind,
            ..
        } => format!("子エージェント: {agent_path}\n{activity_kind:?}"),
        HistoryItemPayload::Compaction { summary, .. } => format!("コンテキスト整理\n{summary}"),
        HistoryItemPayload::CollaborationModeInstruction { mode } => {
            format!("実行モード: {}", mode.as_str())
        }
        HistoryItemPayload::WorldState { .. } => "実行環境を確認（詳細な環境情報は省略）".into(),
    }
}

fn public_progress_text(payload: TurnItemPayload) -> String {
    match payload {
        TurnItemPayload::ToolStatus {
            call_id,
            tool,
            status,
            title,
            summary,
        } => format!(
            "ツール進行: {tool:?}\n呼び出しID: {call_id}\n状態: {status:?}\n{title}\n{summary}"
        ),
        TurnItemPayload::ApprovalRequest { call_id, summary } => {
            format!("承認要求\n呼び出しID: {call_id}\n{summary}")
        }
        TurnItemPayload::Terminal { outcome } => format!(
            "ローカル実行の終端: {:?}\n{}",
            outcome.session_status(),
            outcome.summary()
        ),
        TurnItemPayload::Error { message } => format!("エラー\n{message}"),
        TurnItemPayload::Warning { message } => format!("警告\n{message}"),
        TurnItemPayload::Plan {
            explanation, plan, ..
        } => format!(
            "計画\n{}\n{}",
            explanation.unwrap_or_default(),
            plan.into_iter()
                .map(|step| format!("{:?}: {}", step.status, step.step))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        TurnItemPayload::DurableFeedback { feedback } => {
            format!("{}\n{}", feedback.public_title(), feedback.public_message)
        }
        _ => "対応するツールの進行記録".into(),
    }
}

fn content_text(content: Vec<ContentPart>) -> String {
    content
        .into_iter()
        .map(|part| match part {
            ContentPart::Text { text } => text,
            ContentPart::Image { .. } => "[画像は省略]".into(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Only visible tool arguments are included. Credentials from receiver scope,
/// Hub grants, request headers, private keys and provider request bodies are not read.
fn safe_arguments(text: &str) -> String {
    fn redact(value: &mut Value) {
        match value {
            Value::Object(fields) => {
                for (name, value) in fields {
                    let key = name.to_ascii_lowercase().replace(['-', '_'], "");
                    if [
                        "token",
                        "password",
                        "secret",
                        "authorization",
                        "apikey",
                        "privatekey",
                        "credential",
                        "cookie",
                        "certificatepem",
                    ]
                    .iter()
                    .any(|needle| key.contains(needle))
                    {
                        *value = Value::String("[非表示]".into());
                    } else {
                        redact(value);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(redact),
            _ => {}
        }
    }
    let Ok(mut value) = serde_json::from_str::<Value>(text) else {
        return "引数はJSON形式ではないため表示を省略しました。".into();
    };
    redact(&mut value);
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

fn short(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

fn append_block(output: &mut String, title: &str, text: &str, truncated: &mut bool) {
    let available = MAX_MARKDOWN_BYTES.saturating_sub(output.len() + title.len() + 128) / 5;
    if available == 0 {
        *truncated = true;
        return;
    }
    let mut end = available.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    *truncated |= end < text.len();
    let text = &text[..end];
    output.push_str(&format!("### {title}\n\n"));
    for line in text.lines() {
        output.push_str("    ");
        output.push_str(line);
        output.push('\n');
    }
    output.push('\n');
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
