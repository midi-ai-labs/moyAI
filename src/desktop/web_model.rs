use serde::{Deserialize, Serialize};

use super::models::{
    DesktopArtifactRow, DesktopCommandRow, DesktopFileChangeRow, DesktopProjectRow,
    DesktopSessionRow, DesktopStopMutationTarget, DesktopTranscriptRow,
};
use super::query::desktop_run_phase_label;
use super::startup::{DesktopStartupCheckStatus, DesktopStartupStatus};
use super::state::{DesktopDoclingReadinessState, DesktopOverlay, DesktopState, DesktopStatusCode};
use crate::app::AgentActivityRecord;
use crate::config::{AccessMode, ConfigField, ProviderProfile, ResolvedConfig};
use crate::llm::ProviderModelLoadState;
use crate::runtime::AgentStatus;
use crate::session::{ActiveTurnExpectation, ToolCallStatus};
use crate::tool::PermissionRequest;
use crate::tui::state::{PromptReviewPhase, RunStatus, ToolStatusView, tool_action_label};

const MOYAI_PRODUCT_NAME: &str = "moyAI";
const MOYAI_DESKTOP_CODENAME: &str = "LYNX";
const BUNDLED_LICENSE_TEXT: &str = include_str!("../../LICENSE");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopPermissionProjection {
    pub summary: String,
    pub details: Vec<String>,
    pub targets: Vec<String>,
    pub outside_workspace: bool,
    pub risks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_task_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<crate::remote_agent::approval::RemoteApprovalContext>,
}

impl From<&PermissionRequest> for DesktopPermissionProjection {
    fn from(permission: &PermissionRequest) -> Self {
        Self {
            summary: permission.summary.clone(),
            details: permission.details.clone(),
            targets: permission.targets.iter().map(ToString::to_string).collect(),
            outside_workspace: permission.outside_workspace,
            risks: permission
                .risks
                .iter()
                .map(|risk| risk.label().to_string())
                .collect(),
            agent_path: permission.agent_path.clone(),
            agent_task_name: permission.agent_task_name.clone(),
            remote: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopPlanProjection {
    pub explanation: Option<String>,
    pub steps: Vec<crate::protocol::PlanStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesktopPendingTurnInputProjection {
    pub id: String,
    pub turn_id: String,
    pub text: String,
    pub image_count: usize,
    pub accepted_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopStartupCheckProjection {
    pub key: String,
    pub label: String,
    pub status: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopStartupProjection {
    pub status: String,
    pub title: String,
    pub message: String,
    pub detail: String,
    pub action_overlay: String,
    pub initial_setup_required: bool,
    pub initial_setup_reason: Option<String>,
    pub global_config_path: Option<String>,
    pub setup_target: Option<DesktopInitialSetupMutationTargetProjection>,
    pub checks: Vec<DesktopStartupCheckProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopInitialSetupMutationTargetProjection {
    pub workspace_path: String,
    pub global_config_path: String,
    pub setup_generation: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DesktopConfigFieldProjection {
    pub key: String,
    pub value: String,
    pub sensitive: bool,
    pub configured: bool,
    pub env_override: Option<String>,
    pub value_type: String,
    pub required: bool,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub options: Vec<String>,
}

impl std::fmt::Debug for DesktopConfigFieldProjection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = formatter.debug_struct("DesktopConfigFieldProjection");
        debug.field("key", &self.key);
        if matches!(
            self.key.as_str(),
            "model.system_prompt" | "side_chat.system_prompt"
        ) {
            debug.field("value_chars", &self.value.chars().count());
        } else {
            debug.field("value", &self.value);
        }
        debug
            .field("sensitive", &self.sensitive)
            .field("configured", &self.configured)
            .field("env_override", &self.env_override)
            .field("value_type", &self.value_type)
            .field("required", &self.required)
            .field("min_value", &self.min_value)
            .field("max_value", &self.max_value)
            .field("options", &self.options)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesktopProviderStatusProjection {
    pub kind: String,
    pub title: String,
    pub hint: String,
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesktopAgentActivityRow {
    pub agent_path: String,
    pub session_id: String,
    pub task_name: String,
    pub task_preview: String,
    pub status: String,
    pub current_activity: String,
    pub result_preview: String,
    pub started_order: u64,
    pub updated: bool,
    pub active_turn_id: Option<String>,
    pub interrupt_target: Option<DesktopAgentInterruptTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopAgentInterruptTarget {
    pub workspace_path: String,
    pub root_session_id: String,
    pub agent_path: String,
    pub child_session_id: String,
    pub expected_turn_id: String,
    pub admission_revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopAgentExecutionProjection {
    pub workspace_path: String,
    pub root_session_id: String,
    pub agent_path: String,
    pub session_id: String,
    pub task_name: String,
    pub transcript_rows: Vec<DesktopTranscriptRow>,
    pub turn_page_offset: usize,
    pub turn_page_end: usize,
    pub turn_page_total: usize,
    pub turn_page_has_previous: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopComposerSubmitMode {
    NewRequest,
    Steer,
    Blocked,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopTaskActivityState {
    Idle,
    Running,
    Finalizing,
    Attention,
}

impl DesktopComposerSubmitMode {
    fn admission_is_open(self) -> bool {
        !matches!(self, Self::Blocked)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DesktopRuntimeProjection {
    pub agent_activity_rows: Vec<DesktopAgentActivityRow>,
    pub current_turn_agent_activity_rows: Vec<DesktopAgentActivityRow>,
    pub agent_tree_active: bool,
    pub root_run_finalizing: bool,
    pub root_run_generation: Option<u64>,
    pub last_root_run_epoch: u64,
    pub composer_commit_generation: u64,
    pub active_turn_expectation: ActiveTurnExpectation,
    pub side_chat: DesktopSideChatProjection,
}

impl Default for DesktopRuntimeProjection {
    fn default() -> Self {
        Self {
            agent_activity_rows: Vec::new(),
            current_turn_agent_activity_rows: Vec::new(),
            agent_tree_active: false,
            root_run_finalizing: false,
            root_run_generation: None,
            last_root_run_epoch: 0,
            composer_commit_generation: 0,
            active_turn_expectation: ActiveTurnExpectation::initial_idle(),
            side_chat: DesktopSideChatProjection::default(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopSideChatProjection {
    pub configured: bool,
    pub deleting: bool,
    pub chat_id: Option<String>,
    pub owner_session_id: Option<String>,
    pub model: String,
    pub system_prompt: String,
    pub base_url: String,
    pub provider_profile: String,
    pub status: String,
    pub phase: String,
    pub last_error: String,
    pub generation: String,
    pub draft_text: String,
    pub draft_quote: Option<DesktopSideChatDraftQuoteProjection>,
    pub draft_revision: String,
    pub context_scope: String,
    pub context_as_of_append_position: Option<String>,
    pub context_truncated: bool,
    pub messages: Vec<DesktopSideChatMessageProjection>,
    pub can_send: bool,
    pub can_cancel: bool,
    pub direct_provider_capture: Option<DesktopSideChatDirectCaptureProjection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopSideChatDirectCaptureProjection {
    pub base_url: String,
    pub model: String,
    pub provider_profile: String,
    pub enabled: bool,
    pub reason: String,
}

impl std::fmt::Debug for DesktopSideChatProjection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DesktopSideChatProjection")
            .field("configured", &self.configured)
            .field("deleting", &self.deleting)
            .field("chat_id", &self.chat_id)
            .field("owner_session_id", &self.owner_session_id)
            .field("model", &self.model)
            .field("system_prompt_chars", &self.system_prompt.chars().count())
            .field("base_url", &self.base_url)
            .field("provider_profile", &self.provider_profile)
            .field("status", &self.status)
            .field("phase", &self.phase)
            .field("last_error", &self.last_error)
            .field("generation", &self.generation)
            .field("draft_text", &self.draft_text)
            .field("draft_quote", &self.draft_quote)
            .field("draft_revision", &self.draft_revision)
            .field("context_scope", &self.context_scope)
            .field(
                "context_as_of_append_position",
                &self.context_as_of_append_position,
            )
            .field("context_truncated", &self.context_truncated)
            .field("messages", &self.messages)
            .field("can_send", &self.can_send)
            .field("can_cancel", &self.can_cancel)
            .finish()
    }
}

impl Default for DesktopSideChatProjection {
    fn default() -> Self {
        Self {
            configured: false,
            deleting: false,
            chat_id: None,
            owner_session_id: None,
            model: String::new(),
            system_prompt: String::new(),
            base_url: String::new(),
            provider_profile: String::new(),
            status: "idle".to_string(),
            phase: String::new(),
            last_error: String::new(),
            generation: "0".to_string(),
            draft_text: String::new(),
            draft_quote: None,
            draft_revision: "0".to_string(),
            context_scope: "owner_session".to_string(),
            context_as_of_append_position: None,
            context_truncated: false,
            messages: Vec::new(),
            can_send: false,
            can_cancel: false,
            direct_provider_capture: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopSideChatDraftQuoteProjection {
    pub source_kind: String,
    pub source_history_item_id: String,
    pub source_append_position: Option<String>,
    pub selected_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopSideChatMessageProjection {
    pub id: String,
    pub sequence_no: usize,
    pub role: String,
    pub content: String,
}

impl DesktopRuntimeProjection {
    fn root_run_active(&self) -> bool {
        self.root_run_generation.is_some()
    }

    fn blocks_new_request(&self) -> bool {
        self.root_run_active() || self.root_run_finalizing
    }

    fn pre_admission_active(&self, state_busy: bool) -> bool {
        self.root_run_active() && !self.root_run_finalizing && !state_busy
    }
}

pub(crate) fn access_runtime_owner_token(
    root_run_generation: Option<u64>,
    agent_tree_active: bool,
    last_root_run_epoch: u64,
) -> String {
    if let Some(generation) = root_run_generation {
        format!("root:{generation}")
    } else if agent_tree_active {
        format!("tree:{last_root_run_epoch}")
    } else {
        format!("idle:{last_root_run_epoch}")
    }
}

pub(crate) fn access_runtime_owner_terminal_settlement_matches(
    expected: &str,
    current: &str,
) -> bool {
    let parse = |value: &str| {
        let (kind, generation) = value.split_once(':')?;
        let kind = match kind {
            "root" => 0,
            "tree" => 1,
            "idle" => 2,
            _ => return None,
        };
        Some((kind, generation.parse::<u64>().ok()?))
    };
    match (parse(expected), parse(current)) {
        (Some((0, expected)), Some((1 | 2, current))) => expected == current,
        (Some((1, expected)), Some((0 | 2, current))) => expected == current,
        _ => false,
    }
}

pub(crate) fn navigation_admission_blocker(
    busy: bool,
    background_mutation_pending: bool,
    navigation_loading: bool,
    root_run_finalizing: bool,
) -> Option<&'static str> {
    if root_run_finalizing {
        Some("the current run is finalizing")
    } else if busy {
        Some("a run is active")
    } else if background_mutation_pending {
        Some("a background mutation is active")
    } else if navigation_loading {
        Some("navigation is already active")
    } else {
        None
    }
}

fn composer_new_request_admission_is_open(
    runtime: &DesktopRuntimeProjection,
    busy: bool,
    navigation_loading: bool,
    background_mutation_pending: bool,
    post_run_refresh_pending: bool,
) -> bool {
    matches!(
        runtime.active_turn_expectation,
        ActiveTurnExpectation::Idle { .. }
    ) && !busy
        && !navigation_loading
        && !background_mutation_pending
        && !post_run_refresh_pending
        && !runtime.blocks_new_request()
}

fn composer_steer_admission_is_open(
    runtime: &DesktopRuntimeProjection,
    navigation_loading: bool,
    background_mutation_pending: bool,
) -> bool {
    matches!(
        runtime.active_turn_expectation,
        ActiveTurnExpectation::Turn { .. }
    ) && !runtime.root_run_finalizing
        && !navigation_loading
        && !background_mutation_pending
}

fn config_draft_capability_projection(
    dirty: bool,
    edit_admission_open: bool,
    commit_admission_open: bool,
    initial_setup_required: bool,
    external_owner_admission_open: bool,
    access_mode_admission_open: bool,
) -> DesktopConfigDraftCapabilityProjection {
    DesktopConfigDraftCapabilityProjection {
        dirty,
        edit_enabled: edit_admission_open,
        discard_enabled: dirty && edit_admission_open,
        commit_enabled: commit_admission_open && (dirty || initial_setup_required),
        external_owner_mutation_open: !dirty && external_owner_admission_open,
        access_mode_mutation_enabled: !dirty && access_mode_admission_open,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopConfigMutationTargetProjection {
    pub workspace_path: String,
    pub session_id: Option<String>,
    pub config_generation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopAccessModeMutationTargetProjection {
    pub workspace_path: String,
    pub session_id: Option<String>,
    pub config_generation: String,
    pub access_mode: AccessMode,
    pub runtime_owner_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionSettingsMutationTargetProjection {
    pub workspace_path: String,
    pub root_session_id: String,
    pub settings_revision: String,
    pub config_generation: String,
    pub runtime_owner_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopSessionSettingsProjection {
    pub available: bool,
    pub base_url: String,
    pub model: String,
    pub provider_profile: String,
    pub api_key_env: String,
    pub access_mode: AccessMode,
    pub context_window: String,
    pub context_window_inherited: bool,
    pub provider_mutation_enabled: bool,
    pub access_mutation_enabled: bool,
    pub unavailable_reason: String,
    pub target: Option<DesktopSessionSettingsMutationTargetProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopConfigDraftCapabilityProjection {
    pub dirty: bool,
    pub edit_enabled: bool,
    pub discard_enabled: bool,
    pub commit_enabled: bool,
    pub external_owner_mutation_open: bool,
    pub access_mode_mutation_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopConfigDraftCapabilitiesProjection {
    pub clean: DesktopConfigDraftCapabilityProjection,
    pub dirty: DesktopConfigDraftCapabilityProjection,
}

fn task_activity_state(
    runtime: &DesktopRuntimeProjection,
    busy: bool,
    pending_permission: bool,
) -> DesktopTaskActivityState {
    if runtime.root_run_finalizing {
        DesktopTaskActivityState::Finalizing
    } else if pending_permission {
        DesktopTaskActivityState::Attention
    } else if busy || runtime.agent_tree_active {
        DesktopTaskActivityState::Running
    } else {
        DesktopTaskActivityState::Idle
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesktopAboutProjection {
    pub product_name: String,
    pub version: String,
    pub codename: String,
    pub license_identifier: String,
    pub copyright_notice: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopDraftActionTargetProjection {
    pub workspace_path: String,
    pub session_id: Option<String>,
    pub owner_generation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopPromptReviewMutationTargetProjection {
    pub workspace_path: String,
    pub session_id: Option<String>,
    pub owner_generation: String,
    pub request_id: String,
    pub expected_state: DesktopRunExpectedStateProjection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DesktopRunExpectedStateProjection {
    Idle {
        #[serde(rename = "latestTurnId")]
        latest_turn_id: Option<String>,
        #[serde(rename = "admissionRevision")]
        admission_revision: String,
    },
    Turn {
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "admissionRevision")]
        admission_revision: String,
    },
}

impl From<ActiveTurnExpectation> for DesktopRunExpectedStateProjection {
    fn from(expected: ActiveTurnExpectation) -> Self {
        match expected {
            ActiveTurnExpectation::Idle {
                latest_turn_id,
                revision,
            } => Self::Idle {
                latest_turn_id: latest_turn_id.map(|turn_id| turn_id.to_string()),
                admission_revision: revision.to_string(),
            },
            ActiveTurnExpectation::Turn { turn_id, revision } => Self::Turn {
                turn_id: turn_id.to_string(),
                admission_revision: revision.to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRunMutationTargetProjection {
    pub workspace_path: String,
    pub session_id: Option<String>,
    pub runtime_owner_token: String,
    pub permission_confirmation_id: Option<String>,
    pub expected_state: DesktopRunExpectedStateProjection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopWebState {
    #[serde(skip_deserializing)]
    pub hub: Option<crate::hub::HubConnectionProjection>,
    #[serde(skip_deserializing)]
    pub mcp_publish: Option<crate::mcp_publish::PublishProjection>,
    #[serde(default)]
    pub mcp_activity: Option<crate::remote_agent::RemoteActivityProjection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_network: Option<crate::device_network::DeviceNetworkProjection>,
    pub projection_revision: String,
    pub workspace_path: String,
    pub provider_label: String,
    pub model_label: String,
    pub access_label: String,
    pub access_target: DesktopAccessModeMutationTargetProjection,
    pub session_settings: DesktopSessionSettingsProjection,
    pub config_draft_capabilities: DesktopConfigDraftCapabilitiesProjection,
    pub current_session_label: String,
    pub selected_session_title: String,
    pub status_message: String,
    pub status_detail: String,
    pub status_code: DesktopStatusCode,
    pub run_status_key: String,
    pub run_status_text: String,
    pub run_phase: String,
    pub run_active_step: String,
    pub latest_tool_summary: String,
    pub plan: Option<DesktopPlanProjection>,
    pub progress_text: String,
    pub tool_status_text: String,
    pub token_meter_label: String,
    pub token_meter_title: String,
    pub token_meter_level: String,
    pub session_usage_label: String,
    pub session_usage_title: String,
    pub session_usage_state: String,
    pub confirmation_visible: bool,
    pub confirmation_id: Option<String>,
    pub confirmation_text: String,
    pub confirmation: Option<DesktopPermissionProjection>,
    pub startup: DesktopStartupProjection,
    pub composer_commit_generation: String,
    pub draft_prompt: String,
    pub draft_target: DesktopDraftActionTargetProjection,
    pub image_input: String,
    pub attached_images: Vec<String>,
    pub composer_submit_mode: DesktopComposerSubmitMode,
    pub can_submit: bool,
    pub can_cancel_run: bool,
    pub run_target: DesktopRunMutationTargetProjection,
    pub stop_target: Option<DesktopStopMutationTarget>,
    pub busy: bool,
    pub task_activity_state: DesktopTaskActivityState,
    pub async_polling_required: bool,
    pub pending_async_operations: Vec<String>,
    pub navigation_loading: bool,
    pub navigation_admission_open: bool,
    pub turn_page_admission_open: bool,
    pub post_run_refresh_pending: bool,
    pub background_mutation_pending: bool,
    pub overlay: String,
    pub about: DesktopAboutProjection,
    pub side_chat: DesktopSideChatProjection,
    pub project_rows: Vec<DesktopProjectRow>,
    pub selected_project_index: i32,
    pub session_rows: Vec<DesktopSessionRow>,
    pub chat_session_rows: Vec<DesktopSessionRow>,
    pub selected_session_index: i32,
    pub session_search_text: String,
    pub session_search_include_archived: bool,
    pub thread_empty: bool,
    pub transcript_rows: Vec<DesktopTranscriptRow>,
    pub pending_turn_inputs: Vec<DesktopPendingTurnInputProjection>,
    pub turn_page_offset: usize,
    pub turn_page_limit: usize,
    pub turn_page_total: usize,
    pub turn_page_has_more: bool,
    pub artifact_rows: Vec<DesktopArtifactRow>,
    pub selected_artifact_index: i32,
    pub artifact_preview_available: bool,
    pub artifact_preview_text: String,
    pub file_change_rows: Vec<DesktopFileChangeRow>,
    pub file_change_summary_text: String,
    pub agent_activity_rows: Vec<DesktopAgentActivityRow>,
    pub current_turn_agent_activity_rows: Vec<DesktopAgentActivityRow>,
    pub agent_tree_active: bool,
    pub local_search_text: String,
    pub local_search_results_text: String,
    pub command_rows: Vec<DesktopCommandRow>,
    pub provider_base_url: String,
    pub provider_profile: String,
    pub provider_api_key_env: String,
    pub provider_effective_base_url: String,
    pub provider_effective_profile: String,
    pub provider_effective_api_key_env: String,
    pub provider_effective_context_window: String,
    pub provider_effective_model_id: String,
    pub provider_catalog_base_url: Option<String>,
    pub provider_catalog_profile: Option<String>,
    pub provider_catalog_api_key_env: Option<String>,
    pub provider_context_window: String,
    pub provider_models: Vec<String>,
    pub provider_model_ids: Vec<String>,
    pub provider_selected_index: i32,
    pub provider_status: DesktopProviderStatusProjection,
    pub provider_selected_model_summary: Vec<String>,
    pub provider_loading: bool,
    pub provider_apply_enabled: bool,
    pub docling_readiness: DesktopDoclingReadinessState,
    pub config_fields: Vec<DesktopConfigFieldProjection>,
    pub config_target: DesktopConfigMutationTargetProjection,
    pub workspace_input: String,
    pub review_target: Option<DesktopPromptReviewMutationTargetProjection>,
    pub review_raw_text: String,
    pub review_draft_text: String,
    pub review_status_text: String,
    pub send_enhanced_enabled: bool,
    pub send_raw_enabled: bool,
    pub history_export_enabled: bool,
    pub enhance_enabled: bool,
    pub image_input_enabled: bool,
    pub window_opacity_percent: i32,
}

#[cfg(test)]
pub(crate) fn desktop_web_state(
    state: &DesktopState,
    runtime: &DesktopRuntimeProjection,
) -> DesktopWebState {
    desktop_web_state_with_permission(state, runtime, None)
}

fn stop_mutation_target_projection(
    state: &DesktopState,
    runtime: &DesktopRuntimeProjection,
    pending_permission_id: Option<u64>,
) -> Option<DesktopStopMutationTarget> {
    let workspace_path = state.snapshot.workspace_path.clone();
    let session_id = state
        .app_state
        .current_session_id
        .map(|session_id| session_id.to_string());
    match runtime.active_turn_expectation {
        ActiveTurnExpectation::Turn { turn_id, revision } => {
            Some(DesktopStopMutationTarget::Turn {
                workspace_path,
                session_id: session_id?,
                turn_id: turn_id.to_string(),
                admission_revision: revision.to_string(),
                root_epoch: runtime
                    .root_run_generation
                    .unwrap_or(runtime.last_root_run_epoch)
                    .to_string(),
            })
        }
        ActiveTurnExpectation::Idle {
            latest_turn_id,
            revision,
        } => {
            if let Some(root_generation) = runtime.root_run_generation {
                return Some(DesktopStopMutationTarget::Root {
                    workspace_path,
                    session_id,
                    root_generation: root_generation.to_string(),
                    latest_turn_id: latest_turn_id.map(|turn_id| turn_id.to_string()),
                    admission_revision: revision.to_string(),
                    permission_confirmation_id: pending_permission_id
                        .map(|confirmation_id| confirmation_id.to_string()),
                });
            }
            if (runtime.agent_tree_active || pending_permission_id.is_some())
                && let Some(turn_id) = latest_turn_id
            {
                return Some(DesktopStopMutationTarget::Turn {
                    workspace_path,
                    session_id: session_id?,
                    turn_id: turn_id.to_string(),
                    admission_revision: revision.to_string(),
                    root_epoch: runtime.last_root_run_epoch.to_string(),
                });
            }
            None
        }
    }
}

pub(crate) fn desktop_web_state_with_permission(
    state: &DesktopState,
    runtime: &DesktopRuntimeProjection,
    pending_permission: Option<(u64, &PermissionRequest)>,
) -> DesktopWebState {
    let state_busy = state.is_busy();
    let root_run_active = runtime.root_run_active();
    let mut hub = state
        .hub_connection
        .as_ref()
        .map(crate::hub::HubConnection::projection_now);
    if let Some(hub) = &mut hub {
        hub.can_change_main_mode &=
            !root_run_active && !runtime.agent_tree_active && !state.prompt_enhance_pending();
        hub.can_change_side_chat_mode &= runtime.side_chat.status != "running";
    }
    let main_route_ready = hub.as_ref().is_none_or(|hub| {
        hub.main_mode == crate::hub::HubRouteMode::Direct || hub.can_enable_main_hub
    });
    let main_enhance_route_ready =
        main_route_ready && hub.as_ref().is_none_or(|hub| hub.active_main.is_none());
    let mut side_chat = runtime.side_chat.clone();
    if let Some(hub) = &hub {
        side_chat.can_send &= hub.side_chat_mode == crate::hub::HubRouteMode::Direct
            || (hub.can_enable_side_chat_hub && hub.active_side_chat.is_none());
    }
    let hub_polling = hub.as_ref().is_some_and(|hub| {
        matches!(
            hub.status,
            crate::hub::HubConnectionStatus::Connecting
                | crate::hub::HubConnectionStatus::Connected
        ) || hub.active_main.is_some()
            || hub.active_side_chat.is_some()
    });
    let busy = state_busy || root_run_active;
    let task_activity_state = task_activity_state(runtime, busy, pending_permission.is_some());
    let stop_target = stop_mutation_target_projection(
        state,
        runtime,
        pending_permission.map(|(confirmation_id, _)| confirmation_id),
    );
    let mut session_rows = state.snapshot.session_rows.clone();
    for row in &mut session_rows {
        let Some(turn_id) = row.active_turn_id else {
            continue;
        };
        row.interrupt_target = if state.app_state.current_session_id == Some(row.session_id) {
            stop_target.clone()
        } else {
            Some(DesktopStopMutationTarget::Turn {
                workspace_path: state.snapshot.workspace_path.clone(),
                session_id: row.session_id.to_string(),
                turn_id: turn_id.to_string(),
                admission_revision: row.admission_revision.clone(),
                root_epoch: runtime.last_root_run_epoch.to_string(),
            })
        };
    }
    let pre_admission_active = runtime.pre_admission_active(state_busy);
    let detail = state.selected_detail();
    let pending_turn_inputs = state
        .open_session
        .as_ref()
        .filter(|open_session| Some(open_session.session_id()) == state.selected_session_id())
        .map(|open_session| {
            open_session
                .pending_turn_inputs()
                .iter()
                .map(|input| DesktopPendingTurnInputProjection {
                    id: input.id.to_string(),
                    turn_id: input.turn_id.to_string(),
                    text: input.text.clone(),
                    image_count: input.image_count,
                    accepted_at_ms: input.accepted_at_ms,
                })
                .collect()
        })
        .unwrap_or_default();
    let (
        review_raw_text,
        review_draft_text,
        review_status_text,
        send_enhanced_enabled,
        send_raw_enabled,
    ) = if let Some(review) = &state.app_state.prompt_review {
        let status = match review.phase {
            PromptReviewPhase::Enhancing => {
                "推敲案を生成しています。キャンセルすると元の依頼文を保持します。".to_string()
            }
            PromptReviewPhase::Reviewing => {
                "推敲案を編集し、推敲文または原文のどちらで送るか選んでください。".to_string()
            }
        };
        (
            review.raw_prompt_text.clone(),
            review.current_draft_text.clone(),
            status,
            review.phase == PromptReviewPhase::Reviewing,
            review.phase == PromptReviewPhase::Reviewing,
        )
    } else {
        (
            String::new(),
            String::new(),
            "プロンプト推敲は開始されていません。".to_string(),
            false,
            false,
        )
    };
    let new_request_admission_open = composer_new_request_admission_is_open(
        runtime,
        busy,
        state.navigation_loading(),
        state.background_mutation_pending(),
        state.post_run_refresh_pending(),
    );
    let prompt_review_owner_is_current =
        state.app_state.prompt_review.as_ref().is_none_or(|review| {
            state.prompt_review_expected_active_turn(review.request_id)
                == Some(runtime.active_turn_expectation)
        });
    let steer_admission_open = composer_steer_admission_is_open(
        runtime,
        state.navigation_loading(),
        state.background_mutation_pending(),
    );
    let composer_submit_mode = if state.app_state.prompt_review.is_some() {
        DesktopComposerSubmitMode::Blocked
    } else {
        match runtime.active_turn_expectation {
            ActiveTurnExpectation::Idle { .. } if new_request_admission_open => {
                DesktopComposerSubmitMode::NewRequest
            }
            ActiveTurnExpectation::Turn { .. } if steer_admission_open => {
                DesktopComposerSubmitMode::Steer
            }
            _ => DesktopComposerSubmitMode::Blocked,
        }
    };
    let composer_admission_open = composer_submit_mode.admission_is_open();
    let image_input_enabled =
        desktop_image_input_delegates_capability_to_runtime(state) && composer_admission_open;
    let navigation_admission_open = navigation_admission_blocker(
        busy,
        state.background_mutation_pending(),
        state.navigation_loading(),
        runtime.root_run_finalizing,
    )
    .is_none()
        && state.app_state.prompt_review.is_none();
    let latest_tool_summary = latest_tool_public_summary(&state.app_state.tool_statuses);
    let (status_message, status_detail) = if pre_admission_active {
        ("実行を開始しています…".to_string(), String::new())
    } else {
        state
            .app_state
            .status_message
            .as_deref()
            .map(|message| display_status_projection(state.status_code, message))
            .unwrap_or_else(|| ("準備完了".to_string(), String::new()))
    };
    let confirmation_text = pending_permission
        .map(|(_, request)| format_permission_confirmation_text(request))
        .unwrap_or_default();
    let token_meter = token_meter_projection(
        state.app_state.latest_context_window.as_ref(),
        state.provider_config.effective_config.model.context_window,
    );
    let startup = startup_projection(state);
    let config_draft_edit_open = !state.background_mutation_pending();
    let config_draft_commit_open = !busy
        && !runtime.blocks_new_request()
        && !state.navigation_loading()
        && !state.background_mutation_pending();
    let external_config_owner_mutation_open = !state.background_mutation_pending();
    let access_mode_mutation_open =
        !state.navigation_loading() && !state.background_mutation_pending();
    let config_draft_capabilities = DesktopConfigDraftCapabilitiesProjection {
        clean: config_draft_capability_projection(
            false,
            config_draft_edit_open,
            config_draft_commit_open,
            startup.initial_setup_required,
            external_config_owner_mutation_open,
            access_mode_mutation_open,
        ),
        dirty: config_draft_capability_projection(
            true,
            config_draft_edit_open,
            config_draft_commit_open,
            startup.initial_setup_required,
            external_config_owner_mutation_open,
            access_mode_mutation_open,
        ),
    };
    let session_settings = session_settings_projection(
        state,
        runtime,
        config_draft_commit_open && !runtime.agent_tree_active,
        access_mode_mutation_open,
    );
    DesktopWebState {
        hub,
        mcp_publish: state
            .mcp_publish
            .as_ref()
            .map(crate::mcp_publish::PublishService::projection_now),
        device_network: state
            .device_network
            .as_ref()
            .map(crate::device_network::DeviceNetworkService::projection_now),
        mcp_activity: state
            .mcp_publish
            .as_ref()
            .and_then(crate::mcp_publish::PublishService::remote_activity_now),
        projection_revision: "0".to_string(),
        workspace_path: state.snapshot.workspace_path.clone(),
        provider_label: state
            .provider_config
            .effective_config
            .model
            .base_url
            .clone(),
        model_label: state.provider_config.effective_config.model.model.clone(),
        access_label: access_mode_key(
            state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
        )
        .to_string(),
        access_target: DesktopAccessModeMutationTargetProjection {
            workspace_path: state.snapshot.workspace_path.clone(),
            session_id: state
                .app_state
                .current_session_id
                .map(|session_id| session_id.to_string()),
            config_generation: state.provider_config.config_generation.to_string(),
            access_mode: state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            runtime_owner_token: access_runtime_owner_token(
                runtime.root_run_generation,
                runtime.agent_tree_active,
                runtime.last_root_run_epoch,
            ),
        },
        session_settings,
        config_draft_capabilities,
        current_session_label: state.current_session_label(),
        selected_session_title: state.selected_session_title(),
        status_message,
        status_detail,
        status_code: if pre_admission_active {
            DesktopStatusCode::Plain
        } else {
            state.status_code
        },
        run_status_key: if pre_admission_active {
            "running".to_string()
        } else {
            run_status_key(state.app_state.run_status).to_string()
        },
        run_status_text: if pre_admission_active {
            "実行準備中".to_string()
        } else {
            state.current_run_status_text()
        },
        run_phase: if pre_admission_active {
            "実行準備".to_string()
        } else {
            desktop_run_phase_label(state.app_state.progress.current_phase).to_string()
        },
        run_active_step: if pre_admission_active {
            "durable run admissionを確定しています".to_string()
        } else {
            display_run_step(&state.app_state.progress.active_step)
        },
        latest_tool_summary,
        plan: state
            .app_state
            .current_plan
            .as_ref()
            .map(|plan| DesktopPlanProjection {
                explanation: plan.explanation.clone(),
                steps: plan.steps.clone(),
            }),
        progress_text: if pre_admission_active {
            "実行準備中\nフェーズ: 実行準備\n手順: durable run admissionを確定しています"
                .to_string()
        } else {
            detail.progress_text
        },
        tool_status_text: detail.tool_status_text,
        token_meter_label: token_meter.label,
        token_meter_title: token_meter.title,
        token_meter_level: token_meter.level,
        session_usage_label: detail.session_usage_label,
        session_usage_title: detail.session_usage_title,
        session_usage_state: detail.session_usage_state,
        confirmation_visible: pending_permission.is_some(),
        confirmation_id: pending_permission.map(|(id, _)| id.to_string()),
        confirmation_text,
        confirmation: pending_permission.map(|(_, permission)| permission.into()),
        startup,
        composer_commit_generation: runtime.composer_commit_generation.to_string(),
        draft_prompt: state.composer.draft_prompt.clone(),
        draft_target: DesktopDraftActionTargetProjection {
            workspace_path: state.snapshot.workspace_path.clone(),
            session_id: state
                .app_state
                .current_session_id
                .map(|session_id| session_id.to_string()),
            owner_generation: state.composer.owner_generation().to_string(),
        },
        image_input: state.composer.image_attachment_input.clone(),
        attached_images: state
            .composer
            .image_attachment_paths
            .iter()
            .map(|path| path.to_string())
            .collect(),
        composer_submit_mode,
        can_submit: composer_admission_open && (root_run_active || main_route_ready),
        can_cancel_run: stop_target.is_some() && (busy || pending_permission.is_some()),
        run_target: DesktopRunMutationTargetProjection {
            workspace_path: state.snapshot.workspace_path.clone(),
            session_id: state
                .app_state
                .current_session_id
                .map(|session_id| session_id.to_string()),
            runtime_owner_token: access_runtime_owner_token(
                runtime.root_run_generation,
                runtime.agent_tree_active,
                runtime.last_root_run_epoch,
            ),
            permission_confirmation_id: pending_permission.map(|(id, _)| id.to_string()),
            expected_state: runtime.active_turn_expectation.into(),
        },
        stop_target,
        busy,
        task_activity_state,
        async_polling_required: state.async_polling_required()
            || hub_polling
            || state
                .mcp_publish
                .as_ref()
                .is_some_and(crate::mcp_publish::PublishService::polling_required)
            || state
                .device_network
                .as_ref()
                .is_some_and(crate::device_network::DeviceNetworkService::polling_required)
            || root_run_active
            || runtime.agent_tree_active
            || runtime.root_run_finalizing
            || runtime.side_chat.status == "running",
        pending_async_operations: {
            let mut keys = state.pending_async_operation_keys();
            if runtime.side_chat.status == "running" {
                keys.push("side_chat".to_string());
            }
            keys
        },
        navigation_loading: state.navigation_loading(),
        navigation_admission_open,
        turn_page_admission_open: state.can_begin_turn_page_load(),
        post_run_refresh_pending: state.post_run_refresh_pending(),
        background_mutation_pending: state.background_mutation_pending(),
        overlay: overlay_key(state.view.overlay).to_string(),
        about: about_projection(),
        side_chat,
        project_rows: state.snapshot.project_rows.clone(),
        selected_project_index: state.selected_project_index(),
        session_rows,
        chat_session_rows: state.snapshot.chat_session_rows.clone(),
        selected_session_index: state.selected_index(),
        session_search_text: state.view.session_search_text.clone(),
        session_search_include_archived: state.view.session_search_include_archived,
        thread_empty: detail.thread_empty,
        transcript_rows: detail.transcript_rows,
        pending_turn_inputs,
        turn_page_offset: detail.turn_page_offset,
        turn_page_limit: detail.turn_page_limit,
        turn_page_total: detail.turn_page_total,
        turn_page_has_more: detail.turn_page_has_more,
        artifact_rows: detail.artifacts,
        selected_artifact_index: state.selected_artifact_index(),
        artifact_preview_available: detail.artifact_preview_available,
        artifact_preview_text: state.selected_artifact_preview_text(),
        file_change_rows: detail.file_changes,
        file_change_summary_text: detail.file_change_summary_text,
        agent_activity_rows: runtime.agent_activity_rows.clone(),
        current_turn_agent_activity_rows: runtime.current_turn_agent_activity_rows.clone(),
        agent_tree_active: runtime.agent_tree_active,
        local_search_text: state.view.local_search_text.clone(),
        local_search_results_text: state.local_search_results_text(),
        command_rows: state.snapshot.command_rows.clone(),
        provider_base_url: state.provider_config.provider_base_url_input.clone(),
        provider_profile: state
            .provider_config
            .provider_profile_input
            .as_str()
            .to_string(),
        provider_api_key_env: state.provider_config.provider_api_key_env_input.clone(),
        provider_effective_base_url: state
            .provider_config
            .effective_config
            .model
            .base_url
            .clone(),
        provider_effective_profile: state
            .provider_config
            .effective_config
            .model
            .provider_profile
            .as_str()
            .to_string(),
        provider_effective_api_key_env: state
            .provider_config
            .effective_config
            .model
            .api_key_env
            .clone()
            .unwrap_or_default(),
        provider_effective_context_window: state
            .provider_config
            .effective_config
            .model
            .context_window
            .to_string(),
        provider_effective_model_id: state.provider_config.effective_config.model.model.clone(),
        provider_catalog_base_url: state.provider_config.provider_loaded_base_url.clone(),
        provider_catalog_profile: state
            .provider_config
            .provider_loaded_profile
            .map(|profile| profile.as_str().to_string()),
        provider_catalog_api_key_env: state.provider_config.provider_loaded_api_key_env.clone(),
        provider_context_window: state.provider_config.provider_context_window_input.clone(),
        provider_models: provider_model_labels(state),
        provider_model_ids: state.provider_config.provider_models.clone(),
        provider_selected_index: state.provider_config.provider_selected_index,
        provider_status: DesktopProviderStatusProjection {
            kind: state.provider_config.provider_status.kind.key().to_string(),
            title: state.provider_config.provider_status.title.clone(),
            hint: state.provider_config.provider_status.hint.clone(),
            details: provider_status_details(state),
        },
        provider_selected_model_summary: provider_selected_model_summary(state),
        provider_loading: state.provider_config.provider_loading,
        provider_apply_enabled: state.can_apply_provider_selection(),
        docling_readiness: state.docling_readiness.clone(),
        config_fields: ConfigField::ALL
            .into_iter()
            .filter(|field| !field.is_host_owned_generation())
            .map(|field| config_field_projection(field, state.global_config()))
            .collect(),
        config_target: DesktopConfigMutationTargetProjection {
            workspace_path: state.snapshot.workspace_path.clone(),
            session_id: state
                .app_state
                .current_session_id
                .map(|session_id| session_id.to_string()),
            config_generation: state.provider_config.config_generation.to_string(),
        },
        workspace_input: state.workspace_input.clone(),
        review_target: state.app_state.prompt_review.as_ref().map(|review| {
            DesktopPromptReviewMutationTargetProjection {
                workspace_path: state.snapshot.workspace_path.clone(),
                session_id: state
                    .app_state
                    .current_session_id
                    .map(|session_id| session_id.to_string()),
                owner_generation: state.composer.owner_generation().to_string(),
                request_id: review.request_id.to_string(),
                expected_state: state
                    .prompt_review_expected_active_turn(review.request_id)
                    .expect("active Desktop Prompt Review must retain its captured run owner")
                    .into(),
            }
        }),
        review_raw_text,
        review_draft_text,
        review_status_text,
        send_enhanced_enabled: send_enhanced_enabled
            && new_request_admission_open
            && prompt_review_owner_is_current,
        send_raw_enabled: send_raw_enabled
            && new_request_admission_open
            && prompt_review_owner_is_current,
        history_export_enabled: state.can_export_history() && !root_run_active,
        enhance_enabled: main_enhance_route_ready
            && new_request_admission_open
            && state.app_state.prompt_review.is_none(),
        image_input_enabled,
        window_opacity_percent: state.view.window_opacity_percent,
    }
}

fn config_field_projection(
    field: ConfigField,
    config: &ResolvedConfig,
) -> DesktopConfigFieldProjection {
    let descriptor = field.descriptor();
    let public = field.public_value(config);
    DesktopConfigFieldProjection {
        key: descriptor.key().to_string(),
        value: public.value,
        sensitive: public.sensitive,
        configured: public.configured,
        env_override: descriptor.env_override().map(ToString::to_string),
        value_type: descriptor.value_type().as_str().to_string(),
        required: descriptor.required(),
        min_value: descriptor.integer_min().map(|value| value as f64),
        max_value: descriptor.integer_max().map(|value| value as f64),
        options: descriptor
            .options()
            .iter()
            .map(ToString::to_string)
            .collect(),
    }
}

pub(crate) fn agent_activity_projection(
    workspace_path: &str,
    root_session_id: crate::session::SessionId,
    records: Vec<AgentActivityRecord>,
) -> (Vec<DesktopAgentActivityRow>, bool) {
    let mut rows = records
        .into_iter()
        .map(|record| DesktopAgentActivityRow {
            agent_path: record.agent_path.clone(),
            session_id: record.session_id.to_string(),
            task_name: record.task_name,
            task_preview: record.task_preview,
            status: agent_status_key(&record.status).to_string(),
            current_activity: record.current_activity,
            result_preview: record.result_preview,
            started_order: record.started_order,
            updated: record.updated,
            active_turn_id: record.active_turn_id.map(|turn_id| turn_id.to_string()),
            interrupt_target: record
                .interrupt_target
                .map(|target| DesktopAgentInterruptTarget {
                    workspace_path: workspace_path.to_string(),
                    root_session_id: root_session_id.to_string(),
                    agent_path: record.agent_path.clone(),
                    child_session_id: record.session_id.to_string(),
                    expected_turn_id: target.turn_id.to_string(),
                    admission_revision: target.admission_revision.to_string(),
                }),
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| row.started_order);
    let active = rows
        .iter()
        .any(|row| matches!(row.status.as_str(), "pending_init" | "running"));
    (rows, active)
}

fn agent_status_key(status: &AgentStatus) -> &'static str {
    match status {
        AgentStatus::PendingInit => "pending_init",
        AgentStatus::Running => "running",
        AgentStatus::AwaitingDescendants => "awaiting_descendants",
        AgentStatus::Interrupted => "interrupted",
        AgentStatus::Completed(_) => "completed",
        AgentStatus::Errored(_) => "errored",
        AgentStatus::Shutdown => "shutdown",
    }
}

fn desktop_image_input_delegates_capability_to_runtime(state: &DesktopState) -> bool {
    !state.is_busy() && !state.navigation_loading()
}

fn session_settings_projection(
    state: &DesktopState,
    runtime: &DesktopRuntimeProjection,
    provider_mutation_enabled: bool,
    access_mutation_enabled: bool,
) -> DesktopSessionSettingsProjection {
    let session = state
        .open_session
        .as_ref()
        .map(|open_session| open_session.session());
    let session = session.filter(|session| Some(session.id) == state.app_state.current_session_id);
    let Some(session) = session else {
        return DesktopSessionSettingsProjection {
            available: false,
            base_url: String::new(),
            model: String::new(),
            provider_profile: state
                .provider_config
                .effective_config
                .model
                .provider_profile
                .as_str()
                .to_string(),
            api_key_env: state
                .provider_config
                .effective_config
                .model
                .api_key_env
                .clone()
                .unwrap_or_default(),
            access_mode: state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            context_window: String::new(),
            context_window_inherited: true,
            provider_mutation_enabled: false,
            access_mutation_enabled: false,
            unavailable_reason:
                "root sessionを選択すると、このセッションだけの設定を変更できます。".to_string(),
            target: None,
        };
    };
    DesktopSessionSettingsProjection {
        available: true,
        base_url: session.base_url.clone(),
        model: session.model.clone(),
        provider_profile: session
            .provider_connection
            .as_ref()
            .map(|connection| connection.profile)
            .unwrap_or(
                state
                    .provider_config
                    .effective_config
                    .model
                    .provider_profile,
            )
            .as_str()
            .to_string(),
        api_key_env: session
            .provider_connection
            .as_ref()
            .and_then(|connection| connection.api_key_env.clone())
            .or_else(|| {
                (session.provider_connection.is_none())
                    .then(|| {
                        state
                            .provider_config
                            .effective_config
                            .model
                            .api_key_env
                            .clone()
                    })
                    .flatten()
            })
            .unwrap_or_default(),
        access_mode: session.access_mode,
        context_window: session
            .model_parameters
            .context_window
            .map(|value| value.to_string())
            .unwrap_or_default(),
        context_window_inherited: session.model_parameters.context_window.is_none(),
        provider_mutation_enabled,
        access_mutation_enabled,
        unavailable_reason: String::new(),
        target: Some(DesktopSessionSettingsMutationTargetProjection {
            workspace_path: state.snapshot.workspace_path.clone(),
            root_session_id: session.id.to_string(),
            settings_revision: session.session_settings_revision.to_string(),
            config_generation: state.provider_config.config_generation.to_string(),
            runtime_owner_token: access_runtime_owner_token(
                runtime.root_run_generation,
                runtime.agent_tree_active,
                runtime.last_root_run_epoch,
            ),
        }),
    }
}

fn startup_projection(state: &DesktopState) -> DesktopStartupProjection {
    let setup_target = state.startup.requires_initial_setup().then(|| {
        DesktopInitialSetupMutationTargetProjection {
            workspace_path: state.snapshot.workspace_path.clone(),
            global_config_path: state
                .startup
                .global_config_path
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            setup_generation: state.startup.setup_generation.to_string(),
        }
    });
    DesktopStartupProjection {
        status: startup_status_key(state.startup.status).to_string(),
        title: state.startup.title.clone(),
        message: state.startup.message.clone(),
        detail: state.startup.detail.clone(),
        action_overlay: state
            .startup
            .action_overlay
            .map(overlay_key)
            .unwrap_or("none")
            .to_string(),
        initial_setup_required: state.startup.requires_initial_setup(),
        initial_setup_reason: state
            .startup
            .initial_setup_reason
            .map(|reason| reason.key().to_string()),
        global_config_path: state
            .startup
            .global_config_path
            .as_ref()
            .map(ToString::to_string),
        setup_target,
        checks: state
            .startup
            .checks
            .iter()
            .map(|check| DesktopStartupCheckProjection {
                key: check.key.to_string(),
                label: check.label.to_string(),
                status: startup_check_status_key(check.status).to_string(),
                message: check.message.clone(),
            })
            .collect(),
    }
}

fn startup_status_key(status: DesktopStartupStatus) -> &'static str {
    match status {
        DesktopStartupStatus::Ready => "ready",
        DesktopStartupStatus::RequiresConfig => "requires_config",
        DesktopStartupStatus::RequiresProvider => "requires_provider",
    }
}

fn startup_check_status_key(status: DesktopStartupCheckStatus) -> &'static str {
    match status {
        DesktopStartupCheckStatus::Pass => "pass",
        DesktopStartupCheckStatus::Warning => "warning",
        DesktopStartupCheckStatus::Fail => "fail",
    }
}

fn run_status_key(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Idle => "idle",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Cancelled => "cancelled",
        RunStatus::Failed => "failed",
    }
}

fn access_mode_key(access: AccessMode) -> &'static str {
    match access {
        AccessMode::Default => "default",
        AccessMode::AutoReview => "auto_review",
        AccessMode::FullAccess => "full_access",
    }
}

fn overlay_key(overlay: DesktopOverlay) -> &'static str {
    match overlay {
        DesktopOverlay::None => "none",
        DesktopOverlay::InitialSetup => "initial_setup",
        DesktopOverlay::FileMenu => "file_menu",
        DesktopOverlay::EditMenu => "edit_menu",
        DesktopOverlay::ViewMenu => "view_menu",
        DesktopOverlay::HelpMenu => "help_menu",
        DesktopOverlay::ProjectMenu => "project_menu",
        DesktopOverlay::ConfigEditor => "config",
        DesktopOverlay::HubConnection => "hub",
        DesktopOverlay::McpHistory => "mcp_history",
        DesktopOverlay::SessionSettings => "session_settings",
        DesktopOverlay::ProviderEditor => "provider",
        DesktopOverlay::WorkspacePicker => "workspace",
        DesktopOverlay::PromptReview => "prompt_review",
        DesktopOverlay::CommandPalette => "command_palette",
        DesktopOverlay::KeyboardShortcuts => "shortcuts",
        DesktopOverlay::About => "about",
    }
}

fn about_projection() -> DesktopAboutProjection {
    let copyright_notice = BUNDLED_LICENSE_TEXT
        .lines()
        .find(|line| line.trim_start().starts_with("Copyright"))
        .expect("bundled LICENSE must contain a copyright notice");
    DesktopAboutProjection {
        product_name: MOYAI_PRODUCT_NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        codename: MOYAI_DESKTOP_CODENAME.to_string(),
        license_identifier: env!("CARGO_PKG_LICENSE").to_string(),
        copyright_notice: copyright_notice.to_string(),
    }
}

fn provider_model_labels(state: &DesktopState) -> Vec<String> {
    state
        .provider_config
        .provider_models
        .iter()
        .map(|label| {
            state
                .provider_config
                .provider_model_infos
                .iter()
                .find(|info| info.id == *label)
                .map(|info| {
                    let summary = super::state::provider_model_summary(info);
                    if summary.is_empty() {
                        label.clone()
                    } else {
                        format!("{label}  [{summary}]")
                    }
                })
                .unwrap_or_else(|| label.clone())
        })
        .collect()
}

fn provider_status_details(state: &DesktopState) -> String {
    let profile = match state.provider_config.provider_profile_input {
        ProviderProfile::LmStudio => "Connection type: LM Studio (Responses API).",
        ProviderProfile::OpenAiCompatible => {
            "Connection type: OpenAI-compatible (Chat Completions)."
        }
        ProviderProfile::OpenAiResponses => "Connection type: OpenAI Responses API.",
        ProviderProfile::LmStudioChatCompletions => {
            "Connection type: LM Studio (Chat Completions)."
        }
    };
    let limits = format!(
        "moyAI local context budget: {}. Output length and generation behavior use the Provider host settings.",
        state.provider_config.provider_context_window_input,
    );
    [
        state.provider_config.provider_status.details.as_str(),
        profile,
        limits.as_str(),
    ]
    .into_iter()
    .filter(|line| !line.trim().is_empty())
    .collect::<Vec<_>>()
    .join("\n")
}

fn provider_selected_model_summary(state: &DesktopState) -> Vec<String> {
    let Some(info) = state.selected_provider_model_info() else {
        return vec!["選択中のモデル metadata はまだありません。".to_string()];
    };
    let mut lines = vec![
        format!("Model: {}", info.id),
        format!("Metadata source: {}", info.source),
        format!(
            "Load state: {}",
            match info.load_state {
                ProviderModelLoadState::Loaded => "loaded",
                ProviderModelLoadState::NotLoaded => "not loaded",
                ProviderModelLoadState::Unknown => "unknown",
            }
        ),
        format!(
            "Context: {}",
            info.context_window
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
        format!(
            "Provider metadata max output: {}",
            info.max_output_tokens
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
        format!(
            "Metadata images: {}",
            metadata_capability_label(info.supports_images)
        ),
        format!(
            "Metadata tools: {}",
            metadata_capability_label(info.supports_tools)
        ),
        format!(
            "Metadata reasoning: {}",
            metadata_capability_label(info.supports_reasoning)
        ),
    ];
    lines.push(format!(
        "Parallel prediction: {}",
        info.max_parallel_predictions
            .filter(|value| *value > 1)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none/reported as serial".to_string())
    ));
    lines
}

fn metadata_capability_label(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "reported supported",
        Some(false) => "not reported as supported",
        None => "not reported",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TokenMeterProjection {
    label: String,
    title: String,
    level: String,
}

fn token_meter_projection(
    status: Option<&crate::context::ContextWindowTokenStatus>,
    configured_limit: u32,
) -> TokenMeterProjection {
    let Some(status) = status else {
        return TokenMeterProjection {
            label: format!("-- / {} 未計測", compact_token_count(configured_limit)),
            title: "次回 model request 作成時に概算 token 使用量を表示します。".to_string(),
            level: "unknown".to_string(),
        };
    };

    let limit = status.full_context_window_limit.max(1);
    let ratio = f64::from(status.active_context_tokens) / f64::from(limit);
    let percent = (ratio * 100.0).round() as u32;
    let (level, level_label) = token_meter_level(status.token_limit_reached, ratio);
    TokenMeterProjection {
        label: format!(
            "{} / {} {}",
            compact_token_count(status.active_context_tokens),
            compact_token_count(status.full_context_window_limit),
            level_label
        ),
        title: format!(
            "概算 token 使用量: {} / {} ({}%). configured overflow margin: {}、残り推定: {}。出力量はProvider側の設定を使用します。",
            status.active_context_tokens,
            status.full_context_window_limit,
            percent,
            status.overflow_margin_tokens,
            status.tokens_until_limit
        ),
        level: level.to_string(),
    }
}

fn token_meter_level(limit_reached: bool, ratio: f64) -> (&'static str, &'static str) {
    if limit_reached {
        return ("critical", "上限");
    }
    if ratio >= 0.85 {
        ("critical", "非常に高い")
    } else if ratio >= 0.65 {
        ("high", "高い")
    } else if ratio >= 0.35 {
        ("medium", "中")
    } else {
        ("low", "低い")
    }
}

fn compact_token_count(value: u32) -> String {
    if value >= 1_000_000 {
        trim_trailing_decimal(format!("{:.1}m", f64::from(value) / 1_000_000.0))
    } else if value >= 100_000 {
        format!("{}k", value / 1_000)
    } else if value >= 1_000 {
        trim_trailing_decimal(format!("{:.1}k", f64::from(value) / 1_000.0))
    } else {
        value.to_string()
    }
}

fn trim_trailing_decimal(value: String) -> String {
    value.replace(".0", "")
}

fn display_status_projection(code: DesktopStatusCode, message: &str) -> (String, String) {
    match code {
        DesktopStatusCode::GoalControl => {
            return (
                "Goal の状態を確認しました。".to_string(),
                message.to_string(),
            );
        }
        DesktopStatusCode::ProviderTransport => {
            return (
                "LLMに接続できません。LLM URL とモデル設定を確認してください。".to_string(),
                message.to_string(),
            );
        }
        DesktopStatusCode::ModelUnavailable => {
            return (
                "設定中のモデルが見つかりません。モデル名と LLM URL を確認してください。"
                    .to_string(),
                message.to_string(),
            );
        }
        DesktopStatusCode::ImageUnsupported => {
            return (
                "このモデルは画像入力に対応していません。画像対応モデルを選択してください。"
                    .to_string(),
                message.to_string(),
            );
        }
        DesktopStatusCode::ImageAttachmentInvalid => {
            return (
                "画像を添付できませんでした。存在する PNG / JPEG / WebP / GIF ファイルを指定してください。"
                    .to_string(),
                message.to_string(),
            );
        }
        DesktopStatusCode::PermissionPolicyDenied => {
            return (
                "操作が許可されませんでした。アクセス設定と対象を確認してください。".to_string(),
                message.to_string(),
            );
        }
        DesktopStatusCode::ConfigImportFailed => {
            return (
                "設定ファイルをImportできませんでした。選択したTOMLを確認してください。"
                    .to_string(),
                message.to_string(),
            );
        }
        DesktopStatusCode::Plain
        | DesktopStatusCode::ApprovalAborted
        | DesktopStatusCode::UserStopped
        | DesktopStatusCode::AgentInterrupted
        | DesktopStatusCode::TreeStopped => {}
    }
    if message == "run completed" {
        return ("実行完了".to_string(), String::new());
    }
    if let Some(rest) = message.strip_prefix("assistant running on ") {
        return (format!("実行中: {rest}"), String::new());
    }
    match message {
        "Image attached to the next prompt." => {
            ("画像を次の依頼に添付しました。".to_string(), String::new())
        }
        "Image attachments cleared." => ("画像添付を解除しました。".to_string(), String::new()),
        "Enter an image path before attaching." => (
            "画像ファイルのパスを入力してください。".to_string(),
            String::new(),
        ),
        "Image is already attached." => (
            "この画像はすでに添付されています。".to_string(),
            String::new(),
        ),
        _ if message.starts_with("Removed image attachment") => {
            ("画像添付を1件削除しました。".to_string(), String::new())
        }
        _ => (message.to_string(), String::new()),
    }
}

pub(crate) fn format_permission_confirmation_text(permission: &PermissionRequest) -> String {
    let targets = if permission.targets.is_empty() {
        "(なし)".to_string()
    } else {
        permission
            .targets
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let risks = if permission.risks.is_empty() {
        "なし".to_string()
    } else {
        permission
            .risks
            .iter()
            .map(|risk| risk.label())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let details = if permission.details.is_empty() {
        "なし".to_string()
    } else {
        permission.details.join("\n")
    };
    format!(
        "{}\n\n実行内容:\n{details}\n\n対象: {targets}\nワークスペース外: {}\nリスク: {risks}",
        permission.summary,
        if permission.outside_workspace {
            "はい"
        } else {
            "いいえ"
        }
    )
}

fn display_run_step(step: &str) -> String {
    let trimmed = step.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(rest) = trimmed.strip_prefix("Model request ") {
        if let Some((request, tools)) = rest.split_once(" with ") {
            if let Some(tool_count) = tools.strip_suffix(" tools") {
                return format!(
                    "モデル応答 {}（ツール {}件）",
                    request.trim(),
                    tool_count.trim()
                );
            }
        }
    }
    if let Some(rest) = trimmed.strip_prefix("Running ") {
        return format!("実行中: {}", rest.trim());
    }
    trimmed.to_string()
}

fn latest_tool_public_summary(statuses: &[ToolStatusView]) -> String {
    let selected = statuses
        .iter()
        .rev()
        .find(|tool| {
            matches!(
                tool.status,
                ToolCallStatus::Failed | ToolCallStatus::Declined
            )
        })
        .or_else(|| {
            statuses.iter().rev().find(|tool| {
                matches!(
                    tool.status,
                    ToolCallStatus::Pending | ToolCallStatus::Running
                )
            })
        })
        .or_else(|| statuses.last());
    let Some(tool) = selected else {
        return "ツール待機中".to_string();
    };
    let action = tool_action_label(tool.tool);
    match tool.status {
        ToolCallStatus::Pending => format!("実行中: {action}"),
        ToolCallStatus::Running => format!("実行中: {action}"),
        ToolCallStatus::Completed => format!("完了: {action}"),
        ToolCallStatus::Declined => format!("要確認: {action}は実行されませんでした"),
        ToolCallStatus::Cancelled => format!("キャンセル: {action}"),
        ToolCallStatus::Failed => format!("要確認: {action}を完了できませんでした"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResolvedConfig;

    #[test]
    fn side_chat_default_is_a_valid_idle_unconfigured_projection() {
        let projection = DesktopSideChatProjection::default();
        assert!(!projection.configured);
        assert_eq!(projection.status, "idle");
        assert_eq!(projection.generation, "0");
        assert_eq!(projection.draft_quote, None);
        assert!(!projection.can_send);
        assert!(!projection.can_cancel);
    }

    #[test]
    fn system_prompt_projections_serialize_values_but_debug_only_character_counts() {
        for (key, prompt) in [
            ("model.system_prompt", "MAIN_PRIVATE_PROMPT"),
            ("side_chat.system_prompt", "SIDE_PRIVATE_PROMPT"),
        ] {
            let field = DesktopConfigFieldProjection {
                key: key.to_string(),
                value: prompt.to_string(),
                sensitive: false,
                configured: true,
                env_override: None,
                value_type: "string".to_string(),
                required: false,
                min_value: None,
                max_value: None,
                options: Vec::new(),
            };
            let field_debug = format!("{field:?}");
            assert!(field_debug.contains("value_chars"));
            assert!(!field_debug.contains(prompt));
            assert_eq!(
                serde_json::to_value(&field).expect("serialize config field")["value"],
                prompt
            );
        }

        let side = DesktopSideChatProjection {
            system_prompt: "SIDE_PRIVATE_PROMPT".to_string(),
            ..DesktopSideChatProjection::default()
        };
        let side_debug = format!("{side:?}");
        assert!(side_debug.contains("system_prompt_chars"));
        assert!(!side_debug.contains("SIDE_PRIVATE_PROMPT"));
        assert_eq!(
            serde_json::to_value(&side).expect("serialize side projection")["system_prompt"],
            "SIDE_PRIVATE_PROMPT"
        );
    }

    #[test]
    fn latest_tool_public_summary_is_failure_first_and_hides_raw_details() {
        let statuses = vec![
            crate::tui::state::ToolStatusView {
                tool_call_id: crate::session::ToolCallId::new(),
                tool: crate::tool::ToolName::ApplyPatch,
                title: "C:/private/workspace/secret.rs".to_string(),
                status: ToolCallStatus::Failed,
                summary: None,
                error: Some("request req_123 failed at http://private-host:9443".to_string()),
            },
            crate::tui::state::ToolStatusView {
                tool_call_id: crate::session::ToolCallId::new(),
                tool: crate::tool::ToolName::Shell,
                title: "cargo test --all-features".to_string(),
                status: ToolCallStatus::Completed,
                summary: Some("complete".to_string()),
                error: None,
            },
        ];

        let summary = latest_tool_public_summary(&statuses);

        assert_eq!(summary, "要確認: ファイルの更新を完了できませんでした");
        assert!(!summary.contains("req_123"));
        assert!(!summary.contains("private-host"));
        assert!(!summary.contains("secret.rs"));
    }
    use crate::config::merge::{apply_patch, normalize_request_timeout_alias};
    use crate::config::model::{MAX_MODEL_REQUEST_TIMEOUT_MS, PartialResolvedConfig};
    use crate::tui::state::RunProgressPhase;

    #[test]
    fn unknown_free_form_status_with_error_keywords_is_not_reclassified() {
        for message in [
            "permission approval aborted by user",
            "run cancelled by user",
            "storage connection refused while loading model 404",
            "provider failed after reporting permission denied by user",
        ] {
            assert_eq!(
                display_status_projection(DesktopStatusCode::Plain, message),
                (message.to_string(), String::new()),
                "message={message:?}"
            );
        }
    }

    #[test]
    fn typed_status_code_selects_specialized_guidance_without_message_inference() {
        let message = "opaque diagnostic";
        let (goal, goal_detail) = display_status_projection(
            DesktopStatusCode::GoalControl,
            "Goal: 一時停止\nlong objective",
        );
        assert_eq!(goal, "Goal の状態を確認しました。");
        assert_eq!(goal_detail, "Goal: 一時停止\nlong objective");
        let (provider, provider_detail) =
            display_status_projection(DesktopStatusCode::ProviderTransport, message);
        assert!(provider.contains("LLMに接続できません"));
        assert_eq!(provider_detail, message);

        let (model, model_detail) =
            display_status_projection(DesktopStatusCode::ModelUnavailable, message);
        assert!(model.contains("モデルが見つかりません"));
        assert_eq!(model_detail, message);

        let (image, image_detail) =
            display_status_projection(DesktopStatusCode::ImageUnsupported, message);
        assert!(image.contains("画像入力に対応していません"));
        assert_eq!(image_detail, message);

        let (attachment, attachment_detail) =
            display_status_projection(DesktopStatusCode::ImageAttachmentInvalid, message);
        assert!(attachment.contains("画像を添付できませんでした"));
        assert!(attachment.contains("PNG / JPEG / WebP / GIF"));
        assert_eq!(attachment_detail, message);

        let (permission, permission_detail) =
            display_status_projection(DesktopStatusCode::PermissionPolicyDenied, message);
        assert!(permission.contains("許可されませんでした"));
        assert_eq!(permission_detail, message);

        let (config_import, config_import_detail) =
            display_status_projection(DesktopStatusCode::ConfigImportFailed, message);
        assert!(config_import.contains("Importできませんでした"));
        assert_eq!(config_import_detail, message);
    }

    #[test]
    fn provider_profile_status_explains_the_atomic_connection_type() {
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            crate::config::ResolvedConfig::default(),
        );
        state.provider_config.provider_profile_input = ProviderProfile::OpenAiCompatible;

        let details = provider_status_details(&state);

        assert!(details.contains("OpenAI-compatible (Chat Completions)"));
        assert!(details.contains("moyAI local context budget"));
        assert!(details.contains("Provider host settings"));
        assert!(!details.contains("max_output_tokens"));
        assert!(!details.contains("language"));
        assert!(!details.contains("no-thinking"));
    }

    #[test]
    fn config_field_metadata_matches_rust_parser_shapes_and_bounds() {
        let agents = ConfigField::MultiAgentMaxAgents.descriptor();
        assert_eq!(agents.value_type().as_str(), "integer");
        assert_eq!(agents.integer_min(), Some(1));

        let context = ConfigField::ContextWindow.descriptor();
        assert_eq!(context.value_type().as_str(), "integer");
        assert_eq!(context.integer_min(), Some(1));
        assert_eq!(context.integer_max(), Some(u32::MAX as u64));

        let parallel = ConfigField::MaxParallelPredictions.descriptor();
        assert_eq!(parallel.integer_min(), Some(1));
        let context_projection =
            config_field_projection(ConfigField::ContextWindow, &ResolvedConfig::default());
        assert_eq!(context_projection.min_value, Some(1.0));

        let retries = ConfigField::MaxRetries.descriptor();
        assert_eq!(retries.integer_max(), Some(u8::MAX as u64));

        let response_timeout = ConfigField::RequestTimeoutMs.descriptor();
        assert_eq!(response_timeout.value_type().as_str(), "integer");
        assert_eq!(response_timeout.integer_min(), Some(1));
        assert_eq!(
            response_timeout.integer_max(),
            Some(MAX_MODEL_REQUEST_TIMEOUT_MS)
        );

        let temperature = ConfigField::Temperature.descriptor();
        assert_eq!(temperature.value_type().as_str(), "number");

        let mode = ConfigField::MultiAgentMode.descriptor();
        assert_eq!(mode.value_type().as_str(), "enum");
        assert_eq!(mode.options(), &["explicit_request_only", "proactive"]);

        let access = ConfigField::AccessMode.descriptor();
        assert_eq!(access.value_type().as_str(), "enum");
        assert_eq!(access.options(), &["default", "auto_review", "full_access"]);
        assert_eq!(access_mode_key(AccessMode::AutoReview), "auto_review");
    }

    #[test]
    fn docling_readiness_projects_typed_status_and_pending_operation() {
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            ResolvedConfig::default(),
        );
        state.begin_docling_readiness_check("https://docling.example.test/ready".to_string());

        let projection = desktop_web_state(&state, &DesktopRuntimeProjection::default());
        let json = serde_json::to_value(&projection).expect("serialize Desktop projection");

        assert_eq!(json["docling_readiness"]["status"], "checking");
        assert_eq!(
            json["docling_readiness"]["endpoint"],
            "https://docling.example.test/ready"
        );
        assert_eq!(
            json["docling_readiness"]["httpStatus"],
            serde_json::Value::Null
        );
        assert!(
            projection
                .pending_async_operations
                .contains(&"docling_readiness_check".to_string())
        );
    }

    #[test]
    fn every_config_field_projects_the_complete_editor_empty_value_contract() {
        let config = ResolvedConfig::default();
        let projected = ConfigField::ALL
            .into_iter()
            .map(|field| config_field_projection(field, &config))
            .collect::<Vec<_>>();

        assert_eq!(projected.len(), ConfigField::ALL.len());
        for (field, projection) in ConfigField::ALL.into_iter().zip(projected.iter()) {
            assert_eq!(projection.key, field.label());
            assert_eq!(
                projection.required,
                field.descriptor().required(),
                "{} must project the same empty-value rule used by the complete editor",
                field.label()
            );
        }

        assert!(ConfigField::Model.descriptor().required());
        assert!(!ConfigField::Temperature.descriptor().required());
        assert!(!ConfigField::ExtraBodyJson.descriptor().required());
    }

    #[test]
    fn sensitive_config_projection_exposes_only_configured_state() {
        let mut config = ResolvedConfig::default();
        let secrets = [
            "model-header-super-secret",
            "model-body-super-secret",
            "docling-header-super-secret",
            "mcp-header-super-secret",
        ];
        config
            .model
            .extra_headers
            .insert("Authorization".to_string(), secrets[0].to_string());
        config.model.extra_body_json = Some(serde_json::json!({"token": secrets[1]}));
        config
            .docling
            .headers
            .insert("Authorization".to_string(), secrets[2].to_string());
        config.mcp.servers[0]
            .headers
            .insert("Authorization".to_string(), secrets[3].to_string());

        let projected = ConfigField::ALL
            .into_iter()
            .map(|field| config_field_projection(field, &config))
            .collect::<Vec<_>>();
        for field in projected.iter().filter(|field| field.sensitive) {
            assert!(field.value.is_empty(), "{} must be redacted", field.key);
            assert!(
                field.configured,
                "{} must retain configured state",
                field.key
            );
        }
        assert_eq!(projected.iter().filter(|field| field.sensitive).count(), 4);

        let json = serde_json::to_string(&projected).expect("serialize public config projection");
        let debug = format!("{projected:?}");
        for secret in secrets {
            assert!(!json.contains(secret));
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn desktop_web_state_does_not_project_host_owned_generation_fields() {
        let state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            ResolvedConfig::default(),
        );

        let projection = desktop_web_state(&state, &DesktopRuntimeProjection::default());
        let host_owned = ConfigField::ALL
            .into_iter()
            .filter(|field| field.is_host_owned_generation())
            .collect::<Vec<_>>();

        assert_eq!(host_owned.len(), 10);
        assert_eq!(
            projection.config_fields.len(),
            ConfigField::ALL.len() - host_owned.len()
        );
        for field in host_owned {
            assert!(
                projection
                    .config_fields
                    .iter()
                    .all(|projected| projected.key != field.label()),
                "{} must not cross the Desktop GUI DTO boundary",
                field.label()
            );
        }
    }

    #[test]
    fn prompt_review_projects_an_exact_request_target_only_while_the_review_is_active() {
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            crate::config::ResolvedConfig::default(),
        );
        assert!(
            desktop_web_state(&state, &DesktopRuntimeProjection::default())
                .review_target
                .is_none()
        );

        let session_id = crate::session::SessionId::new();
        state.app_state.current_session_id = Some(session_id);
        state.rebind_composer_owner(Some(session_id));
        let owner_generation = state.composer.owner_generation();
        state.begin_prompt_enhance(42, "raw prompt", tokio_util::sync::CancellationToken::new());

        let projection = desktop_web_state(&state, &DesktopRuntimeProjection::default());
        let target = projection.review_target.expect("active review target");
        assert_eq!(target.workspace_path, "C:/workspace");
        assert_eq!(target.session_id, Some(session_id.to_string()));
        assert_eq!(target.owner_generation, owner_generation.to_string());
        assert_eq!(target.request_id, "42");
        let value = serde_json::to_value(target).expect("serialize review target");
        assert_eq!(value["workspacePath"], "C:/workspace");
        assert_eq!(value["requestId"], "42");
        assert!(value.get("request_id").is_none());
    }

    #[test]
    fn run_and_prompt_review_targets_serialize_exact_nested_expected_state_keys() {
        let session_id = crate::session::SessionId::new().to_string();
        let idle_turn_id = crate::protocol::TurnId::new().to_string();
        let active_turn_id = crate::protocol::TurnId::new().to_string();
        for (expected_state, expected_json) in [
            (
                DesktopRunExpectedStateProjection::Idle {
                    latest_turn_id: Some(idle_turn_id.clone()),
                    admission_revision: "18446744073709551614".to_string(),
                },
                serde_json::json!({
                    "kind": "idle",
                    "latestTurnId": idle_turn_id,
                    "admissionRevision": "18446744073709551614",
                }),
            ),
            (
                DesktopRunExpectedStateProjection::Turn {
                    turn_id: active_turn_id.clone(),
                    admission_revision: "18446744073709551615".to_string(),
                },
                serde_json::json!({
                    "kind": "turn",
                    "turnId": active_turn_id,
                    "admissionRevision": "18446744073709551615",
                }),
            ),
        ] {
            let run_target = DesktopRunMutationTargetProjection {
                workspace_path: "C:/workspace".to_string(),
                session_id: Some(session_id.clone()),
                runtime_owner_token: "root:9".to_string(),
                permission_confirmation_id: Some("41".to_string()),
                expected_state: expected_state.clone(),
            };
            let run_json = serde_json::to_value(&run_target).expect("serialize run target");
            assert_eq!(
                run_json,
                serde_json::json!({
                    "workspacePath": "C:/workspace",
                    "sessionId": session_id.clone(),
                    "runtimeOwnerToken": "root:9",
                    "permissionConfirmationId": "41",
                    "expectedState": expected_json.clone(),
                })
            );
            let run_roundtrip: DesktopRunMutationTargetProjection =
                serde_json::from_value(run_json.clone()).expect("deserialize run target");
            assert_eq!(
                serde_json::to_value(run_roundtrip).expect("reserialize run target"),
                run_json
            );

            let review_target = DesktopPromptReviewMutationTargetProjection {
                workspace_path: "C:/workspace".to_string(),
                session_id: Some(session_id.clone()),
                owner_generation: "18446744073709551615".to_string(),
                request_id: "42".to_string(),
                expected_state,
            };
            let review_json =
                serde_json::to_value(&review_target).expect("serialize Prompt Review target");
            assert_eq!(
                review_json,
                serde_json::json!({
                    "workspacePath": "C:/workspace",
                    "sessionId": session_id.clone(),
                    "ownerGeneration": "18446744073709551615",
                    "requestId": "42",
                    "expectedState": expected_json.clone(),
                })
            );
            let review_roundtrip: DesktopPromptReviewMutationTargetProjection =
                serde_json::from_value(review_json.clone())
                    .expect("deserialize Prompt Review target");
            assert_eq!(
                serde_json::to_value(review_roundtrip).expect("reserialize Prompt Review target"),
                review_json
            );
        }
    }

    #[test]
    fn about_projection_uses_cargo_metadata_and_the_bundled_license_notice() {
        let about = about_projection();

        assert_eq!(about.product_name, "moyAI");
        assert_eq!(about.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(about.license_identifier, env!("CARGO_PKG_LICENSE"));
        assert!(about.copyright_notice.starts_with("Copyright"));
        assert!(
            BUNDLED_LICENSE_TEXT
                .lines()
                .any(|line| line == about.copyright_notice.as_str())
        );
        assert_eq!(overlay_key(DesktopOverlay::About), "about");
    }

    #[test]
    fn migrated_legacy_timeout_projects_one_canonical_desktop_field() {
        let mut patch =
            toml::from_str::<PartialResolvedConfig>("[model]\nstream_idle_timeout_ms = 3600000\n")
                .expect("legacy import fixture");
        normalize_request_timeout_alias(
            &mut patch,
            "model.request_timeout_ms",
            "model.stream_idle_timeout_ms",
        )
        .expect("unambiguous legacy timeout");
        let resolved = apply_patch(ResolvedConfig::default(), patch);
        let projected = ConfigField::ALL
            .into_iter()
            .map(|field| config_field_projection(field, &resolved))
            .collect::<Vec<_>>();
        let timeout_fields = projected
            .iter()
            .filter(|field| field.key == "model.request_timeout_ms")
            .collect::<Vec<_>>();

        assert_eq!(timeout_fields.len(), 1);
        assert_eq!(timeout_fields[0].value, "3600000");
        assert_eq!(timeout_fields[0].value_type, "integer");
        assert_eq!(timeout_fields[0].min_value, Some(1.0));
        assert_eq!(
            timeout_fields[0].max_value,
            Some(MAX_MODEL_REQUEST_TIMEOUT_MS as f64)
        );
        assert!(
            projected
                .iter()
                .all(|field| field.key != "model.stream_idle_timeout_ms")
        );
    }

    #[test]
    fn root_finalizing_closes_but_child_only_activity_keeps_the_composer_gate_open() {
        assert!(composer_new_request_admission_is_open(
            &DesktopRuntimeProjection::default(),
            false,
            false,
            false,
            false,
        ));
        assert!(composer_steer_admission_is_open(
            &DesktopRuntimeProjection {
                root_run_generation: Some(7),
                active_turn_expectation: ActiveTurnExpectation::Turn {
                    turn_id: crate::protocol::TurnId::new(),
                    revision: 1,
                },
                ..DesktopRuntimeProjection::default()
            },
            false,
            false,
        ));
        assert!(!composer_steer_admission_is_open(
            &DesktopRuntimeProjection {
                root_run_generation: Some(7),
                root_run_finalizing: true,
                active_turn_expectation: ActiveTurnExpectation::Turn {
                    turn_id: crate::protocol::TurnId::new(),
                    revision: 1,
                },
                ..DesktopRuntimeProjection::default()
            },
            false,
            false,
        ));
        assert!(!composer_steer_admission_is_open(
            &DesktopRuntimeProjection {
                root_run_generation: Some(7),
                active_turn_expectation: ActiveTurnExpectation::Turn {
                    turn_id: crate::protocol::TurnId::new(),
                    revision: 1,
                },
                ..DesktopRuntimeProjection::default()
            },
            false,
            true,
        ));
        assert!(!composer_new_request_admission_is_open(
            &DesktopRuntimeProjection {
                root_run_finalizing: true,
                ..DesktopRuntimeProjection::default()
            },
            false,
            false,
            false,
            false,
        ));
        assert!(!composer_new_request_admission_is_open(
            &DesktopRuntimeProjection::default(),
            false,
            false,
            false,
            true,
        ));
        assert!(composer_new_request_admission_is_open(
            &DesktopRuntimeProjection {
                agent_tree_active: true,
                ..DesktopRuntimeProjection::default()
            },
            false,
            false,
            false,
            false,
        ));
    }

    #[test]
    fn access_runtime_owner_distinguishes_queued_commands_across_the_tree_lifecycle() {
        let lifecycle = [
            (None, false, 7, "idle:7"),
            (Some(8), false, 8, "root:8"),
            (None, true, 8, "tree:8"),
            (None, false, 8, "idle:8"),
        ];
        let mut tokens = std::collections::BTreeSet::new();
        for (root, tree, epoch, expected_token) in lifecycle {
            let token = access_runtime_owner_token(root, tree, epoch);
            assert_eq!(token, expected_token);
            assert!(
                tokens.insert(token),
                "each lifecycle boundary is a CAS barrier"
            );
        }
    }

    #[test]
    fn task_activity_state_uses_runtime_owned_precedence() {
        let running_tree = DesktopRuntimeProjection {
            agent_tree_active: true,
            ..DesktopRuntimeProjection::default()
        };
        let finalizing = DesktopRuntimeProjection {
            agent_tree_active: true,
            root_run_finalizing: true,
            ..DesktopRuntimeProjection::default()
        };

        assert_eq!(
            task_activity_state(&DesktopRuntimeProjection::default(), false, false),
            DesktopTaskActivityState::Idle
        );
        assert_eq!(
            task_activity_state(&DesktopRuntimeProjection::default(), true, false),
            DesktopTaskActivityState::Running
        );
        assert_eq!(
            task_activity_state(&running_tree, false, false),
            DesktopTaskActivityState::Running
        );
        assert_eq!(
            task_activity_state(&running_tree, true, true),
            DesktopTaskActivityState::Attention
        );
        assert_eq!(
            task_activity_state(&finalizing, true, true),
            DesktopTaskActivityState::Finalizing
        );
    }

    #[test]
    fn root_finalizing_is_projected_as_closed_navigation_admission() {
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            crate::config::ResolvedConfig::default(),
        );
        assert!(
            desktop_web_state(&state, &DesktopRuntimeProjection::default())
                .navigation_admission_open
        );
        let finalizing_state = desktop_web_state(
            &state,
            &DesktopRuntimeProjection {
                root_run_finalizing: true,
                ..DesktopRuntimeProjection::default()
            },
        );
        assert!(!finalizing_state.navigation_admission_open);
        assert_eq!(
            finalizing_state.task_activity_state,
            DesktopTaskActivityState::Finalizing
        );
        assert_eq!(
            serde_json::to_value(&finalizing_state).expect("serialize finalizing web state")["task_activity_state"],
            "finalizing"
        );
        assert_eq!(
            desktop_web_state(
                &state,
                &DesktopRuntimeProjection {
                    composer_commit_generation: 42,
                    root_run_generation: Some(u64::MAX),
                    ..DesktopRuntimeProjection::default()
                },
            )
            .composer_commit_generation,
            "42"
        );
        assert_eq!(
            desktop_web_state(
                &state,
                &DesktopRuntimeProjection {
                    root_run_generation: Some(u64::MAX),
                    ..DesktopRuntimeProjection::default()
                },
            )
            .access_target
            .runtime_owner_token,
            "root:18446744073709551615",
            "run generations cross the web boundary without JS number precision loss"
        );
        let operation_id = state.begin_project_delete_mutation();
        assert!(
            !desktop_web_state(&state, &DesktopRuntimeProjection::default())
                .config_draft_capabilities
                .clean
                .access_mode_mutation_enabled
        );
        assert!(state.finish_project_delete_mutation(operation_id));
        let child_only = desktop_web_state(
            &state,
            &DesktopRuntimeProjection {
                agent_tree_active: true,
                last_root_run_epoch: 8,
                ..DesktopRuntimeProjection::default()
            },
        );
        assert!(
            child_only
                .config_draft_capabilities
                .clean
                .access_mode_mutation_enabled
        );
        assert_eq!(child_only.access_target.runtime_owner_token, "tree:8");
        let root_with_children = desktop_web_state(
            &state,
            &DesktopRuntimeProjection {
                agent_tree_active: true,
                root_run_generation: Some(8),
                last_root_run_epoch: 8,
                ..DesktopRuntimeProjection::default()
            },
        );
        assert!(
            root_with_children
                .config_draft_capabilities
                .clean
                .access_mode_mutation_enabled
        );
        assert_eq!(
            root_with_children.access_target.runtime_owner_token,
            "root:8"
        );

        state.begin_prompt_enhance(1, "raw review", tokio_util::sync::CancellationToken::new());
        assert!(state.finish_prompt_enhance(1, "edited review".to_string()));
        let idle_review = desktop_web_state(&state, &DesktopRuntimeProjection::default());
        assert!(idle_review.send_enhanced_enabled);
        assert!(idle_review.send_raw_enabled);
        state.app_state.run_status = crate::tui::state::RunStatus::Running;
        let running_review = desktop_web_state(&state, &DesktopRuntimeProjection::default());
        assert!(!running_review.send_enhanced_enabled);
        assert!(!running_review.send_raw_enabled);
    }

    #[test]
    fn pre_admission_root_is_projected_active_before_the_first_run_event() {
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            crate::config::ResolvedConfig::default(),
        );
        state.app_state.run_status = crate::tui::state::RunStatus::Completed;
        state.app_state.status_message = Some("run completed".to_string());
        state.app_state.progress.current_phase = RunProgressPhase::Terminal;
        state.app_state.progress.active_step = "previous run completed".to_string();

        let projection = desktop_web_state(
            &state,
            &DesktopRuntimeProjection {
                root_run_generation: Some(9),
                last_root_run_epoch: 9,
                ..DesktopRuntimeProjection::default()
            },
        );

        assert!(
            projection.busy,
            "Stop must be available during run admission"
        );
        assert_eq!(projection.run_status_key, "running");
        assert_eq!(projection.run_status_text, "実行準備中");
        assert_eq!(projection.run_phase, "実行準備");
        assert!(!projection.can_submit);
        assert!(!projection.navigation_admission_open);
        assert!(projection.async_polling_required);
        assert!(projection.can_cancel_run);
        assert_eq!(projection.access_target.runtime_owner_token, "root:9");
        assert!(matches!(
            projection.stop_target,
            Some(DesktopStopMutationTarget::Root {
                root_generation,
                latest_turn_id: None,
                ..
            }) if root_generation == "9"
        ));
    }

    #[test]
    fn rust_projects_clean_and_dirty_config_capabilities_without_owning_the_draft() {
        let state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            crate::config::ResolvedConfig::default(),
        );
        let idle = desktop_web_state(&state, &DesktopRuntimeProjection::default());
        assert!(!idle.config_draft_capabilities.clean.dirty);
        assert!(!idle.config_draft_capabilities.clean.discard_enabled);
        assert!(!idle.config_draft_capabilities.clean.commit_enabled);
        assert!(
            idle.config_draft_capabilities
                .clean
                .external_owner_mutation_open
        );
        assert!(idle.config_draft_capabilities.dirty.dirty);
        assert!(idle.config_draft_capabilities.dirty.discard_enabled);
        assert!(idle.config_draft_capabilities.dirty.commit_enabled);
        assert!(
            !idle
                .config_draft_capabilities
                .dirty
                .external_owner_mutation_open
        );

        for runtime in [
            DesktopRuntimeProjection {
                root_run_generation: Some(4),
                last_root_run_epoch: 4,
                ..DesktopRuntimeProjection::default()
            },
            DesktopRuntimeProjection {
                root_run_finalizing: true,
                last_root_run_epoch: 6,
                ..DesktopRuntimeProjection::default()
            },
        ] {
            assert!(
                !desktop_web_state(&state, &runtime)
                    .config_draft_capabilities
                    .dirty
                    .commit_enabled,
                "root run ownership must close config commit"
            );
        }
        let child_only = desktop_web_state(
            &state,
            &DesktopRuntimeProjection {
                agent_tree_active: true,
                last_root_run_epoch: 5,
                ..DesktopRuntimeProjection::default()
            },
        );
        assert!(
            child_only.config_draft_capabilities.dirty.commit_enabled,
            "a detached child-only tree must not retain root config ownership"
        );
    }

    #[test]
    fn config_target_uses_the_effective_runtime_session_not_list_selection() {
        let session_id = crate::session::SessionId::new();
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            crate::config::ResolvedConfig::default(),
        );
        state.app_state.current_session_id = Some(session_id);

        let projection = desktop_web_state(&state, &DesktopRuntimeProjection::default());

        assert_eq!(
            projection.config_target.session_id,
            Some(session_id.to_string())
        );
    }

    #[test]
    fn preferences_config_fields_project_global_values_not_root_effective_overrides() {
        let mut global = crate::config::ResolvedConfig::default();
        global.model.model = "global-model".to_string();
        global.model.context_window = 32_768;
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            global.clone(),
        );
        let mut root_effective = global.clone();
        root_effective.model.model = "root-model".to_string();
        root_effective.model.context_window = 131_072;
        state.reset_effective_config(root_effective);

        let projection = desktop_web_state(&state, &DesktopRuntimeProjection::default());
        let model = projection
            .config_fields
            .iter()
            .find(|field| field.key == ConfigField::Model.descriptor().key())
            .expect("model config field");
        let context_window = projection
            .config_fields
            .iter()
            .find(|field| field.key == ConfigField::ContextWindow.descriptor().key())
            .expect("context-window config field");

        assert_eq!(model.value, "global-model");
        assert_eq!(context_window.value, "32768");
    }

    #[test]
    fn inherited_session_local_context_projects_as_a_blank_optional_override() {
        let mut global = crate::config::ResolvedConfig::default();
        global.model.context_window = 32_768;
        global.model.max_output_tokens = 2_048;
        let session = crate::session::SessionRecord {
            id: crate::session::SessionId::new(),
            project_id: crate::session::ProjectId::new(),
            title: "session".to_string(),
            status: crate::session::SessionStatus::Running,
            cwd: camino::Utf8PathBuf::from("C:/workspace"),
            model: "session-model".to_string(),
            base_url: "http://127.0.0.1:1234".to_string(),
            access_mode: crate::config::AccessMode::Default,
            model_parameters: crate::session::SessionModelParameters::default(),
            provider_connection: None,
            session_settings_revision: 3,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: None,
        };
        let read = crate::session::CanonicalSessionRead {
            session: session.clone(),
            history: crate::session::CanonicalHistoryPage {
                session: session.clone(),
                offset: 0,
                limit: 1,
                total: 0,
                has_more: false,
                items: Vec::new(),
            },
            turns: crate::session::CanonicalTurnPage {
                session: session.clone(),
                offset: 0,
                limit: 1,
                total: 0,
                has_more: false,
                items: Vec::new(),
            },
            pending_turn_inputs: Vec::new(),
            turn_elapsed_ms: std::collections::HashMap::new(),
            session_token_usage: Default::default(),
            active_turn_progress: None,
            latest_turn_id: None,
            active_turn_id: None,
            active_turn_sequence_no: None,
            admission_revision: 0,
        };
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            global,
        );
        state.app_state.current_session_id = Some(session.id);
        state.load_open_session_preserving_history(&read);

        let projection =
            session_settings_projection(&state, &DesktopRuntimeProjection::default(), true, true);

        assert!(projection.context_window_inherited);
        assert_eq!(projection.context_window, "");
    }

    #[test]
    fn running_root_accepts_one_steer_without_reopening_new_run_actions() {
        let session_id = crate::session::SessionId::new();
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            crate::config::ResolvedConfig::default(),
        );
        state.app_state.current_session_id = Some(session_id);
        state.app_state.run_status = crate::tui::state::RunStatus::Running;
        let turn_id = crate::protocol::TurnId::new();
        let runtime = DesktopRuntimeProjection {
            root_run_generation: Some(8),
            last_root_run_epoch: 8,
            active_turn_expectation: ActiveTurnExpectation::Turn {
                turn_id,
                revision: 1,
            },
            ..DesktopRuntimeProjection::default()
        };

        let running = desktop_web_state(&state, &runtime);
        assert!(running.can_submit);
        assert_eq!(
            running.composer_submit_mode,
            DesktopComposerSubmitMode::Steer
        );
        assert!(!running.enhance_enabled);
        assert!(!running.send_enhanced_enabled);
        assert!(matches!(
            running.stop_target,
            Some(DesktopStopMutationTarget::Turn {
                turn_id: projected_turn,
                root_epoch,
                ..
            }) if projected_turn == turn_id.to_string() && root_epoch == "8"
        ));

        let running_with_child = desktop_web_state(
            &state,
            &DesktopRuntimeProjection {
                agent_tree_active: true,
                ..runtime.clone()
            },
        );
        assert!(running_with_child.can_submit);
        assert_eq!(
            running_with_child.composer_submit_mode,
            DesktopComposerSubmitMode::Steer
        );

        let operation_id = state.begin_steer_submission();
        let pending = desktop_web_state(&state, &runtime);
        assert!(!pending.can_submit);
        assert_eq!(
            pending.composer_submit_mode,
            DesktopComposerSubmitMode::Blocked
        );
        assert!(pending.background_mutation_pending);
        assert!(state.finish_steer_submission(operation_id));
    }

    #[test]
    fn session_rows_project_the_same_typed_stop_owner_as_their_command_route() {
        let current_session_id = crate::session::SessionId::new();
        let background_session_id = crate::session::SessionId::new();
        let current_turn_id = crate::protocol::TurnId::new();
        let background_turn_id = crate::protocol::TurnId::new();
        let mut current_row = super::super::models::DesktopSessionRow::from_parts(
            current_session_id,
            "current",
            crate::session::SessionStatus::Running,
        );
        current_row.active_turn_id = Some(current_turn_id);
        current_row.admission_revision = "1".to_string();
        let mut background_row = super::super::models::DesktopSessionRow::from_parts(
            background_session_id,
            "background",
            crate::session::SessionStatus::Running,
        );
        background_row.active_turn_id = Some(background_turn_id);
        background_row.admission_revision = "1".to_string();
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: vec![current_row, background_row],
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            crate::config::ResolvedConfig::default(),
        );
        state.app_state.current_session_id = Some(current_session_id);

        let projection = desktop_web_state(
            &state,
            &DesktopRuntimeProjection {
                root_run_generation: Some(9),
                last_root_run_epoch: 9,
                active_turn_expectation: ActiveTurnExpectation::Turn {
                    turn_id: current_turn_id,
                    revision: 1,
                },
                ..DesktopRuntimeProjection::default()
            },
        );

        assert_eq!(
            projection.session_rows[0].interrupt_target,
            projection.stop_target
        );
        assert_eq!(
            projection.session_rows[1].interrupt_target,
            Some(DesktopStopMutationTarget::Turn {
                workspace_path: "C:/workspace".to_string(),
                session_id: background_session_id.to_string(),
                turn_id: background_turn_id.to_string(),
                admission_revision: "1".to_string(),
                root_epoch: "9".to_string(),
            }),
        );
    }

    #[test]
    fn composer_mode_and_run_target_share_the_active_turn_expectation_owner() {
        let session_id = crate::session::SessionId::new();
        let turn_id = crate::protocol::TurnId::new();
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            crate::config::ResolvedConfig::default(),
        );
        state.app_state.current_session_id = Some(session_id);

        let turn_projection = desktop_web_state(
            &state,
            &DesktopRuntimeProjection {
                active_turn_expectation: ActiveTurnExpectation::Turn {
                    turn_id,
                    revision: 1,
                },
                ..DesktopRuntimeProjection::default()
            },
        );
        assert_eq!(
            turn_projection.composer_submit_mode,
            DesktopComposerSubmitMode::Steer,
            "a captured Turn remains a steer even before duplicate busy flags catch up"
        );
        assert!(turn_projection.can_submit);
        assert!(matches!(
            turn_projection.run_target.expected_state,
            DesktopRunExpectedStateProjection::Turn {
                turn_id: projected_turn_id,
                admission_revision,
            } if projected_turn_id == turn_id.to_string() && admission_revision == "1"
        ));

        state.app_state.run_status = crate::tui::state::RunStatus::Running;
        let idle_projection = desktop_web_state(
            &state,
            &DesktopRuntimeProjection {
                active_turn_expectation: ActiveTurnExpectation::Idle {
                    latest_turn_id: Some(turn_id),
                    revision: 1,
                },
                ..DesktopRuntimeProjection::default()
            },
        );
        assert_eq!(
            idle_projection.composer_submit_mode,
            DesktopComposerSubmitMode::Blocked,
            "a captured Idle must never be relabelled as a steer by stale busy state"
        );
        assert!(!idle_projection.can_submit);
        assert!(matches!(
            idle_projection.run_target.expected_state,
            DesktopRunExpectedStateProjection::Idle {
                latest_turn_id: Some(projected_turn_id),
                admission_revision,
            } if projected_turn_id == turn_id.to_string() && admission_revision == "1"
        ));
    }

    #[test]
    fn post_run_refresh_closes_new_request_until_the_durable_owner_settles() {
        let session_id = crate::session::SessionId::new();
        let turn_id = crate::protocol::TurnId::new();
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            crate::config::ResolvedConfig::default(),
        );
        state.app_state.current_session_id = Some(session_id);
        let runtime = DesktopRuntimeProjection {
            active_turn_expectation: ActiveTurnExpectation::Idle {
                latest_turn_id: Some(turn_id),
                revision: 2,
            },
            ..DesktopRuntimeProjection::default()
        };

        state.mark_post_run_refresh_pending();
        let pending = desktop_web_state(&state, &runtime);
        assert!(pending.post_run_refresh_pending);
        assert_eq!(
            pending.composer_submit_mode,
            DesktopComposerSubmitMode::Blocked
        );
        assert!(!pending.can_submit);

        state.clear_post_run_refresh_pending();
        let settled = desktop_web_state(&state, &runtime);
        assert!(!settled.post_run_refresh_pending);
        assert_eq!(
            settled.composer_submit_mode,
            DesktopComposerSubmitMode::NewRequest
        );
        assert!(settled.can_submit);
    }

    #[test]
    fn cancel_capability_is_owned_by_the_rust_runtime_projection() {
        let state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            crate::config::ResolvedConfig::default(),
        );
        assert!(!desktop_web_state(&state, &DesktopRuntimeProjection::default()).can_cancel_run);
        assert!(
            desktop_web_state(
                &state,
                &DesktopRuntimeProjection {
                    root_run_generation: Some(1),
                    ..DesktopRuntimeProjection::default()
                }
            )
            .can_cancel_run
        );
        assert!(
            !desktop_web_state(
                &state,
                &DesktopRuntimeProjection {
                    agent_tree_active: true,
                    ..DesktopRuntimeProjection::default()
                }
            )
            .can_cancel_run,
            "detached child liveness is not an ordinary root Stop target"
        );
        let permission = PermissionRequest {
            access: crate::workspace::AccessKind::Shell,
            summary: "confirm".to_string(),
            details: Vec::new(),
            targets: Vec::new(),
            outside_workspace: false,
            risks: Vec::new(),
            agent_path: None,
            agent_task_name: None,
        };
        assert!(
            !desktop_web_state_with_permission(
                &state,
                &DesktopRuntimeProjection::default(),
                Some((7, &permission)),
            )
            .can_cancel_run,
            "permission presence alone is not an exact Stop capability"
        );
        assert!(
            desktop_web_state_with_permission(
                &state,
                &DesktopRuntimeProjection {
                    root_run_generation: Some(1),
                    ..DesktopRuntimeProjection::default()
                },
                Some((7, &permission)),
            )
            .can_cancel_run,
            "a permission owned by an exact root generation remains stoppable"
        );
    }

    #[test]
    fn agent_activity_projection_preserves_contract_and_spawn_order() {
        let completed_session_id = crate::session::SessionId::new();
        let running_session_id = crate::session::SessionId::new();
        let root_session_id = crate::session::SessionId::new();
        let running_turn_id = crate::protocol::TurnId::new();
        let (rows, active) = agent_activity_projection(
            "C:/workspace",
            root_session_id,
            vec![
                AgentActivityRecord {
                    agent_path: "/root/review".to_string(),
                    session_id: completed_session_id,
                    task_name: "review".to_string(),
                    task_preview: "Review the implementation".to_string(),
                    status: AgentStatus::Completed(Some("reviewed".to_string())),
                    current_activity: String::new(),
                    result_preview: "reviewed".to_string(),
                    started_order: 2,
                    updated: true,
                    is_current_turn: false,
                    active_turn_id: None,
                    interrupt_target: None,
                },
                AgentActivityRecord {
                    agent_path: "/root/runtime".to_string(),
                    session_id: running_session_id,
                    task_name: "runtime".to_string(),
                    task_preview: "Implement runtime".to_string(),
                    status: AgentStatus::Running,
                    current_activity: "Running tests".to_string(),
                    result_preview: String::new(),
                    started_order: 1,
                    updated: false,
                    is_current_turn: true,
                    active_turn_id: Some(running_turn_id),
                    interrupt_target: Some(crate::app::agent_runtime::AgentInterruptTarget {
                        turn_id: running_turn_id,
                        admission_revision: 7,
                    }),
                },
            ],
        );

        assert!(active);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].agent_path, "/root/runtime");
        assert_eq!(rows[0].session_id, running_session_id.to_string());
        assert_eq!(rows[0].status, "running");
        assert_eq!(
            rows[0].interrupt_target,
            Some(DesktopAgentInterruptTarget {
                workspace_path: "C:/workspace".to_string(),
                root_session_id: root_session_id.to_string(),
                agent_path: "/root/runtime".to_string(),
                child_session_id: running_session_id.to_string(),
                expected_turn_id: running_turn_id.to_string(),
                admission_revision: "7".to_string(),
            })
        );
        assert_eq!(rows[1].agent_path, "/root/review");
        assert_eq!(rows[1].session_id, completed_session_id.to_string());
        assert_eq!(rows[1].status, "completed");
        assert!(rows[1].updated);
    }

    #[test]
    fn agent_status_projection_matches_desktop_web_union() {
        let cases = [
            (AgentStatus::PendingInit, "pending_init"),
            (AgentStatus::Running, "running"),
            (AgentStatus::AwaitingDescendants, "awaiting_descendants"),
            (AgentStatus::Interrupted, "interrupted"),
            (AgentStatus::Completed(None), "completed"),
            (AgentStatus::Errored("failed".to_string()), "errored"),
            (AgentStatus::Shutdown, "shutdown"),
        ];

        for (status, expected) in cases {
            assert_eq!(agent_status_key(&status), expected);
        }
    }

    #[test]
    fn final_agent_rows_do_not_keep_async_polling_active() {
        let (rows, active) = agent_activity_projection(
            "C:/workspace",
            crate::session::SessionId::new(),
            vec![AgentActivityRecord {
                agent_path: "/root/done".to_string(),
                session_id: crate::session::SessionId::new(),
                task_name: "done".to_string(),
                task_preview: String::new(),
                status: AgentStatus::Interrupted,
                current_activity: String::new(),
                result_preview: String::new(),
                started_order: 1,
                updated: false,
                is_current_turn: false,
                active_turn_id: None,
                interrupt_target: None,
            }],
        );

        assert_eq!(rows[0].status, "interrupted");
        assert!(!active);
    }

    #[test]
    fn token_meter_projection_formats_estimated_usage() {
        let status = crate::context::ContextWindowTokenStatus {
            source: crate::context::ActiveContextTokenSource::FullPreparedRequestEstimate,
            active_context_tokens: 12_345,
            full_context_window_limit: 124_518,
            configured_max_output_tokens: None,
            overflow_margin_tokens: 1_024,
            tokens_until_limit: 112_173,
            token_limit_reached: false,
        };

        let projection = token_meter_projection(Some(&status), 131_072);

        assert_eq!(projection.label, "12.3k / 124k 低い");
        assert_eq!(projection.level, "low");
        assert!(projection.title.contains("12345 / 124518"));
        assert!(
            projection
                .title
                .contains("出力量はProvider側の設定を使用します")
        );
        assert!(!projection.title.contains("設定output上限"));
        assert!(
            projection
                .title
                .contains("configured overflow margin: 1024")
        );
        assert!(!projection.title.contains("出力予約"));
    }

    #[test]
    fn token_meter_projection_marks_reached_limit() {
        let status = crate::context::ContextWindowTokenStatus {
            source: crate::context::ActiveContextTokenSource::FullPreparedRequestEstimate,
            active_context_tokens: 125_000,
            full_context_window_limit: 124_518,
            configured_max_output_tokens: None,
            overflow_margin_tokens: 1_024,
            tokens_until_limit: -482,
            token_limit_reached: true,
        };

        let projection = token_meter_projection(Some(&status), 131_072);

        assert_eq!(projection.level, "critical");
        assert!(projection.label.ends_with("上限"));
    }

    #[test]
    fn provider_phase_projection_labels_every_current_transport_boundary() {
        let current = [
            (
                crate::llm::ProviderPhase::AttemptStarted,
                "Provider要求開始",
            ),
            (
                crate::llm::ProviderPhase::RequestInFlight,
                "Provider要求処理中",
            ),
            (
                crate::llm::ProviderPhase::HeadersReceived,
                "Provider応答ヘッダー受信",
            ),
            (
                crate::llm::ProviderPhase::FirstProgress,
                "Provider応答受信中",
            ),
            (
                crate::llm::ProviderPhase::LastProgress,
                "Provider最終応答受信",
            ),
            (crate::llm::ProviderPhase::ProviderTerminal, "Provider完了"),
        ];

        for (phase, expected) in current {
            assert_eq!(
                desktop_run_phase_label(RunProgressPhase::Provider(phase)),
                expected
            );
        }
    }

    #[test]
    fn provider_phase_projection_never_exposes_the_wire_key_in_desktop_progress() {
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            crate::config::ResolvedConfig::default(),
        );
        state.app_state.current_session_id = Some(crate::session::SessionId::new());
        state.app_state.run_status = RunStatus::Running;
        state.app_state.progress.current_phase =
            RunProgressPhase::Provider(crate::llm::ProviderPhase::RequestInFlight);

        let projection = desktop_web_state(&state, &DesktopRuntimeProjection::default());

        assert_eq!(projection.run_phase, "Provider要求処理中");
        assert!(projection.progress_text.contains("Provider要求処理中"));
        assert!(!projection.run_phase.contains("request_in_flight"));
        assert!(!projection.progress_text.contains("request_in_flight"));
    }
}
