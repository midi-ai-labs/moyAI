use std::collections::{BTreeMap, HashSet};
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ulid::Ulid;

use crate::session::{
    ChangeId, ChangeKind, EditorContext, ImagePart, PromptDispatchPart, RequestDiagnosticsPart,
    SessionId, SessionStatus, ToolCallId,
};
use crate::tool::ToolName;

mod projection;
mod recording;
mod store;

pub use projection::{
    ProtocolRunEventProjection, project_inter_agent_communication, project_protocol_run_event,
    project_sub_agent_activity, project_turn_item_for_run_event,
};
pub use recording::ProtocolRecordingSink;
pub use store::{
    ActiveHistoryPage, ActiveHistorySnapshot, CanonicalProtocolFence, CanonicalProtocolSnapshot,
    MAX_PROTOCOL_PAGE_LIMIT, ProtocolEventStore, ProtocolPage, ProtocolPageRequest,
    SqliteProtocolEventStore,
};
pub(crate) use store::{
    canonical_protocol_snapshot_from_connection, fork_canonical_items_in_transaction,
    insert_idle_inter_agent_history_in_transaction,
    insert_session_owned_event_bundle_in_transaction, latest_protocol_turn_ids_in_transaction,
};

macro_rules! protocol_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Ulid);

        impl $name {
            pub fn new() -> Self {
                Self(Ulid::new())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = ulid::DecodeError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Ok(Self(Ulid::from_string(value)?))
            }
        }
    };
}

protocol_id!(TurnId);
protocol_id!(RuntimeEventId);
protocol_id!(HistoryItemId);
protocol_id!(TurnItemId);
protocol_id!(ModelResponseId);

/// Durable collaboration mode selected for a thread.
///
/// The canonical history stream owns this value. Runtime turn policy resolves a
/// [`crate::agent::mode::CollaborationMode`] from the latest stored value rather
/// than maintaining a separate planner flag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeKind {
    #[default]
    Default,
    Plan,
}

impl ModeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Plan => "plan",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTurn {
    pub turn_id: TurnId,
    pub items: Vec<UserInputItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_dispatch: Option<PromptDispatchPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_context: Option<EditorContext>,
}

impl UserTurn {
    pub fn requires_image_capability(&self) -> bool {
        self.items.iter().any(UserInputItem::contains_image)
    }

    pub fn text(&self) -> String {
        self.items
            .iter()
            .filter_map(|item| match item {
                UserInputItem::Text { text } => Some(text.as_str()),
                UserInputItem::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn images(&self) -> Vec<ImagePart> {
        self.items
            .iter()
            .filter_map(|item| match item {
                UserInputItem::Image { image } => Some(image.clone()),
                UserInputItem::Text { .. } => None,
            })
            .collect()
    }

    pub fn content_parts(&self) -> Vec<ContentPart> {
        self.items
            .iter()
            .filter_map(|item| match item {
                UserInputItem::Text { text } if text.is_empty() => None,
                UserInputItem::Text { text } => Some(ContentPart::Text { text: text.clone() }),
                UserInputItem::Image { image } => Some(ContentPart::Image {
                    image: image.clone(),
                }),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SteerTurn {
    pub expected_turn_id: TurnId,
    pub items: Vec<UserInputItem>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub additional_context: BTreeMap<String, AdditionalContextEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_user_message_id: Option<String>,
}

impl SteerTurn {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn requires_image_capability(&self) -> bool {
        self.items.iter().any(UserInputItem::contains_image)
    }

    pub fn text(&self) -> String {
        self.items
            .iter()
            .filter_map(|item| match item {
                UserInputItem::Text { text } => Some(text.as_str()),
                UserInputItem::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn content_parts(&self) -> Vec<ContentPart> {
        self.items
            .iter()
            .filter_map(|item| match item {
                UserInputItem::Text { text } if text.is_empty() => None,
                UserInputItem::Text { text } => Some(ContentPart::Text { text: text.clone() }),
                UserInputItem::Image { image } => Some(ContentPart::Image {
                    image: image.clone(),
                }),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdditionalContextKind {
    Untrusted,
    Application,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdditionalContextEntry {
    pub value: String,
    pub kind: AdditionalContextKind,
}

pub fn steer_turn_is_active_turn_mailbox_contract_fixture_passes() -> bool {
    let expected_turn_id = TurnId::new();
    let steer = SteerTurn {
        expected_turn_id,
        items: vec![UserInputItem::Text {
            text: "Please adjust the current work before continuing.".to_string(),
        }],
        additional_context: BTreeMap::from([(
            "desktop.composer".to_string(),
            AdditionalContextEntry {
                value: "submitted while the turn was running".to_string(),
                kind: AdditionalContextKind::Application,
            },
        )]),
        client_user_message_id: Some("client-message-1".to_string()),
    };
    steer.expected_turn_id == expected_turn_id
        && !steer.is_empty()
        && !steer.requires_image_capability()
        && steer.text().contains("current work")
        && steer.content_parts().len() == 1
        && steer.additional_context.contains_key("desktop.composer")
        && steer.client_user_message_id.as_deref() == Some("client-message-1")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserInputItem {
    Text { text: String },
    Image { image: ImagePart },
}

impl UserInputItem {
    pub fn contains_image(&self) -> bool {
        matches!(self, Self::Image { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfile {
    ReadOnly,
    WorkspaceWrite,
    FullAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalDecision {
    Approved,
    Denied { reason: String },
}

/// A decision made by a human-facing approval surface.
///
/// `Abort` is deliberately not a [`ToolApprovalDecision`]: it interrupts the requesting turn
/// instead of resolving the tool approval waiter with a rejection result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    Denied,
    Abort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnInterruptionCause {
    ApprovalAborted,
    UserStop,
    AgentInterrupted,
    TreeStopped,
}

impl TurnInterruptionCause {
    pub const fn summary(self) -> &'static str {
        match self {
            Self::ApprovalAborted => "permission approval aborted by user",
            Self::UserStop => "run stopped by user",
            Self::AgentInterrupted => "agent interrupted",
            Self::TreeStopped => "agent tree stopped",
        }
    }
}

/// The sole durable owner of a turn's terminal classification and payload.
///
/// Session status, finish reason, interruption cause, and terminal display text
/// are projections of this discriminant. They must never be persisted beside it
/// as independently mutable fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnTerminalOutcome {
    Completed,
    Interrupted { cause: TurnInterruptionCause },
    Failed { error: String },
}

impl TurnTerminalOutcome {
    pub const fn session_status(&self) -> SessionStatus {
        match self {
            Self::Completed => SessionStatus::Completed,
            Self::Interrupted { .. } => SessionStatus::Cancelled,
            Self::Failed { .. } => SessionStatus::Failed,
        }
    }

    pub const fn finish_reason(&self) -> crate::session::FinishReason {
        match self {
            Self::Completed => crate::session::FinishReason::Stop,
            Self::Interrupted { .. } => crate::session::FinishReason::Cancelled,
            Self::Failed { .. } => crate::session::FinishReason::Error,
        }
    }

    pub const fn interruption_cause(&self) -> Option<TurnInterruptionCause> {
        match self {
            Self::Interrupted { cause } => Some(*cause),
            Self::Completed | Self::Failed { .. } => None,
        }
    }

    pub fn summary(&self) -> &str {
        match self {
            Self::Completed => "completed",
            Self::Interrupted { cause } => cause.summary(),
            Self::Failed { error } => error,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionMode {
    Automatic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeEvent {
    pub id: RuntimeEventId,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub sequence_no: i64,
    pub created_at_ms: i64,
    pub msg: RuntimeEventMsg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentActivityKind {
    Started,
    Interacted,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterAgentCommunication {
    pub author: String,
    pub recipient: String,
    pub content: String,
    pub trigger_turn: bool,
}

impl RuntimeEvent {
    pub fn terminal_outcome(&self) -> Option<&TurnTerminalOutcome> {
        match &self.msg {
            RuntimeEventMsg::TurnTerminal { terminal } => Some(&terminal.outcome),
            _ => None,
        }
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal_outcome().is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeEventMsg {
    UserInputAccepted {
        item_count: usize,
    },
    SteerInputAccepted {
        item_count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_user_message_id: Option<String>,
    },
    InterAgentCommunicationReceived {
        communication: InterAgentCommunication,
    },
    SubAgentActivity {
        activity_id: String,
        agent_session_id: SessionId,
        agent_path: String,
        activity_kind: SubAgentActivityKind,
    },
    AssistantMessageCommitted {
        response_id: ModelResponseId,
        text: String,
    },
    ModelRequestPrepared {
        diagnostics: RequestDiagnosticsPart,
    },
    WorldStateUpdated {
        snapshot: crate::context::WorldStateSnapshot,
    },
    ToolLifecycle {
        envelope: ToolLifecycleEnvelope,
    },
    ApprovalRequested {
        call_id: ToolCallId,
        tool: ToolName,
        summary: String,
    },
    ApprovalResolved {
        call_id: ToolCallId,
        tool: ToolName,
        decision: PermissionDecision,
    },
    ContextCompacted {
        item_id: HistoryItemId,
        mode: CompactionMode,
    },
    FileChangesRecorded {
        call_id: ToolCallId,
        change_ids: Vec<ChangeId>,
        summary: String,
    },
    Warning {
        message: String,
    },
    TurnTerminal {
        terminal: Box<crate::session::model::DurableTurnTerminal>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryScope {
    Turn { turn_id: TurnId },
    Session,
}

impl HistoryScope {
    pub const fn turn_id(self) -> Option<TurnId> {
        match self {
            Self::Turn { turn_id } => Some(turn_id),
            Self::Session => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Turn { .. } => "turn",
            Self::Session => "session",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryItem {
    pub id: HistoryItemId,
    pub session_id: SessionId,
    pub scope: HistoryScope,
    pub sequence_no: i64,
    pub created_at_ms: i64,
    pub payload: HistoryItemPayload,
}

impl HistoryItem {
    pub const fn turn_id(&self) -> Option<TurnId> {
        self.scope.turn_id()
    }
}

/// Canonical history items hidden by committed semantic compaction.
///
/// This derivation lives with the durable payload contract so every consumer
/// (model projection, compaction selection, and filtered agent-context fork)
/// observes the same active-history boundary.
pub(crate) fn compacted_history_item_ids(items: &[HistoryItem]) -> HashSet<HistoryItemId> {
    items
        .iter()
        .filter_map(|item| match &item.payload {
            HistoryItemPayload::Compaction {
                replacement_item_ids,
                ..
            } => Some(replacement_item_ids.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryItemPayload {
    UserTurn {
        content: Vec<ContentPart>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_dispatch: Option<PromptDispatchPart>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        editor_context: Option<EditorContext>,
    },
    SteerTurn {
        expected_turn_id: TurnId,
        content: Vec<ContentPart>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        additional_context: BTreeMap<String, AdditionalContextEntry>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_user_message_id: Option<String>,
    },
    InterAgentCommunication {
        communication: InterAgentCommunication,
    },
    SubAgentActivity {
        activity_id: String,
        agent_session_id: SessionId,
        agent_path: String,
        activity_kind: SubAgentActivityKind,
    },
    /// A typed developer-instruction boundary for subsequent turns.
    ///
    /// The effective instruction text is resolved once when constructing the
    /// immutable turn context. This item is state/replay evidence and is not
    /// independently projected into model messages.
    CollaborationModeInstruction {
        mode: ModeKind,
    },
    AssistantMessage {
        response_id: ModelResponseId,
        content: Vec<ContentPart>,
    },
    Error {
        message: String,
    },
    ToolCall {
        call_id: ToolCallId,
        response_id: ModelResponseId,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        model_call_id: String,
        /// The exact provider-emitted tool name. Execution routing derives a
        /// typed `ToolName` from this value without replacing the durable text.
        tool_name: String,
        /// The exact provider-emitted JSON text. Parsing and schema validation
        /// are transient execution concerns and never rewrite canonical history.
        arguments_json: String,
    },
    ToolOutput {
        call_id: ToolCallId,
        status: ToolLifecycleStatus,
        title: String,
        output_text: String,
        metadata: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        success: Option<bool>,
    },
    RequestDiagnostics {
        diagnostics: RequestDiagnosticsPart,
    },
    WorldState {
        snapshot: crate::context::WorldStateSnapshot,
        rendered: String,
    },
    ApprovalDecision {
        call_id: ToolCallId,
        decision: PermissionDecision,
    },
    Compaction {
        mode: CompactionMode,
        summary: String,
        replacement_item_ids: Vec<HistoryItemId>,
    },
    FileChange {
        call_id: ToolCallId,
        change_ids: Vec<ChangeId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changes: Vec<FileChangeEvidence>,
        summary: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Image { image: ImagePart },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnItem {
    pub id: TurnItemId,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub source_item_id: Option<HistoryItemId>,
    pub sequence_no: i64,
    pub payload: TurnItemPayload,
}

pub fn turn_items_in_projection_order(turn_items: &[TurnItem]) -> Vec<&TurnItem> {
    let mut turn_order = Vec::new();
    for item in turn_items {
        if !turn_order.contains(&item.turn_id) {
            turn_order.push(item.turn_id);
        }
    }
    let mut ordered = turn_items.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|item| {
        let turn_index = turn_order
            .iter()
            .position(|turn_id| *turn_id == item.turn_id)
            .unwrap_or(usize::MAX);
        (turn_index, item.sequence_no, item.id.0)
    });
    ordered
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanStep {
    pub step: String,
    pub status: PlanStepStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnItemPayload {
    UserMessage {
        text: String,
    },
    SteerMessage {
        text: String,
    },
    InterAgentCommunication {
        communication: InterAgentCommunication,
    },
    SubAgentActivity {
        activity_id: String,
        agent_session_id: SessionId,
        agent_path: String,
        activity_kind: SubAgentActivityKind,
    },
    AgentMessage {
        text: String,
    },
    Plan {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        explanation: Option<String>,
        plan: Vec<PlanStep>,
    },
    WorldState {
        summary: String,
    },
    ToolStatus {
        call_id: ToolCallId,
        tool: ToolName,
        status: ToolLifecycleStatus,
        title: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        summary: String,
    },
    FileChange {
        call_id: ToolCallId,
        change_ids: Vec<ChangeId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changes: Vec<FileChangeEvidence>,
        summary: String,
    },
    ContextCompaction {
        summary: String,
    },
    ApprovalRequest {
        call_id: ToolCallId,
        summary: String,
    },
    Warning {
        message: String,
    },
    Error {
        message: String,
    },
    Terminal {
        outcome: TurnTerminalOutcome,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnItemProjectionRole {
    UserVisibleMessage,
    AssistantVisibleMessage,
    RuntimeProjection,
    RuntimeControl,
    ToolLifecycleEvidence,
    FileEvidence,
    MemoryContinuity,
    ApprovalEvidence,
    RuntimeDiagnostic,
    RuntimeError,
    TerminalOutcome,
}

impl TurnItemPayload {
    pub fn projection_role(&self) -> TurnItemProjectionRole {
        match self {
            Self::UserMessage { .. } | Self::SteerMessage { .. } => {
                TurnItemProjectionRole::UserVisibleMessage
            }
            Self::AgentMessage { .. } | Self::InterAgentCommunication { .. } => {
                TurnItemProjectionRole::AssistantVisibleMessage
            }
            Self::Plan { .. } | Self::WorldState { .. } => {
                TurnItemProjectionRole::RuntimeProjection
            }
            Self::SubAgentActivity { .. } => TurnItemProjectionRole::RuntimeControl,
            Self::ToolStatus { .. } => TurnItemProjectionRole::ToolLifecycleEvidence,
            Self::FileChange { .. } => TurnItemProjectionRole::FileEvidence,
            Self::ContextCompaction { .. } => TurnItemProjectionRole::MemoryContinuity,
            Self::ApprovalRequest { .. } => TurnItemProjectionRole::ApprovalEvidence,
            Self::Warning { .. } => TurnItemProjectionRole::RuntimeDiagnostic,
            Self::Error { .. } => TurnItemProjectionRole::RuntimeError,
            Self::Terminal { .. } => TurnItemProjectionRole::TerminalOutcome,
        }
    }

    pub fn is_internal_projection_only(&self) -> bool {
        matches!(
            self.projection_role(),
            TurnItemProjectionRole::RuntimeProjection | TurnItemProjectionRole::RuntimeControl
        )
    }
}

pub fn turn_item_internal_projection_roles_are_not_primary_display_fixture_passes() -> bool {
    let internal = [
        TurnItemPayload::Plan {
            explanation: Some("plan cache".to_string()),
            plan: vec![PlanStep {
                step: "inspect the relevant contract".to_string(),
                status: PlanStepStatus::InProgress,
            }],
        },
        TurnItemPayload::WorldState {
            summary: "captured world state".to_string(),
        },
    ];
    let visible = [
        TurnItemPayload::UserMessage {
            text: "user".to_string(),
        },
        TurnItemPayload::AgentMessage {
            text: "assistant".to_string(),
        },
        TurnItemPayload::ContextCompaction {
            summary: "compaction".to_string(),
        },
        TurnItemPayload::Terminal {
            outcome: TurnTerminalOutcome::Completed,
        },
    ];

    internal
        .iter()
        .all(TurnItemPayload::is_internal_projection_only)
        && visible
            .iter()
            .all(|payload| !payload.is_internal_projection_only())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolLifecycleEnvelope {
    pub call_id: ToolCallId,
    pub tool: ToolName,
    pub status: ToolLifecycleStatus,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChangeEvidence {
    pub change_id: ChangeId,
    pub kind: ChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_before: Option<Utf8PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_after: Option<Utf8PathBuf>,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    NotRequired,
    Pending,
    Approved,
    Denied { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxDecision {
    pub profile: SandboxProfile,
    pub network_allowed: bool,
    pub escalated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolLifecycleStatus {
    Pending,
    Running,
    Completed,
    Declined,
    Cancelled,
    Failed,
}
