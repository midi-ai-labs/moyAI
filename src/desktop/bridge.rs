use slint::{ComponentHandle, PlatformError, Weak};

use super::state::DesktopState;
use super::view_model;

slint::include_modules!();

pub struct DesktopBridge {
    ui: AppWindow,
}

impl DesktopBridge {
    pub fn new() -> Result<Self, PlatformError> {
        Ok(Self {
            ui: AppWindow::new()?,
        })
    }

    pub fn render(&self, state: &DesktopState) {
        render_handle(&self.ui, state);
    }

    pub fn on_session_selected<F>(&self, handler: F)
    where
        F: Fn(i32) + 'static,
    {
        self.ui.on_session_selected(handler);
    }

    pub fn on_composer_changed<F>(&self, handler: F)
    where
        F: Fn(slint::SharedString) + 'static,
    {
        self.ui.on_composer_changed(handler);
    }

    pub fn on_image_path_changed<F>(&self, handler: F)
    where
        F: Fn(slint::SharedString) + 'static,
    {
        self.ui.on_image_path_changed(handler);
    }

    pub fn on_image_attach_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_image_attach_requested(handler);
    }

    pub fn on_image_browse_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_image_browse_requested(handler);
    }

    pub fn on_image_clear_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_image_clear_requested(handler);
    }

    pub fn on_refresh_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_refresh_requested(handler);
    }

    pub fn on_session_reload_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_session_reload_requested(handler);
    }

    pub fn on_history_export_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_history_export_requested(handler);
    }

    pub fn on_run_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_run_requested(handler);
    }

    pub fn on_review_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_review_requested(handler);
    }

    pub fn on_enhance_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_enhance_requested(handler);
    }

    pub fn on_open_folder_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_open_folder_requested(handler);
    }

    pub fn on_config_editor_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_config_editor_requested(handler);
    }

    pub fn on_provider_editor_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_provider_editor_requested(handler);
    }

    pub fn on_access_mode_toggle_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_access_mode_toggle_requested(handler);
    }

    pub fn on_provider_close_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_provider_close_requested(handler);
    }

    pub fn on_provider_base_url_changed<F>(&self, handler: F)
    where
        F: Fn(slint::SharedString) + 'static,
    {
        self.ui.on_provider_base_url_changed(handler);
    }

    pub fn on_provider_load_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_provider_load_requested(handler);
    }

    pub fn on_provider_model_selected<F>(&self, handler: F)
    where
        F: Fn(slint::SharedString) + 'static,
    {
        self.ui.on_provider_model_selected(handler);
    }

    pub fn on_provider_apply_session_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_provider_apply_session_requested(handler);
    }

    pub fn on_provider_save_project_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_provider_save_project_requested(handler);
    }

    pub fn on_provider_save_global_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_provider_save_global_requested(handler);
    }

    pub fn on_config_close_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_config_close_requested(handler);
    }

    pub fn on_config_selected<F>(&self, handler: F)
    where
        F: Fn(i32) + 'static,
    {
        self.ui.on_config_selected(handler);
    }

    pub fn on_config_value_changed<F>(&self, handler: F)
    where
        F: Fn(slint::SharedString) + 'static,
    {
        self.ui.on_config_value_changed(handler);
    }

    pub fn on_config_apply_session_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_config_apply_session_requested(handler);
    }

    pub fn on_config_save_project_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_config_save_project_requested(handler);
    }

    pub fn on_config_save_global_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_config_save_global_requested(handler);
    }

    pub fn on_workspace_picker_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_workspace_picker_requested(handler);
    }

    pub fn on_workspace_close_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_workspace_close_requested(handler);
    }

    pub fn on_workspace_input_changed<F>(&self, handler: F)
    where
        F: Fn(slint::SharedString) + 'static,
    {
        self.ui.on_workspace_input_changed(handler);
    }

    pub fn on_workspace_apply_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_workspace_apply_requested(handler);
    }

    pub fn on_workspace_browse_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_workspace_browse_requested(handler);
    }

    pub fn on_open_typed_path_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_open_typed_path_requested(handler);
    }

    pub fn on_review_draft_changed<F>(&self, handler: F)
    where
        F: Fn(slint::SharedString) + 'static,
    {
        self.ui.on_review_draft_changed(handler);
    }

    pub fn on_send_enhanced_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_send_enhanced_requested(handler);
    }

    pub fn on_send_raw_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_send_raw_requested(handler);
    }

    pub fn on_cancel_review_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_cancel_review_requested(handler);
    }

    pub fn on_confirm_accept_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_confirm_accept_requested(handler);
    }

    pub fn on_confirm_reject_requested<F>(&self, handler: F)
    where
        F: Fn() + 'static,
    {
        self.ui.on_confirm_reject_requested(handler);
    }

    pub fn on_window_opacity_changed<F>(&self, handler: F)
    where
        F: Fn(f32) + 'static,
    {
        self.ui.on_window_opacity_changed(handler);
    }

    pub fn as_weak(&self) -> Weak<AppWindow> {
        self.ui.as_weak()
    }

    pub fn run(self) -> Result<(), PlatformError> {
        self.ui.run()
    }
}

pub fn render_handle(handle: &AppWindow, state: &DesktopState) {
    let model = view_model::build(state);
    handle.set_workspace_name(model.workspace_name);
    handle.set_workspace_path_text(model.workspace_path_text);
    handle.set_session_summary_text(model.session_summary_text);
    handle.set_selection_title(model.selection_title);
    handle.set_current_session_title(model.current_session_title);
    handle.set_provider_title(model.provider_title);
    handle.set_model_title(model.model_title);
    handle.set_access_mode_title(model.access_mode_title);
    handle.set_run_status_text(model.run_status_text);
    handle.set_progress_text(model.progress_text);
    handle.set_transcript_text(model.transcript_text);
    handle.set_tool_status_text(model.tool_status_text);
    handle.set_status_banner_text(model.status_banner_text);
    handle.set_confirmation_text(model.confirmation_text);
    handle.set_confirmation_visible(model.confirmation_visible);
    handle.set_session_items(model.session_items);
    handle.set_current_session_index(model.current_session_index);
    handle.set_composer_text(model.composer_text);
    handle.set_image_path_text(model.image_path_text);
    handle.set_image_summary_text(model.image_summary_text);
    handle.set_image_attach_enabled(model.image_attach_enabled);
    handle.set_image_clear_enabled(model.image_clear_enabled);
    handle.set_run_enabled(model.run_enabled);
    handle.set_review_enabled(model.review_enabled);
    handle.set_open_session_enabled(model.open_session_enabled);
    handle.set_history_export_enabled(model.history_export_enabled);
    handle.set_enhance_enabled(model.enhance_enabled);
    handle.set_config_visible(model.config_visible);
    handle.set_config_items(model.config_items);
    handle.set_current_config_index(model.current_config_index);
    handle.set_config_field_title(model.config_field_title);
    handle.set_config_value_text(model.config_value_text);
    handle.set_config_feedback_text(model.config_feedback_text);
    handle.set_provider_visible(model.provider_visible);
    handle.set_provider_base_url_text(model.provider_base_url_text);
    handle.set_provider_model_items(model.provider_model_items);
    handle.set_current_provider_model_index(model.current_provider_model_index);
    handle.set_provider_feedback_text(model.provider_feedback_text);
    handle.set_provider_load_button_text(model.provider_load_button_text);
    handle.set_provider_load_enabled(model.provider_load_enabled);
    handle.set_provider_apply_enabled(model.provider_apply_enabled);
    handle.set_workspace_picker_visible(model.workspace_picker_visible);
    handle.set_workspace_input_text(model.workspace_input_text);
    handle.set_review_visible(model.review_visible);
    handle.set_review_raw_text(model.review_raw_text);
    handle.set_review_draft_text(model.review_draft_text);
    handle.set_review_status_text(model.review_status_text);
    handle.set_send_enhanced_enabled(model.send_enhanced_enabled);
    handle.set_send_raw_enabled(model.send_raw_enabled);
    handle.set_window_opacity_percent(model.window_opacity_percent);
    handle.set_window_opacity_text(model.window_opacity_text);
}
