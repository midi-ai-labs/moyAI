use serde::{Deserialize, Serialize};

use super::models::{
    DesktopArtifactRow, DesktopCommandRow, DesktopFileChangeRow, DesktopProjectRow,
    DesktopSessionRow, DesktopTranscriptRow,
};
use super::query::desktop_run_phase_label;
use super::startup::{DesktopStartupCheckStatus, DesktopStartupStatus};
use super::state::{DesktopOverlay, DesktopState, DesktopStatusCode};
use crate::app::AgentActivityRecord;
use crate::config::{AccessMode, ProviderMetadataMode};
use crate::llm::ProviderModelLoadState;
use crate::runtime::AgentStatus;
use crate::tool::PermissionRequest;
use crate::tui::config_editor::{ConfigEditorState, ConfigField, ConfigFieldState};
use crate::tui::state::{PromptReviewPhase, RunStatus};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopPlanProjection {
    pub explanation: Option<String>,
    pub steps: Vec<crate::protocol::PlanStep>,
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
    pub checks: Vec<DesktopStartupCheckProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopConfigFieldProjection {
    pub key: String,
    pub value: String,
    pub env_override: Option<String>,
    pub value_type: String,
    pub required: bool,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub options: Vec<String>,
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

impl DesktopComposerSubmitMode {
    fn admission_is_open(self) -> bool {
        !matches!(self, Self::Blocked)
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DesktopRuntimeProjection {
    pub agent_activity_rows: Vec<DesktopAgentActivityRow>,
    pub current_turn_agent_activity_rows: Vec<DesktopAgentActivityRow>,
    pub agent_tree_active: bool,
    pub root_run_finalizing: bool,
    pub root_run_generation: Option<u64>,
    pub last_root_run_epoch: u64,
    pub composer_commit_generation: u64,
}

impl DesktopRuntimeProjection {
    fn root_run_active(&self) -> bool {
        self.root_run_generation.is_some()
    }

    fn blocks_new_request(&self) -> bool {
        self.root_run_active() || self.root_run_finalizing || self.agent_tree_active
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

pub(crate) fn navigation_admission_blocker(
    busy: bool,
    background_mutation_pending: bool,
    navigation_loading: bool,
    agent_tree_active: bool,
    root_run_finalizing: bool,
) -> Option<&'static str> {
    if agent_tree_active {
        Some("the current agent tree is active")
    } else if root_run_finalizing {
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

fn composer_admission_is_open(
    runtime: &DesktopRuntimeProjection,
    busy: bool,
    navigation_loading: bool,
    background_mutation_pending: bool,
) -> bool {
    !busy && !navigation_loading && !background_mutation_pending && !runtime.blocks_new_request()
}

fn composer_steer_admission_is_open(
    runtime: &DesktopRuntimeProjection,
    state_busy: bool,
    navigation_loading: bool,
    background_mutation_pending: bool,
) -> bool {
    state_busy
        && !runtime.root_run_finalizing
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopDraftActionTargetProjection {
    pub workspace_path: String,
    pub session_id: Option<String>,
    pub owner_generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRunMutationTargetProjection {
    pub workspace_path: String,
    pub session_id: Option<String>,
    pub runtime_owner_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopWebState {
    pub projection_revision: String,
    pub workspace_path: String,
    pub provider_label: String,
    pub model_label: String,
    pub access_label: String,
    pub access_target: DesktopAccessModeMutationTargetProjection,
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
    pub busy: bool,
    pub async_polling_required: bool,
    pub pending_async_operations: Vec<String>,
    pub navigation_loading: bool,
    pub navigation_admission_open: bool,
    pub turn_page_admission_open: bool,
    pub post_run_refresh_pending: bool,
    pub background_mutation_pending: bool,
    pub overlay: String,
    pub project_rows: Vec<DesktopProjectRow>,
    pub selected_project_index: i32,
    pub session_rows: Vec<DesktopSessionRow>,
    pub chat_session_rows: Vec<DesktopSessionRow>,
    pub selected_session_index: i32,
    pub session_search_text: String,
    pub session_search_include_archived: bool,
    pub thread_empty: bool,
    pub transcript_rows: Vec<DesktopTranscriptRow>,
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
    pub provider_metadata_mode: String,
    pub provider_catalog_base_url: Option<String>,
    pub provider_catalog_metadata_mode: Option<String>,
    pub provider_context_window: String,
    pub provider_max_output_tokens: String,
    pub provider_models: Vec<String>,
    pub provider_model_ids: Vec<String>,
    pub provider_selected_index: i32,
    pub provider_status: DesktopProviderStatusProjection,
    pub provider_selected_model_summary: Vec<String>,
    pub provider_loading: bool,
    pub provider_apply_enabled: bool,
    pub config_fields: Vec<DesktopConfigFieldProjection>,
    pub config_target: DesktopConfigMutationTargetProjection,
    pub workspace_input: String,
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

pub(crate) fn desktop_web_state_with_permission(
    state: &DesktopState,
    runtime: &DesktopRuntimeProjection,
    pending_permission: Option<(u64, &PermissionRequest)>,
) -> DesktopWebState {
    let state_busy = state.is_busy();
    let root_run_active = runtime.root_run_active();
    let busy = state_busy || root_run_active;
    let pre_admission_active = runtime.pre_admission_active(state_busy);
    let detail = state.selected_detail();
    let config_editor = ConfigEditorState::from_config(&state.provider_config.effective_config);
    let (review_raw_text, review_status_text, send_enhanced_enabled, send_raw_enabled) =
        if let Some(review) = &state.app_state.prompt_review {
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
                status,
                review.phase == PromptReviewPhase::Reviewing,
                review.phase == PromptReviewPhase::Reviewing,
            )
        } else {
            (
                String::new(),
                "プロンプト推敲は開始されていません。".to_string(),
                false,
                false,
            )
        };
    let new_request_admission_open = composer_admission_is_open(
        runtime,
        busy,
        state.navigation_loading(),
        state.background_mutation_pending(),
    );
    let steer_admission_open = composer_steer_admission_is_open(
        runtime,
        state_busy,
        state.navigation_loading(),
        state.background_mutation_pending(),
    );
    let composer_submit_mode = if steer_admission_open {
        DesktopComposerSubmitMode::Steer
    } else if new_request_admission_open {
        DesktopComposerSubmitMode::NewRequest
    } else {
        DesktopComposerSubmitMode::Blocked
    };
    let composer_admission_open = composer_submit_mode.admission_is_open();
    let image_input_enabled =
        desktop_image_input_delegates_capability_to_runtime(state) && composer_admission_open;
    let navigation_admission_open = navigation_admission_blocker(
        busy,
        state.background_mutation_pending(),
        state.navigation_loading(),
        runtime.agent_tree_active,
        runtime.root_run_finalizing,
    )
    .is_none();
    let latest_tool_summary = detail
        .tool_status_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("ツール待機中")
        .to_string();
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
    DesktopWebState {
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
        latest_tool_summary: display_tool_summary(&latest_tool_summary),
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
        confirmation_visible: pending_permission.is_some(),
        confirmation_id: pending_permission.map(|(id, _)| id.to_string()),
        confirmation_text,
        confirmation: pending_permission.map(|(_, permission)| DesktopPermissionProjection {
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
        }),
        startup,
        composer_commit_generation: runtime.composer_commit_generation.to_string(),
        draft_prompt: state.composer.draft_prompt.clone(),
        draft_target: DesktopDraftActionTargetProjection {
            workspace_path: state.snapshot.workspace_path.clone(),
            session_id: state
                .app_state
                .current_session_id
                .map(|session_id| session_id.to_string()),
            owner_generation: state.composer.owner_generation(),
        },
        image_input: state.composer.image_attachment_input.clone(),
        attached_images: state
            .composer
            .image_attachment_paths
            .iter()
            .map(|path| path.to_string())
            .collect(),
        composer_submit_mode,
        can_submit: composer_admission_open,
        can_cancel_run: busy || pending_permission.is_some() || runtime.agent_tree_active,
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
        },
        busy,
        async_polling_required: state.async_polling_required()
            || root_run_active
            || runtime.agent_tree_active
            || runtime.root_run_finalizing,
        pending_async_operations: state.pending_async_operation_keys(),
        navigation_loading: state.navigation_loading(),
        navigation_admission_open,
        turn_page_admission_open: state.can_begin_turn_page_load(),
        post_run_refresh_pending: state.post_run_refresh_pending(),
        background_mutation_pending: state.background_mutation_pending(),
        overlay: overlay_key(state.view.overlay).to_string(),
        project_rows: state.snapshot.project_rows.clone(),
        selected_project_index: state.selected_project_index(),
        session_rows: state.snapshot.session_rows.clone(),
        chat_session_rows: state.snapshot.chat_session_rows.clone(),
        selected_session_index: state.selected_index(),
        session_search_text: state.view.session_search_text.clone(),
        session_search_include_archived: state.view.session_search_include_archived,
        thread_empty: detail.thread_empty,
        transcript_rows: detail.transcript_rows,
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
        provider_metadata_mode: provider_metadata_mode_key(
            state.provider_config.provider_metadata_mode_input,
        )
        .to_string(),
        provider_catalog_base_url: state.provider_config.provider_loaded_base_url.clone(),
        provider_catalog_metadata_mode: state
            .provider_config
            .provider_loaded_base_url
            .as_ref()
            .map(|_| {
                provider_metadata_mode_key(state.provider_config.provider_metadata_mode_input)
                    .to_string()
            }),
        provider_context_window: state.provider_config.provider_context_window_input.clone(),
        provider_max_output_tokens: state
            .provider_config
            .provider_max_output_tokens_input
            .clone(),
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
        config_fields: config_editor
            .fields
            .iter()
            .map(config_field_projection)
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
        review_raw_text,
        review_draft_text: state.composer.review_draft_text.clone(),
        review_status_text,
        send_enhanced_enabled: send_enhanced_enabled && new_request_admission_open,
        send_raw_enabled: send_raw_enabled && new_request_admission_open,
        history_export_enabled: state.can_export_history() && !root_run_active,
        enhance_enabled: new_request_admission_open,
        image_input_enabled,
        window_opacity_percent: state.view.window_opacity_percent,
    }
}

fn config_field_projection(field: &ConfigFieldState) -> DesktopConfigFieldProjection {
    let metadata = config_field_metadata(field.key);
    DesktopConfigFieldProjection {
        key: field.key.label().to_string(),
        value: field.value.clone(),
        env_override: field.key.env_override().map(ToString::to_string),
        value_type: metadata.value_type.to_string(),
        required: metadata.required,
        min_value: metadata.min_value,
        max_value: metadata.max_value,
        options: metadata.options.iter().map(ToString::to_string).collect(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ConfigFieldMetadata {
    value_type: &'static str,
    required: bool,
    min_value: Option<f64>,
    max_value: Option<f64>,
    options: &'static [&'static str],
}

fn config_field_metadata(field: ConfigField) -> ConfigFieldMetadata {
    const NONE: &[&str] = &[];
    const PROVIDER_MODES: &[&str] = &["lm_studio_native_required", "openai_compatible_only"];
    const ACCESS_MODES: &[&str] = &["default", "auto_review", "full_access"];
    const MULTI_AGENT_MODES: &[&str] = &["explicit_request_only", "proactive"];

    let (value_type, min_value, max_value, options) = match field {
        ConfigField::ProviderMetadataMode => ("enum", None, None, PROVIDER_MODES),
        ConfigField::AccessMode => ("enum", None, None, ACCESS_MODES),
        ConfigField::MultiAgentMode => ("enum", None, None, MULTI_AGENT_MODES),
        ConfigField::MultiAgentEnabled
        | ConfigField::SupportsTools
        | ConfigField::SupportsReasoning
        | ConfigField::SupportsImages
        | ConfigField::ParallelToolCalls
        | ConfigField::ShellHideWindows
        | ConfigField::InspectionIncludeHiddenByDefault
        | ConfigField::DoclingEnabled
        | ConfigField::McpEnabled => ("boolean", None, None, NONE),
        ConfigField::Temperature
        | ConfigField::TopP
        | ConfigField::PresencePenalty
        | ConfigField::FrequencyPenalty => ("number", None, None, NONE),
        ConfigField::ExtraHeadersJson
        | ConfigField::ExtraBodyJson
        | ConfigField::DoclingHeadersJson
        | ConfigField::McpServersJson => ("json", None, None, NONE),
        ConfigField::MultiAgentMaxAgents | ConfigField::MultiAgentMaxModelRequests => {
            ("integer", Some(1.0), None, NONE)
        }
        ConfigField::TopK
        | ConfigField::ContextWindow
        | ConfigField::MaxOutputTokens
        | ConfigField::MaxParallelPredictions => {
            ("integer", Some(0.0), Some(u32::MAX as f64), NONE)
        }
        ConfigField::MaxRetries => ("integer", Some(0.0), Some(u8::MAX as f64), NONE),
        ConfigField::InspectionDefaultMaxDepth
        | ConfigField::InspectionDefaultMaxEntriesPerDir
        | ConfigField::InspectionMaxExtensionsReported => ("integer", Some(0.0), None, NONE),
        ConfigField::RequestTimeoutMs
        | ConfigField::StreamIdleTimeoutMs
        | ConfigField::ConnectTimeoutMs
        | ConfigField::FileGuardMaxInlineReadBytes
        | ConfigField::FileGuardLargeFileWarningBytes
        | ConfigField::DoclingTimeoutMs => ("integer", Some(0.0), None, NONE),
        ConfigField::Seed => ("integer", Some(0.0), None, NONE),
        ConfigField::BaseUrl
        | ConfigField::Model
        | ConfigField::StopSequences
        | ConfigField::FileGuardBlockedReadExtensions
        | ConfigField::FileGuardStructuredDocumentExtensions
        | ConfigField::DoclingBaseUrl
        | ConfigField::DoclingApiKeyEnv => ("string", None, None, NONE),
    };
    ConfigFieldMetadata {
        value_type,
        required: false,
        min_value,
        max_value,
        options,
    }
}

pub(crate) fn agent_activity_projection(
    records: Vec<AgentActivityRecord>,
) -> (Vec<DesktopAgentActivityRow>, bool) {
    let mut rows = records
        .into_iter()
        .map(|record| DesktopAgentActivityRow {
            agent_path: record.agent_path,
            session_id: record.session_id.to_string(),
            task_name: record.task_name,
            task_preview: record.task_preview,
            status: agent_status_key(&record.status).to_string(),
            current_activity: record.current_activity,
            result_preview: record.result_preview,
            started_order: record.started_order,
            updated: record.updated,
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
        AgentStatus::Interrupted => "interrupted",
        AgentStatus::Completed(_) => "completed",
        AgentStatus::Errored(_) => "errored",
        AgentStatus::Shutdown => "shutdown",
    }
}

fn desktop_image_input_delegates_capability_to_runtime(state: &DesktopState) -> bool {
    !state.is_busy() && !state.navigation_loading()
}

fn startup_projection(state: &DesktopState) -> DesktopStartupProjection {
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
        DesktopOverlay::FileMenu => "file_menu",
        DesktopOverlay::EditMenu => "edit_menu",
        DesktopOverlay::ViewMenu => "view_menu",
        DesktopOverlay::HelpMenu => "help_menu",
        DesktopOverlay::ProjectMenu => "project_menu",
        DesktopOverlay::ConfigEditor => "config",
        DesktopOverlay::ProviderEditor => "provider",
        DesktopOverlay::WorkspacePicker => "workspace",
        DesktopOverlay::PromptReview => "prompt_review",
        DesktopOverlay::CommandPalette => "command_palette",
        DesktopOverlay::KeyboardShortcuts => "shortcuts",
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

fn provider_metadata_mode_key(mode: ProviderMetadataMode) -> &'static str {
    match mode {
        ProviderMetadataMode::LmStudioNativeRequired => "lm_studio_native_required",
        ProviderMetadataMode::OpenAiCompatibleOnly => "openai_compatible_only",
    }
}

fn provider_status_details(state: &DesktopState) -> String {
    let mode = match state.provider_config.provider_metadata_mode_input {
        ProviderMetadataMode::LmStudioNativeRequired => {
            "Provider mode: LM Studio native metadata required."
        }
        ProviderMetadataMode::OpenAiCompatibleOnly => {
            "Provider mode: OpenAI-compatible model catalog only."
        }
    };
    let limits = format!(
        "Managed request limits: context_window={}, max_output_tokens={}.",
        state.provider_config.provider_context_window_input,
        state.provider_config.provider_max_output_tokens_input
    );
    [
        state.provider_config.provider_status.details.as_str(),
        mode,
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
            "Max output: {}",
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
            "概算 token 使用量: {} / {} ({}%). 出力予約: {}、overflow margin: {}、残り推定: {}。",
            status.active_context_tokens,
            status.full_context_window_limit,
            percent,
            status.configured_max_output_tokens,
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
        DesktopStatusCode::PermissionPolicyDenied => {
            return (
                "操作が許可されませんでした。アクセス設定と対象を確認してください。".to_string(),
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

fn format_permission_confirmation_text(permission: &PermissionRequest) -> String {
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

fn display_tool_summary(summary: &str) -> String {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return "ツール待機中".to_string();
    }
    if trimmed == "No tool activity yet." || trimmed == "Tool activity pending" {
        return "ツール待機中".to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("Running ") {
        return format!("ツール実行中: {}", rest.trim());
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
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

        let (permission, permission_detail) =
            display_status_projection(DesktopStatusCode::PermissionPolicyDenied, message);
        assert!(permission.contains("許可されませんでした"));
        assert_eq!(permission_detail, message);
    }

    #[test]
    fn provider_metadata_mode_status_does_not_claim_a_hidden_system_policy() {
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
        state.provider_config.provider_metadata_mode_input =
            ProviderMetadataMode::OpenAiCompatibleOnly;

        let details = provider_status_details(&state);

        assert!(details.contains("OpenAI-compatible model catalog only"));
        assert!(!details.contains("language"));
        assert!(!details.contains("no-thinking"));
    }

    #[test]
    fn config_field_metadata_matches_rust_parser_shapes_and_bounds() {
        let agents = config_field_metadata(ConfigField::MultiAgentMaxAgents);
        assert_eq!(agents.value_type, "integer");
        assert_eq!(agents.min_value, Some(1.0));

        let context = config_field_metadata(ConfigField::ContextWindow);
        assert_eq!(context.value_type, "integer");
        assert_eq!(context.max_value, Some(u32::MAX as f64));

        let retries = config_field_metadata(ConfigField::MaxRetries);
        assert_eq!(retries.max_value, Some(u8::MAX as f64));

        let temperature = config_field_metadata(ConfigField::Temperature);
        assert_eq!(temperature.value_type, "number");

        let mode = config_field_metadata(ConfigField::MultiAgentMode);
        assert_eq!(mode.value_type, "enum");
        assert_eq!(mode.options, &["explicit_request_only", "proactive"]);

        let access = config_field_metadata(ConfigField::AccessMode);
        assert_eq!(access.value_type, "enum");
        assert_eq!(access.options, &["default", "auto_review", "full_access"]);
        assert_eq!(access_mode_key(AccessMode::AutoReview), "auto_review");
    }

    #[test]
    fn root_finalizing_and_agent_tree_close_the_authoritative_composer_gate() {
        assert!(composer_admission_is_open(
            &DesktopRuntimeProjection::default(),
            false,
            false,
            false,
        ));
        assert!(composer_steer_admission_is_open(
            &DesktopRuntimeProjection {
                root_run_generation: Some(7),
                ..DesktopRuntimeProjection::default()
            },
            true,
            false,
            false,
        ));
        assert!(!composer_steer_admission_is_open(
            &DesktopRuntimeProjection {
                root_run_generation: Some(7),
                root_run_finalizing: true,
                ..DesktopRuntimeProjection::default()
            },
            true,
            false,
            false,
        ));
        assert!(!composer_steer_admission_is_open(
            &DesktopRuntimeProjection {
                root_run_generation: Some(7),
                ..DesktopRuntimeProjection::default()
            },
            true,
            false,
            true,
        ));
        assert!(!composer_admission_is_open(
            &DesktopRuntimeProjection {
                root_run_finalizing: true,
                ..DesktopRuntimeProjection::default()
            },
            false,
            false,
            false,
        ));
        assert!(!composer_admission_is_open(
            &DesktopRuntimeProjection {
                agent_tree_active: true,
                ..DesktopRuntimeProjection::default()
            },
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
        assert!(
            !desktop_web_state(
                &state,
                &DesktopRuntimeProjection {
                    root_run_finalizing: true,
                    ..DesktopRuntimeProjection::default()
                },
            )
            .navigation_admission_open
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
                agent_tree_active: true,
                last_root_run_epoch: 5,
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
                "run and tree ownership must close config commit"
            );
        }
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
        let runtime = DesktopRuntimeProjection {
            root_run_generation: Some(8),
            last_root_run_epoch: 8,
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
            desktop_web_state(
                &state,
                &DesktopRuntimeProjection {
                    agent_tree_active: true,
                    ..DesktopRuntimeProjection::default()
                }
            )
            .can_cancel_run
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
            desktop_web_state_with_permission(
                &state,
                &DesktopRuntimeProjection::default(),
                Some((7, &permission)),
            )
            .can_cancel_run
        );
    }

    #[test]
    fn agent_activity_projection_preserves_contract_and_spawn_order() {
        let completed_session_id = crate::session::SessionId::new();
        let running_session_id = crate::session::SessionId::new();
        let (rows, active) = agent_activity_projection(vec![
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
            },
        ]);

        assert!(active);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].agent_path, "/root/runtime");
        assert_eq!(rows[0].session_id, running_session_id.to_string());
        assert_eq!(rows[0].status, "running");
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
        let (rows, active) = agent_activity_projection(vec![AgentActivityRecord {
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
        }]);

        assert_eq!(rows[0].status, "interrupted");
        assert!(!active);
    }

    #[test]
    fn token_meter_projection_formats_estimated_usage() {
        let status = crate::context::ContextWindowTokenStatus {
            source: crate::context::ActiveContextTokenSource::FullPreparedRequestEstimate,
            active_context_tokens: 12_345,
            full_context_window_limit: 131_072,
            configured_max_output_tokens: 8_192,
            overflow_margin_tokens: 1_024,
            tokens_until_limit: 109_511,
            token_limit_reached: false,
        };

        let projection = token_meter_projection(Some(&status), 131_072);

        assert_eq!(projection.label, "12.3k / 131k 低い");
        assert_eq!(projection.level, "low");
        assert!(projection.title.contains("12345 / 131072"));
    }

    #[test]
    fn token_meter_projection_marks_reached_limit() {
        let status = crate::context::ContextWindowTokenStatus {
            source: crate::context::ActiveContextTokenSource::FullPreparedRequestEstimate,
            active_context_tokens: 130_000,
            full_context_window_limit: 131_072,
            configured_max_output_tokens: 8_192,
            overflow_margin_tokens: 1_024,
            tokens_until_limit: -8_144,
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
