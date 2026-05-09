use slint::{ModelRc, SharedString, StandardListViewItem, VecModel};

use crate::tui::config_editor::ConfigField;
use crate::tui::state::PromptReviewPhase;

use super::state::{DesktopOverlay, DesktopState};

pub struct DesktopViewModel {
    pub workspace_name: SharedString,
    pub workspace_path_text: SharedString,
    pub session_summary_text: SharedString,
    pub selection_title: SharedString,
    pub current_session_title: SharedString,
    pub provider_title: SharedString,
    pub model_title: SharedString,
    pub access_mode_title: SharedString,
    pub run_status_text: SharedString,
    pub progress_text: SharedString,
    pub transcript_text: SharedString,
    pub tool_status_text: SharedString,
    pub status_banner_text: SharedString,
    pub confirmation_text: SharedString,
    pub confirmation_visible: bool,
    pub session_items: ModelRc<StandardListViewItem>,
    pub current_session_index: i32,
    pub composer_text: SharedString,
    pub image_path_text: SharedString,
    pub image_summary_text: SharedString,
    pub image_attach_enabled: bool,
    pub image_clear_enabled: bool,
    pub run_enabled: bool,
    pub review_enabled: bool,
    pub open_session_enabled: bool,
    pub history_export_enabled: bool,
    pub enhance_enabled: bool,
    pub config_visible: bool,
    pub config_items: ModelRc<StandardListViewItem>,
    pub current_config_index: i32,
    pub config_field_title: SharedString,
    pub config_value_text: SharedString,
    pub config_feedback_text: SharedString,
    pub provider_visible: bool,
    pub provider_base_url_text: SharedString,
    pub provider_model_items: ModelRc<SharedString>,
    pub current_provider_model_index: i32,
    pub provider_feedback_text: SharedString,
    pub provider_load_button_text: SharedString,
    pub provider_load_enabled: bool,
    pub provider_apply_enabled: bool,
    pub workspace_picker_visible: bool,
    pub workspace_input_text: SharedString,
    pub review_visible: bool,
    pub review_raw_text: SharedString,
    pub review_draft_text: SharedString,
    pub review_status_text: SharedString,
    pub send_enhanced_enabled: bool,
    pub send_raw_enabled: bool,
    pub window_opacity_percent: f32,
    pub window_opacity_text: SharedString,
}

pub fn build(state: &DesktopState) -> DesktopViewModel {
    let session_items = state
        .snapshot
        .session_rows
        .iter()
        .map(|row| {
            let label = if Some(row.session_id) == state.app_state.current_session_id {
                format!("● {}", row.label)
            } else {
                row.label.clone()
            };
            StandardListViewItem::from(SharedString::from(label))
        })
        .collect::<Vec<_>>();
    let detail = state.selected_detail();
    let workspace_name = state
        .snapshot
        .workspace_path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(state.snapshot.workspace_path.as_str())
        .to_string();
    let session_summary_text = match state.snapshot.session_rows.len() {
        0 => "No sessions".to_string(),
        1 => "1 recent session".to_string(),
        count => format!("{count} recent sessions"),
    };
    let status_banner_text = state
        .app_state
        .status_message
        .clone()
        .unwrap_or_else(|| "Desktop ready.".to_string());
    let config_items = state
        .config_editor
        .fields
        .iter()
        .map(config_item_label)
        .map(|label| StandardListViewItem::from(SharedString::from(label)))
        .collect::<Vec<_>>();
    let provider_model_items = state
        .provider_models
        .iter()
        .map(|label| {
            let display = state
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
                .unwrap_or_else(|| label.clone());
            SharedString::from(display)
        })
        .collect::<Vec<_>>();
    let (review_raw_text, review_status_text, send_enhanced_enabled, send_raw_enabled) =
        if let Some(review) = &state.app_state.prompt_review {
            let status = match review.phase {
                PromptReviewPhase::Enhancing => {
                    "Generating enhanced draft. Cancel keeps the raw prompt.".to_string()
                }
                PromptReviewPhase::Reviewing => {
                    "Edit the draft, then choose Send Enhanced or Send Raw.".to_string()
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
                "Prompt enhancement is not active.".to_string(),
                false,
                false,
            )
        };

    DesktopViewModel {
        workspace_name: SharedString::from(workspace_name),
        workspace_path_text: SharedString::from(state.snapshot.workspace_path.clone()),
        session_summary_text: SharedString::from(session_summary_text),
        selection_title: SharedString::from(state.selected_session_title()),
        current_session_title: SharedString::from(state.current_session_label()),
        provider_title: SharedString::from(state.effective_config.model.base_url.clone()),
        model_title: SharedString::from(state.effective_config.model.model.clone()),
        access_mode_title: SharedString::from(
            state.effective_config.permissions.access_mode.label(),
        ),
        run_status_text: SharedString::from(state.current_run_status_text()),
        progress_text: SharedString::from(detail.progress_text),
        transcript_text: SharedString::from(detail.transcript_text),
        tool_status_text: SharedString::from(detail.tool_status_text),
        status_banner_text: SharedString::from(status_banner_text),
        confirmation_text: SharedString::from(detail.confirmation_text),
        confirmation_visible: detail.confirmation_visible,
        session_items: VecModel::from_slice(&session_items),
        current_session_index: state.selected_index(),
        composer_text: SharedString::from(state.draft_prompt.clone()),
        image_path_text: SharedString::from(state.image_attachment_input.clone()),
        image_summary_text: SharedString::from(state.image_attachment_summary()),
        image_attach_enabled: !state.is_busy() && !state.image_attachment_input.trim().is_empty(),
        image_clear_enabled: !state.is_busy() && !state.image_attachment_paths.is_empty(),
        run_enabled: state.can_submit_prompt(),
        review_enabled: !state.is_busy(),
        open_session_enabled: state.can_open_session(),
        history_export_enabled: state.can_export_history(),
        enhance_enabled: !state.is_busy() && !state.draft_prompt.trim().is_empty(),
        config_visible: state.overlay == DesktopOverlay::ConfigEditor,
        config_items: VecModel::from_slice(&config_items),
        current_config_index: state.config_editor.selected as i32,
        config_field_title: SharedString::from(state.config_editor.selected_field().key.label()),
        config_value_text: SharedString::from(state.config_value_text.clone()),
        config_feedback_text: SharedString::from(
            state
                .config_editor
                .feedback
                .clone()
                .unwrap_or_else(|| config_feedback_text(state.config_editor.selected_field().key)),
        ),
        provider_visible: state.overlay == DesktopOverlay::ProviderEditor,
        provider_base_url_text: SharedString::from(state.provider_base_url_input.clone()),
        provider_model_items: VecModel::from_slice(&provider_model_items),
        current_provider_model_index: state.provider_selected_index,
        provider_feedback_text: SharedString::from(provider_feedback_text(state)),
        provider_load_button_text: SharedString::from(if state.provider_loading {
            "Loading..."
        } else {
            "Load Models"
        }),
        provider_load_enabled: !state.provider_loading,
        provider_apply_enabled: state.can_apply_provider_selection(),
        workspace_picker_visible: state.overlay == DesktopOverlay::WorkspacePicker,
        workspace_input_text: SharedString::from(state.workspace_input.clone()),
        review_visible: state.overlay == DesktopOverlay::PromptReview,
        review_raw_text: SharedString::from(review_raw_text),
        review_draft_text: SharedString::from(state.review_draft_text.clone()),
        review_status_text: SharedString::from(review_status_text),
        send_enhanced_enabled,
        send_raw_enabled,
        window_opacity_percent: state.window_opacity_percent as f32,
        window_opacity_text: SharedString::from(format!("{}%", state.window_opacity_percent)),
    }
}

fn config_item_label(field: &crate::tui::config_editor::ConfigFieldState) -> String {
    let env_badge = field
        .key
        .env_override()
        .filter(|name| std::env::var(name).is_ok())
        .map(|_| " [ENV]")
        .unwrap_or("");
    format!(
        "{} = {}{}",
        field.key.label(),
        truncate_middle(&field.value, 26),
        env_badge
    )
}

fn provider_feedback_text(state: &DesktopState) -> String {
    let mut text = state.provider_status_text.clone();
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

fn config_feedback_text(key: ConfigField) -> String {
    let env_text = key
        .env_override()
        .filter(|name| std::env::var(name).is_ok())
        .unwrap_or("none");
    format!(
        "Blank value means inherit/remove.\nSession apply is memory only.\nEnv override: {env_text}"
    )
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
