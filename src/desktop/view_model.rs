use std::cell::RefCell;
use std::collections::HashMap;

use camino::Utf8Path;
use slint::{Image, ModelRc, SharedString, StandardListViewItem, VecModel};

use crate::tui::config_editor::ConfigField;
use crate::tui::state::PromptReviewPhase;

use super::bridge::{ArtifactListItem, ImageAttachmentPreview, TranscriptRow};
use super::state::{DesktopOverlay, DesktopState};

thread_local! {
    static IMAGE_PREVIEW_CACHE: RefCell<HashMap<String, Image>> = RefCell::new(HashMap::new());
}

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
    pub transcript_items: ModelRc<TranscriptRow>,
    pub tool_status_text: SharedString,
    pub status_banner_text: SharedString,
    pub confirmation_text: SharedString,
    pub confirmation_visible: bool,
    pub project_items: ModelRc<StandardListViewItem>,
    pub current_project_index: i32,
    pub session_items: ModelRc<StandardListViewItem>,
    pub current_session_index: i32,
    pub artifact_items: ModelRc<ArtifactListItem>,
    pub current_artifact_index: i32,
    pub artifact_preview_text: SharedString,
    pub file_change_summary_text: SharedString,
    pub local_search_text: SharedString,
    pub local_search_results_text: SharedString,
    pub command_items: ModelRc<StandardListViewItem>,
    pub file_menu_visible: bool,
    pub edit_menu_visible: bool,
    pub view_menu_visible: bool,
    pub help_menu_visible: bool,
    pub command_palette_visible: bool,
    pub keyboard_shortcuts_visible: bool,
    pub keyboard_shortcuts_text: SharedString,
    pub image_thumbnail_items: ModelRc<ImageAttachmentPreview>,
    pub composer_text: SharedString,
    pub image_path_text: SharedString,
    pub image_summary_text: SharedString,
    pub image_input_enabled: bool,
    pub image_attach_enabled: bool,
    pub image_browse_enabled: bool,
    pub image_clear_enabled: bool,
    pub run_enabled: bool,
    pub review_enabled: bool,
    pub open_session_enabled: bool,
    pub delete_project_enabled: bool,
    pub delete_session_enabled: bool,
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
    let project_items = state
        .snapshot
        .project_rows
        .iter()
        .map(|row| StandardListViewItem::from(SharedString::from(row.label.clone())))
        .collect::<Vec<_>>();
    let session_items = state
        .snapshot
        .session_rows
        .iter()
        .map(|row| StandardListViewItem::from(SharedString::from(row.label.clone())))
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
    let artifact_items = detail
        .artifacts
        .iter()
        .map(|artifact| ArtifactListItem {
            label: SharedString::from(artifact.label.clone()),
            path: SharedString::from(artifact.path.clone()),
            kind: SharedString::from(artifact.kind.clone()),
            action: SharedString::from(artifact.action.clone()),
        })
        .collect::<Vec<_>>();
    let command_items = state
        .snapshot
        .command_rows
        .iter()
        .map(|command| {
            StandardListViewItem::from(SharedString::from(format!(
                "{}  {}",
                command.label, command.path
            )))
        })
        .collect::<Vec<_>>();
    let transcript_items = detail
        .transcript_rows
        .iter()
        .map(|row| TranscriptRow {
            kind: SharedString::from(row.kind.clone()),
            step: SharedString::from(row.step.clone()),
            title: SharedString::from(row.title.clone()),
            body: SharedString::from(row.body.clone()),
        })
        .collect::<Vec<_>>();
    let image_thumbnail_items = state
        .image_attachment_paths
        .iter()
        .map(|path| {
            let label = path
                .file_name()
                .map(str::to_string)
                .unwrap_or_else(|| path.to_string());
            ImageAttachmentPreview {
                label: SharedString::from(label),
                path: SharedString::from(path.to_string()),
                preview: cached_image_preview(path),
            }
        })
        .collect::<Vec<_>>();
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

    let image_input_enabled = !state.is_busy() && state.effective_config.model.supports_images;

    let transcript_text = detail.transcript_text;
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
        transcript_text: SharedString::from(transcript_text),
        transcript_items: VecModel::from_slice(&transcript_items),
        tool_status_text: SharedString::from(detail.tool_status_text),
        status_banner_text: SharedString::from(status_banner_text),
        confirmation_text: SharedString::from(detail.confirmation_text),
        confirmation_visible: detail.confirmation_visible,
        project_items: VecModel::from_slice(&project_items),
        current_project_index: state.selected_project_index(),
        session_items: VecModel::from_slice(&session_items),
        current_session_index: state.selected_index(),
        artifact_items: VecModel::from_slice(&artifact_items),
        current_artifact_index: state.selected_artifact_index(),
        artifact_preview_text: SharedString::from(state.selected_artifact_preview_text()),
        file_change_summary_text: SharedString::from(detail.file_change_summary_text),
        local_search_text: SharedString::from(state.local_search_text.clone()),
        local_search_results_text: SharedString::from(state.local_search_results_text()),
        command_items: VecModel::from_slice(&command_items),
        file_menu_visible: state.overlay == DesktopOverlay::FileMenu,
        edit_menu_visible: state.overlay == DesktopOverlay::EditMenu,
        view_menu_visible: state.overlay == DesktopOverlay::ViewMenu,
        help_menu_visible: state.overlay == DesktopOverlay::HelpMenu,
        command_palette_visible: state.overlay == DesktopOverlay::CommandPalette,
        keyboard_shortcuts_visible: state.overlay == DesktopOverlay::KeyboardShortcuts,
        keyboard_shortcuts_text: SharedString::from(keyboard_shortcuts_text()),
        image_thumbnail_items: VecModel::from_slice(&image_thumbnail_items),
        composer_text: SharedString::from(state.draft_prompt.clone()),
        image_path_text: SharedString::from(state.image_attachment_input.clone()),
        image_summary_text: SharedString::from(state.image_attachment_summary()),
        image_input_enabled,
        image_attach_enabled: image_input_enabled
            && !state.image_attachment_input.trim().is_empty(),
        image_browse_enabled: image_input_enabled,
        image_clear_enabled: !state.is_busy() && !state.image_attachment_paths.is_empty(),
        run_enabled: state.can_submit_prompt(),
        review_enabled: !state.is_busy(),
        open_session_enabled: state.can_open_session(),
        delete_project_enabled: state.can_delete_project(),
        delete_session_enabled: state.can_delete_session(),
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

fn cached_image_preview(path: &Utf8Path) -> Image {
    let key = path.to_string();
    IMAGE_PREVIEW_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(image) = cache.get(&key) {
            return image.clone();
        }
        let image = Image::load_from_path(path.as_std_path()).unwrap_or_default();
        cache.insert(key, image.clone());
        image
    })
}

fn keyboard_shortcuts_text() -> &'static str {
    "Ctrl+N  New prompt focus\nCtrl+K  Command palette\nCtrl+Enter  Send prompt\nCtrl+R  Refresh sessions\nEsc  Close overlay"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResolvedConfig;
    use crate::desktop::models::{DesktopProjectRow, DesktopSessionRow, DesktopSnapshot};
    use crate::desktop::state::DesktopState;
    use crate::session::{ProjectId, SessionId};
    use slint::Model;

    fn empty_state() -> DesktopState {
        DesktopState::new(
            DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: "provider".to_string(),
                model_label: "model".to_string(),
                command_rows: Vec::new(),
                session_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            ResolvedConfig::default(),
        )
    }

    #[test]
    fn desktop_view_model_enables_send_when_prompt_is_ready() {
        let mut state = empty_state();
        assert!(!build(&state).run_enabled);

        state.set_draft_prompt("summarize this repository".to_string());
        let model = build(&state);

        assert_eq!(model.composer_text.as_str(), "summarize this repository");
        assert!(model.run_enabled);
        assert!(model.enhance_enabled);
    }

    #[test]
    fn desktop_view_model_projects_image_attachments_as_thumbnail_rows() {
        let mut state = empty_state();
        state.attach_image_path(camino::Utf8PathBuf::from("C:/workspace/screen.png"));

        let model = build(&state);

        assert_eq!(model.image_thumbnail_items.row_count(), 1);
        let row = model.image_thumbnail_items.row_data(0).unwrap();
        assert_eq!(row.label.as_str(), "screen.png");
        assert_eq!(row.path.as_str(), "C:/workspace/screen.png");

        state.remove_image_attachment(0);
        assert!(state.image_attachment_paths.is_empty());
    }

    #[test]
    fn desktop_view_model_disables_image_attach_for_non_vision_models() {
        let mut state = empty_state();
        state.effective_config.model.supports_images = false;
        state.set_image_attachment_input("C:/workspace/screen.png".to_string());

        let model = build(&state);

        assert!(!model.image_input_enabled);
        assert!(!model.image_attach_enabled);
        assert!(!model.image_browse_enabled);
    }

    #[test]
    fn desktop_sidebar_rows_leave_selection_marker_to_delegate() {
        let project_id = ProjectId::new();
        let session_id = SessionId::new();
        let mut state = empty_state();
        state.snapshot.project_rows = vec![DesktopProjectRow {
            project_id,
            label: "workspace".to_string(),
            path: "C:/workspace".to_string(),
        }];
        state.snapshot.session_rows = vec![DesktopSessionRow {
            session_id,
            label: "calculator [Done] 01KTEST".to_string(),
        }];
        state.app_state.current_session_id = Some(session_id);

        let model = build(&state);

        assert_eq!(
            model.project_items.row_data(0).unwrap().text.as_str(),
            "workspace"
        );
        assert_eq!(
            model.session_items.row_data(0).unwrap().text.as_str(),
            "calculator [Done] 01KTEST"
        );
    }

    #[test]
    fn desktop_view_model_projects_top_menu_visibility() {
        let mut state = empty_state();

        state.show_file_menu();
        let model = build(&state);
        assert!(model.file_menu_visible);
        assert!(!model.edit_menu_visible);

        state.show_view_menu();
        let model = build(&state);
        assert!(model.view_menu_visible);
        assert!(!model.file_menu_visible);
    }

    #[test]
    fn desktop_slint_source_exposes_provider_apply_and_config_field_buttons() {
        let source = include_str!("../../ui/desktop/app-window.slint");

        assert!(source.contains("height: 34px"));
        assert!(source.contains("height: 54px"));
        assert!(source.contains("component MenuButton inherits Rectangle"));
        assert!(source.contains("component MenuItem inherits Rectangle"));
        assert!(source.contains("text: \"File\""));
        assert!(source.contains("text: \"Edit\""));
        assert!(source.contains("text: \"View\""));
        assert!(source.contains("text: \"Help\""));
        assert!(source.contains("root.file_menu_requested()"));
        assert!(source.contains("root.edit_menu_requested()"));
        assert!(source.contains("root.view_menu_requested()"));
        assert!(source.contains("root.help_menu_requested()"));
        assert!(source.contains("root.new_chat_requested()"));
        assert!(source.contains("root.overlay_close_requested(); }"));
        assert!(source.contains("height: 230px"));
        assert!(source.contains("if root.image_clear_enabled : HorizontalBox"));
        assert!(source.contains("enabled: root.image_input_enabled"));
        assert!(source.contains("enabled: root.image_browse_enabled"));
        assert!(source.contains("text <=> root.image_path_text"));
        assert!(source.contains("text <=> root.composer_text"));
        assert!(!source.contains("text: \"Clear Prompt\""));
        assert!(source.contains("text: \"Open Workspace...\""));
        assert!(source.contains("text: \"Open Selected Session\""));
        assert!(source.contains("text: \"Export History\""));
        assert!(source.contains("text: \"Enhance Prompt\""));
        assert!(source.contains("text: \"Prompt Review\""));
        assert!(source.contains("text: \"Cycle Access Preset\""));
        assert!(source.contains("root.provider_editor_requested()"));
        assert!(
            source.contains("icon: @image-url(\"../../logo/fabicon/android-chrome-512x512.png\")")
        );
        assert!(source.contains("root.config_editor_requested()"));
        assert!(source.contains("text: \"Open Project Dir\""));
        assert!(source.contains("root.config_open_project_folder_requested()"));
        assert!(source.contains("text: \"Open Global Dir\""));
        assert!(source.contains("root.config_open_global_folder_requested()"));
        assert!(source.contains("root.session_reload_requested()"));
        assert!(source.contains("root.history_export_requested()"));
        assert!(source.contains("root.artifact_selected(index)"));
        assert!(source.contains("root.artifact_folder_open_requested(index)"));
        assert!(source.contains("root.command_palette_requested()"));
        assert!(source.contains("root.command_selected(index)"));
        assert!(source.contains("root.keyboard_shortcuts_requested()"));
        assert!(!source.contains("root.local_search_changed(text)"));
        assert!(!source.contains("Type to search projects"));
        assert!(source.contains("vertical-scrollbar-policy: ScrollBarPolicy.always-off"));
        assert!(source.contains("component TranscriptCard inherits Rectangle"));
        assert!(source.contains("for item[index] in root.transcript_items : TranscriptCard"));
        assert!(!source.contains("viewport-y: root.transcript_viewport_y * 1px"));
        assert!(source.contains("default-font-family: \"Yu Gothic UI\""));
        assert!(source.contains("font-family: \"Yu Gothic UI\""));
        assert!(source.contains("for attachment[index] in root.image_thumbnail_items"));
        assert!(source.contains("export struct ImageAttachmentPreview"));
        assert!(source.contains("source: attachment.preview"));
        assert!(source.contains("root.image_remove_requested(index)"));
        assert!(source.contains("text: \"アーティファクト\""));
        assert!(!source.contains("text: \"File Changes\""));
        assert!(!source.contains("text: \"Progress\""));
        assert!(source.contains("component ActionButton inherits Rectangle"));
        assert!(source.contains("text: \"Browse Folder...\""));
        assert!(source.contains("text: \"Cancel\""));
        assert!(source.contains("primary: true;\n                            clicked => { root.workspace_apply_requested(); }"));
        assert!(source.contains("clicked => { root.config_close_requested(); }"));
        assert!(source.contains("primary: true;\n                            clicked => { root.config_apply_session_requested(); }"));
        assert!(source.contains("text: \"OK - Apply Session\""));
        assert!(source.contains("clicked => { root.provider_close_requested(); }"));
        assert!(source.contains(
            "primary: true;\n                            enabled: root.provider_apply_enabled;"
        ));
        assert!(source.contains("clicked => { root.cancel_review_requested(); }"));
        assert!(source.contains(
            "primary: true;\n                            enabled: root.send_enhanced_enabled;"
        ));
        assert!(source.contains("clicked => { root.confirm_reject_requested(); }"));
        assert!(source.contains("primary: true;\n                            clicked => { root.confirm_accept_requested(); }"));
        assert!(source.contains("for item[index] in root.config_items : Button"));
        assert!(source.contains("root.current_config_index = index;"));
        assert!(source.contains("root.config_selected(index)"));
    }
}
