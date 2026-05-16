use serde::{Deserialize, Serialize};

use super::models::{
    DesktopArtifactRow, DesktopCommandRow, DesktopFileChangeRow, DesktopProjectRow,
    DesktopSessionRow, DesktopTranscriptRow,
};
use super::state::{DesktopOverlay, DesktopState};
use crate::tui::config_editor::{ConfigField, ConfigFieldState};
use crate::tui::state::{PromptReviewPhase, RunStatus};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopPermissionProjection {
    pub summary: String,
    pub targets: Vec<String>,
    pub outside_workspace: bool,
    pub risks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopWebState {
    pub workspace_path: String,
    pub provider_label: String,
    pub model_label: String,
    pub access_label: String,
    pub current_session_label: String,
    pub selected_session_title: String,
    pub status_message: String,
    pub run_status_key: String,
    pub run_status_text: String,
    pub run_phase: String,
    pub run_active_step: String,
    pub latest_tool_summary: String,
    pub progress_text: String,
    pub tool_status_text: String,
    pub confirmation_visible: bool,
    pub confirmation_text: String,
    pub confirmation: Option<DesktopPermissionProjection>,
    pub draft_prompt: String,
    pub image_input: String,
    pub attached_images: Vec<String>,
    pub can_submit: bool,
    pub busy: bool,
    pub navigation_loading: bool,
    pub overlay: String,
    pub project_rows: Vec<DesktopProjectRow>,
    pub selected_project_index: i32,
    pub session_rows: Vec<DesktopSessionRow>,
    pub chat_session_rows: Vec<DesktopSessionRow>,
    pub selected_session_index: i32,
    pub transcript_rows: Vec<DesktopTranscriptRow>,
    pub artifact_rows: Vec<DesktopArtifactRow>,
    pub selected_artifact_index: i32,
    pub artifact_preview_text: String,
    pub file_change_rows: Vec<DesktopFileChangeRow>,
    pub file_change_summary_text: String,
    pub local_search_text: String,
    pub local_search_results_text: String,
    pub command_rows: Vec<DesktopCommandRow>,
    pub provider_base_url: String,
    pub provider_models: Vec<String>,
    pub provider_selected_index: i32,
    pub provider_status_text: String,
    pub provider_selected_model_summary: Vec<String>,
    pub provider_loading: bool,
    pub provider_apply_enabled: bool,
    pub config_items: Vec<String>,
    pub selected_config_index: i32,
    pub config_field_title: String,
    pub config_value_text: String,
    pub config_feedback_text: String,
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

pub fn desktop_web_state(state: &DesktopState) -> DesktopWebState {
    let detail = state.selected_detail();
    let config_items = state
        .provider_config
        .config_editor
        .fields
        .iter()
        .map(config_item_label)
        .collect::<Vec<_>>();
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
    let image_input_enabled =
        !state.is_busy() && state.provider_config.effective_config.model.supports_images;
    let latest_tool_summary = detail
        .tool_status_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("ツール待機中")
        .to_string();
    DesktopWebState {
        workspace_path: state.snapshot.workspace_path.clone(),
        provider_label: state.snapshot.provider_label.clone(),
        model_label: state.snapshot.model_label.clone(),
        access_label: format!(
            "{:?}",
            state
                .provider_config
                .effective_config
                .permissions
                .access_mode
        )
        .to_lowercase(),
        current_session_label: state.current_session_label(),
        selected_session_title: state.selected_session_title(),
        status_message: state
            .app_state
            .status_message
            .as_deref()
            .map(display_status_message)
            .unwrap_or_else(|| "準備完了".to_string()),
        run_status_key: run_status_key(state.app_state.run_status).to_string(),
        run_status_text: state.current_run_status_text(),
        run_phase: state.app_state.progress.current_phase.clone(),
        run_active_step: state.app_state.progress.active_step.clone(),
        latest_tool_summary,
        progress_text: detail.progress_text,
        tool_status_text: detail.tool_status_text,
        confirmation_visible: detail.confirmation_visible,
        confirmation_text: detail.confirmation_text,
        confirmation: state.app_state.permission.as_ref().map(|permission| {
            DesktopPermissionProjection {
                summary: permission.summary.clone(),
                targets: permission.targets.clone(),
                outside_workspace: permission.outside_workspace,
                risks: permission.risks.clone(),
            }
        }),
        draft_prompt: state.composer.draft_prompt.clone(),
        image_input: state.composer.image_attachment_input.clone(),
        attached_images: state
            .composer
            .image_attachment_paths
            .iter()
            .map(|path| path.to_string())
            .collect(),
        can_submit: state.can_submit_prompt(),
        busy: state.is_busy(),
        navigation_loading: state.navigation_loading(),
        overlay: overlay_key(state.view.overlay).to_string(),
        project_rows: state.snapshot.project_rows.clone(),
        selected_project_index: state.selected_project_index(),
        session_rows: state.snapshot.session_rows.clone(),
        chat_session_rows: state.snapshot.chat_session_rows.clone(),
        selected_session_index: state.selected_index(),
        transcript_rows: detail.transcript_rows,
        artifact_rows: detail.artifacts,
        selected_artifact_index: state.selected_artifact_index(),
        artifact_preview_text: state.selected_artifact_preview_text(),
        file_change_rows: detail.file_changes,
        file_change_summary_text: detail.file_change_summary_text,
        local_search_text: state.view.local_search_text.clone(),
        local_search_results_text: state.local_search_results_text(),
        command_rows: state.snapshot.command_rows.clone(),
        provider_base_url: state.provider_config.provider_base_url_input.clone(),
        provider_models: provider_model_labels(state),
        provider_selected_index: state.provider_config.provider_selected_index,
        provider_status_text: provider_feedback_text(state),
        provider_selected_model_summary: provider_selected_model_summary(state),
        provider_loading: state.provider_config.provider_loading,
        provider_apply_enabled: state.can_apply_provider_selection(),
        config_items,
        selected_config_index: state.provider_config.config_editor.selected as i32,
        config_field_title: state
            .provider_config
            .config_editor
            .selected_field()
            .key
            .label()
            .to_string(),
        config_value_text: state.provider_config.config_value_text.clone(),
        config_feedback_text: state
            .provider_config
            .config_editor
            .feedback
            .clone()
            .unwrap_or_else(|| {
                config_feedback_text(state.provider_config.config_editor.selected_field().key)
            }),
        workspace_input: state.workspace_input.clone(),
        review_raw_text,
        review_draft_text: state.composer.review_draft_text.clone(),
        review_status_text,
        send_enhanced_enabled,
        send_raw_enabled,
        history_export_enabled: state.can_export_history(),
        enhance_enabled: !state.is_busy() && !state.composer.draft_prompt.trim().is_empty(),
        image_input_enabled,
        window_opacity_percent: state.view.window_opacity_percent,
    }
}

fn run_status_key(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Idle => "idle",
        RunStatus::Running => "running",
        RunStatus::Confirming => "confirming",
        RunStatus::Completed => "completed",
        RunStatus::AwaitingUser => "awaiting_user",
        RunStatus::Cancelled => "cancelled",
        RunStatus::Failed => "failed",
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

fn config_item_label(field: &ConfigFieldState) -> String {
    let env_badge = field
        .key
        .env_override()
        .filter(|name| std::env::var(name).is_ok())
        .map(|_| " [ENV]")
        .unwrap_or("");
    format!(
        "{} = {}{}",
        field.key.label(),
        truncate_middle(&field.value, 30),
        env_badge
    )
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

fn provider_feedback_text(state: &DesktopState) -> String {
    let mut text = state.provider_config.provider_status_text.clone();
    if let Some(info) = state.selected_provider_model_info() {
        let summary = super::state::provider_model_summary(info);
        if !summary.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str("Selected: ");
            text.push_str(&summary);
        }
    }
    text
}

fn provider_selected_model_summary(state: &DesktopState) -> Vec<String> {
    let Some(info) = state.selected_provider_model_info() else {
        return vec!["選択中のモデル metadata はまだありません。".to_string()];
    };
    let mut lines = vec![
        format!("Model: {}", info.id),
        format!("Metadata source: {}", info.source),
        format!("Loaded: {}", if info.loaded { "yes" } else { "unknown/no" }),
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
            "Images: {}",
            capability_label(info.supports_images, "supported", "not supported")
        ),
        format!(
            "Tools: {}",
            capability_label(info.supports_tools, "supported", "not supported")
        ),
        format!(
            "Reasoning: {}",
            capability_label(info.supports_reasoning, "supported", "not reported")
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

fn capability_label<'a>(value: Option<bool>, yes: &'a str, no: &'a str) -> &'a str {
    match value {
        Some(true) => yes,
        Some(false) => no,
        None => "unknown",
    }
}

fn config_feedback_text(key: ConfigField) -> String {
    let env_text = key
        .env_override()
        .filter(|name| std::env::var(name).is_ok())
        .unwrap_or("none");
    format!(
        "空欄は継承または削除を意味します。\nセッション適用は現在の起動中だけ有効です。\n環境変数の上書き: {env_text}"
    )
}

fn display_status_message(message: &str) -> String {
    if message == "run completed" {
        "実行完了".to_string()
    } else if let Some(rest) = message.strip_prefix("assistant running on ") {
        format!("実行中: {rest}")
    } else {
        message.to_string()
    }
}

fn truncate_middle(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return value.chars().take(max_chars).collect();
    }
    let head = (max_chars - 1) / 2;
    let tail = max_chars - head - 1;
    let prefix = value.chars().take(head).collect::<String>();
    let suffix = value
        .chars()
        .rev()
        .take(tail)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{prefix}…{suffix}")
}
