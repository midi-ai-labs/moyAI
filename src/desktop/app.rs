use std::cell::RefCell;
use std::process::Command as ProcessCommand;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use slint::{Timer, TimerMode};

use crate::app::{App, AppBootstrap, AppCommand, ReviewRequest, RunRequest};
use crate::cli::{ConfirmationPrompt, EventRenderer, OutputMode};
use crate::config::loader::{global_config_path, project_config_paths};
use crate::config::merge::apply_patch as apply_config_patch;
use crate::config::model::{PartialModelConfig, PartialPermissionsConfig, PartialResolvedConfig};
use crate::config::{ConfigLoader, ResolvedConfig, ShellFamily};
use crate::error::{AppRunError, CliPromptError, CliRenderError};
use crate::llm::{
    ProviderModelInfo, apply_provider_model_info_to_config, fetch_provider_model_infos,
    normalize_provider_base_url,
};
use crate::runtime::SystemClock;
use crate::session::{
    EditorContext, ProjectId, ProjectRecord, RunEvent, RunSummary, SessionId, SessionRecord,
    TodoItem, history_items_to_markdown, history_markdown_file_name,
};
use crate::tool::PermissionRequest;
use crate::workspace::project::normalize_path;

use super::args::DesktopArgs;
use super::bridge::{DesktopBridge, render_composer_action_state, render_handle};
use super::preferences::DesktopPreferences;
use super::query::{
    load_session_detail, load_snapshot, load_snapshot_continue_last, load_snapshot_for_selection,
};
use super::state::DesktopState;

pub async fn run(app: App, args: DesktopArgs) -> Result<(), AppRunError> {
    let controller = Rc::new(RefCell::new(DesktopController::new(app, args).await?));
    let bridge = DesktopBridge::new().map_err(|error| {
        AppRunError::Message(format!("desktop ui initialization failed: {error}"))
    })?;

    bridge.render(&controller.borrow().state);

    bind_handlers(&bridge, &controller);

    let timer = Timer::default();
    {
        let controller = Rc::clone(&controller);
        let weak = bridge.as_weak();
        timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
            let mut controller = controller.borrow_mut();
            let changed = controller.drain_runtime_messages();
            if changed && let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        });
    }

    bridge
        .run()
        .map_err(|error| AppRunError::Message(format!("desktop ui runtime failed: {error}")))?;
    drop(timer);
    Ok(())
}

fn bind_handlers(bridge: &DesktopBridge, controller: &Rc<RefCell<DesktopController>>) {
    bridge.on_project_selected({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |index| {
            if index < 0 {
                return;
            }
            let mut controller = controller.borrow_mut();
            controller.state.select_project(index as usize);
            controller.open_selected_project();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_session_selected({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |index| {
            if index < 0 {
                return;
            }
            let mut controller = controller.borrow_mut();
            controller.state.select_session(index as usize);
            controller.open_selected_session();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_project_delete_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |index| {
            if index < 0 {
                return;
            }
            let mut controller = controller.borrow_mut();
            controller.state.select_project(index as usize);
            controller.delete_selected_project();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_session_delete_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |index| {
            if index < 0 {
                return;
            }
            let mut controller = controller.borrow_mut();
            controller.state.select_session(index as usize);
            controller.delete_selected_session();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_artifact_selected({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |index| {
            if index < 0 {
                return;
            }
            let mut controller = controller.borrow_mut();
            controller.state.select_artifact(index as usize);
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_artifact_folder_open_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |index| {
            if index < 0 {
                return;
            }
            let mut controller = controller.borrow_mut();
            controller.state.select_artifact(index as usize);
            controller.open_selected_artifact_folder();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_local_search_changed({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |text| {
            let mut controller = controller.borrow_mut();
            controller.state.set_local_search_text(text.to_string());
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_command_palette_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.show_command_palette();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_file_menu_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.show_file_menu();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_edit_menu_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.show_edit_menu();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_view_menu_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.show_view_menu();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_help_menu_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.show_help_menu();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_new_chat_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.start_new_chat();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_command_selected({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |index| {
            if index < 0 {
                return;
            }
            let mut controller = controller.borrow_mut();
            controller.state.insert_command_from_palette(index as usize);
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_keyboard_shortcuts_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.show_keyboard_shortcuts();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_overlay_close_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.hide_overlay();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_composer_changed({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |text| {
            let mut controller = controller.borrow_mut();
            controller.state.set_draft_prompt(text.to_string());
            if let Some(handle) = weak.upgrade() {
                render_composer_action_state(&handle, &controller.state);
            }
        }
    });
    bridge.on_image_path_changed({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |text| {
            let mut controller = controller.borrow_mut();
            controller
                .state
                .set_image_attachment_input(text.to_string());
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_image_attach_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.attach_image_from_input();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_image_browse_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.browse_image_dialog();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_image_clear_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.clear_image_attachments();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_image_remove_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |index| {
            if index < 0 {
                return;
            }
            let mut controller = controller.borrow_mut();
            controller.state.remove_image_attachment(index as usize);
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_refresh_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.refresh_snapshot();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_session_reload_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.open_selected_session();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_history_export_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.export_selected_history_markdown();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_run_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.start_run();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_review_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.start_review_uncommitted();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_enhance_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.start_prompt_enhance();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_open_folder_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.open_current_workspace_in_file_manager();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_config_editor_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.show_config_editor();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_provider_editor_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.show_provider_editor();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_access_mode_toggle_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.toggle_access_mode_session();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_config_close_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.hide_overlay();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_provider_close_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.hide_overlay();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_provider_base_url_changed({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |text| {
            let mut controller = controller.borrow_mut();
            controller
                .state
                .set_provider_base_url_input(text.to_string());
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_provider_load_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.load_provider_models();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_provider_model_selected({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |value| {
            let mut controller = controller.borrow_mut();
            controller.state.set_provider_model_value(value.as_str());
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_provider_apply_session_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.apply_provider_session();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_provider_save_project_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.save_provider_project();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_provider_save_global_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.save_provider_global();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_config_selected({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |index| {
            if index < 0 {
                return;
            }
            let mut controller = controller.borrow_mut();
            controller.state.set_config_selection(index as usize);
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_config_value_changed({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |text| {
            let mut controller = controller.borrow_mut();
            controller.state.set_config_value(text.to_string());
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_config_apply_session_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.apply_session_config();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_config_save_project_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.save_project_config();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_config_save_global_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.save_global_config();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_config_open_project_folder_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.open_project_config_folder();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_config_open_global_folder_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.open_global_config_folder();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_workspace_picker_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            let current = controller.app.workspace.cwd.to_string();
            controller.state.show_workspace_picker(&current);
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_workspace_close_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.hide_overlay();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_workspace_input_changed({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |text| {
            let mut controller = controller.borrow_mut();
            controller.state.set_workspace_input(text.to_string());
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_workspace_apply_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.switch_workspace();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_workspace_browse_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.browse_workspace_dialog();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_open_typed_path_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.open_typed_path_in_file_manager();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_review_draft_changed({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |text| {
            let mut controller = controller.borrow_mut();
            controller.state.set_review_draft(text.to_string());
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_send_enhanced_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.send_prompt_review(true);
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_send_raw_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.send_prompt_review(false);
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_cancel_review_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.state.cancel_prompt_review();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_confirm_accept_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.answer_permission(true);
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_confirm_reject_requested({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move || {
            let mut controller = controller.borrow_mut();
            controller.answer_permission(false);
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
    bridge.on_window_opacity_changed({
        let weak = bridge.as_weak();
        let controller = Rc::clone(controller);
        move |value| {
            let mut controller = controller.borrow_mut();
            controller
                .state
                .set_window_opacity_percent(value.round() as i32);
            controller.preferences.window_opacity_percent =
                Some(controller.state.window_opacity_percent);
            controller.persist_preferences();
            if let Some(handle) = weak.upgrade() {
                render_handle(&handle, &controller.state);
            }
        }
    });
}

enum RuntimeMessage {
    RunEvent(RunEvent),
    Finished(Result<RunSummary, String>),
    Permission(PermissionRequest, mpsc::Sender<bool>),
    EnhanceFinished {
        request_id: u64,
        result: Result<String, String>,
    },
    SnapshotLoaded(Result<super::models::DesktopSnapshot, String>),
    SessionLoaded {
        session_id: SessionId,
        reason: SessionLoadReason,
        result: Result<LoadedSession, String>,
    },
    SessionDeleted {
        session_id: SessionId,
        result: Result<super::models::DesktopSnapshot, String>,
    },
    ProjectDeleted {
        project_id: ProjectId,
        project_root: Utf8PathBuf,
        result: Result<WorkspaceLoadResult, String>,
    },
    CurrentTodosLoaded {
        session_id: SessionId,
        result: Result<Vec<TodoItem>, String>,
    },
    ModelCatalogLoaded {
        requested_base_url: String,
        result: Result<Vec<ProviderModelInfo>, String>,
    },
    HistoryExported(Result<Utf8PathBuf, String>),
    WorkspaceSwitched(Result<WorkspaceLoadResult, String>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionLoadReason {
    UserSelection,
    CurrentRefresh,
}

struct LoadedSession {
    session: crate::session::SessionRecord,
    transcript: crate::session::Transcript,
    turn_items: Vec<crate::protocol::TurnItem>,
    state: crate::session::SessionStateSnapshot,
    todos: Vec<TodoItem>,
}

#[derive(Clone)]
struct WorkspaceLoadResult {
    app: App,
    snapshot: super::models::DesktopSnapshot,
}

struct DesktopController {
    app: App,
    state: DesktopState,
    preferences: DesktopPreferences,
    persist_preferences_to_disk: bool,
    runtime_tx: tokio::sync::mpsc::UnboundedSender<RuntimeMessage>,
    runtime_rx: tokio::sync::mpsc::UnboundedReceiver<RuntimeMessage>,
    permission_response: Option<mpsc::Sender<bool>>,
    next_enhance_request_id: u64,
}

impl DesktopController {
    async fn new(app: App, args: DesktopArgs) -> Result<Self, AppRunError> {
        Self::new_with_preferences_and_persistence(
            app,
            args,
            DesktopPreferences::load_or_default(),
            true,
        )
        .await
    }

    async fn new_with_preferences_and_persistence(
        mut app: App,
        args: DesktopArgs,
        mut preferences: DesktopPreferences,
        persist_preferences_to_disk: bool,
    ) -> Result<Self, AppRunError> {
        let (runtime_tx, runtime_rx) = tokio::sync::mpsc::unbounded_channel();
        if args.directory.is_some() {
            preferences.unmark_project_deleted(&app.workspace.root);
        } else {
            purge_deleted_project_roots(&app, &preferences)
                .await
                .map_err(AppRunError::Message)?;
            if preferences.is_project_deleted(&app.workspace.root) {
                let store = app.session_service.store.clone();
                let next_root = next_project_root_after_delete(
                    &app,
                    app.workspace.project_id,
                    &preferences.deleted_project_roots,
                    &app.workspace.root,
                )
                .await
                .map_err(AppRunError::Message)?
                .unwrap_or_else(|| {
                    fallback_workspace_after_project_delete(
                        &app.workspace.root,
                        &preferences.deleted_project_roots,
                        app.session_service.store.paths().data_dir.as_path(),
                    )
                });
                std::fs::create_dir_all(next_root.as_std_path()).map_err(|error| {
                    AppRunError::Message(format!(
                        "failed to create fallback workspace {} after deleted project restore: {error}",
                        next_root
                    ))
                })?;
                app = AppBootstrap::rebuild_for_directory(&next_root, store)
                    .await
                    .map_err(|error| {
                        AppRunError::Message(format!(
                            "failed to open fallback workspace {} after deleted project restore: {error}",
                            next_root
                        ))
                    })?;
            }
        }
        if args.directory.is_none() {
            if let Some(restored_workspace) = preferences.last_workspace.clone() {
                if preferences.is_project_deleted(&restored_workspace) {
                    preferences.last_workspace = None;
                } else if restored_workspace != app.workspace.root {
                    let store = app.session_service.store.clone();
                    if restored_workspace.exists()
                        && project_root_exists(&app, &restored_workspace).await?
                    {
                        app = AppBootstrap::rebuild_for_directory(&restored_workspace, store)
                            .await
                            .map_err(|error| {
                                AppRunError::Message(format!(
                                    "failed to restore last workspace {}: {error}",
                                    restored_workspace
                                ))
                            })?;
                    } else {
                        preferences.last_workspace = None;
                    }
                }
            }
        }
        if let Some(session_id) = args.session_id {
            let session = app.session_service.get_session(session_id).await?;
            if session.cwd != app.workspace.cwd {
                let store = app.session_service.store.clone();
                app = AppBootstrap::rebuild_for_directory(&session.cwd, store)
                    .await
                    .map_err(|error| {
                        AppRunError::Message(format!(
                            "failed to open session workspace {}: {error}",
                            session.cwd
                        ))
                    })?;
            }
        }

        let snapshot = if args.continue_last {
            load_snapshot_continue_last(&app).await?
        } else {
            load_snapshot(&app, &args).await?
        };
        let effective_config =
            apply_preferences_override(&preferences, &app.workspace.root, app.config.clone());
        let mut state = DesktopState::new(snapshot, effective_config);
        state.workspace_input = app.workspace.cwd.to_string();
        if let Some(opacity) = preferences.window_opacity_percent {
            state.set_window_opacity_percent(opacity);
        }
        if let Some(session_id) = args.session_id.or_else(|| state.selected_session_id()) {
            let (session, transcript, turn_items, session_state, todos) =
                load_session_detail(&app, session_id).await?;
            state.load_open_session(&session, &transcript, &turn_items, session_state, todos);
        }
        preferences.last_workspace = Some(app.workspace.root.clone());
        let mut controller = Self {
            app,
            state,
            preferences,
            persist_preferences_to_disk,
            runtime_tx,
            runtime_rx,
            permission_response: None,
            next_enhance_request_id: 1,
        };
        controller.persist_preferences();
        if !controller.state.provider_base_url_input.trim().is_empty() {
            controller.load_provider_models();
        }
        Ok(controller)
    }

    fn refresh_snapshot(&mut self) {
        let app = self.app.clone();
        let selected_session_id = self.state.selected_session_id();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop refresh runtime");
            let result = runtime.block_on(async move {
                load_snapshot_for_selection(&app, selected_session_id)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::SnapshotLoaded(result));
        });
    }

    fn open_selected_session(&mut self) {
        if let Some(session_id) = self.state.selected_session_id() {
            self.state
                .set_status_message(format!("opening session {session_id}..."));
            self.spawn_session_load(session_id, SessionLoadReason::UserSelection);
        }
    }

    fn open_selected_project(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("project cannot change while a run is active");
            return;
        }
        let Some(path) = self.state.selected_project_path().map(Utf8PathBuf::from) else {
            self.state.set_status_message("select a project first");
            return;
        };
        if path == self.app.workspace.root {
            return;
        }
        self.state
            .set_status_message(format!("opening project {}...", path));
        self.spawn_workspace_load(path);
    }

    fn delete_selected_session(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("chat cannot be deleted while a run is active");
            return;
        }
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a chat before deleting");
            return;
        };
        self.state
            .set_status_message(format!("deleting chat {}...", session_id));
        self.spawn_session_delete(session_id);
    }

    fn delete_selected_project(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("project cannot be deleted while a run is active");
            return;
        }
        let Some(project_id) = self.state.selected_project_id() else {
            self.state
                .set_status_message("select a project before deleting");
            return;
        };
        let Some(project_root) = self.state.selected_project_path().map(Utf8PathBuf::from) else {
            self.state
                .set_status_message("selected project path is not available");
            return;
        };
        self.state
            .set_status_message(format!("deleting project {}...", project_id));
        let mut hidden_roots = self.preferences.deleted_project_roots.clone();
        if !hidden_roots.iter().any(|root| root == &project_root) {
            hidden_roots.push(project_root.clone());
        }
        self.spawn_project_delete(project_id, project_root, hidden_roots);
    }

    fn export_selected_history_markdown(&mut self) {
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a session before exporting history");
            return;
        };
        let default_file_name =
            history_markdown_file_name(&self.state.selected_session_title(), session_id);
        match pick_history_markdown_path(&default_file_name) {
            Ok(Some(path)) => self.export_selected_history_markdown_to_path(path),
            Ok(None) => self
                .state
                .set_status_message("history markdown export cancelled"),
            Err(error) => self.state.set_status_message(error),
        }
    }

    fn export_selected_history_markdown_to_path(&mut self, path: Utf8PathBuf) {
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a session before exporting history");
            return;
        };
        self.state
            .set_status_message("exporting history markdown...");
        self.spawn_history_markdown_export(session_id, normalize_markdown_export_path(path));
    }

    fn spawn_history_markdown_export(&self, session_id: SessionId, export_path: Utf8PathBuf) {
        let service = self.app.session_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop history-export runtime");
            let path = export_path.clone();
            let result = runtime
                .block_on(async move {
                    let session = service.get_session(session_id).await?;
                    let history_items = service.canonical_history_items(session_id).await?;
                    if history_items.is_empty() {
                        return Err(crate::error::SessionError::Message(
                            "canonical protocol history is empty".to_string(),
                        ));
                    }
                    Ok::<_, crate::error::SessionError>((session, history_items))
                })
                .map_err(|error| error.to_string())
                .and_then(|(session, history_items)| {
                    if let Some(parent) = export_path.parent() {
                        std::fs::create_dir_all(parent.as_std_path())
                            .map_err(|error| error.to_string())?;
                    }
                    let markdown = history_items_to_markdown(&session, &history_items);
                    std::fs::write(export_path.as_std_path(), markdown)
                        .map_err(|error| error.to_string())?;
                    Ok(path)
                });
            let _ = runtime_tx.send(RuntimeMessage::HistoryExported(result));
        });
    }

    fn spawn_session_load(&self, session_id: SessionId, reason: SessionLoadReason) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session runtime");
            let result = runtime.block_on(async move {
                load_session_detail(&app, session_id)
                    .await
                    .map(
                        |(session, transcript, turn_items, state, todos)| LoadedSession {
                            session,
                            transcript,
                            turn_items,
                            state,
                            todos,
                        },
                    )
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionLoaded {
                session_id,
                reason,
                result,
            });
        });
    }

    fn spawn_session_delete(&self, session_id: SessionId) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-delete runtime");
            let result = runtime.block_on(async move {
                app.session_service
                    .delete_session(session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                load_snapshot_for_selection(&app, None)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionDeleted { session_id, result });
        });
    }

    fn spawn_project_delete(
        &self,
        project_id: ProjectId,
        project_root: Utf8PathBuf,
        hidden_roots: Vec<Utf8PathBuf>,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        let project_root_for_thread = project_root.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop project-delete runtime");
            let result = runtime.block_on(async move {
                let deleted_was_current = project_id == app.workspace.project_id;
                app.session_service
                    .delete_project(project_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let mut app = app;
                if deleted_was_current {
                    let remaining = app
                        .session_service
                        .list_projects(30)
                        .await
                        .map_err(|error| error.to_string())?;
                    let next_root = first_restorable_project_root(
                        &remaining,
                        project_id,
                        &hidden_roots,
                        &project_root_for_thread,
                    )
                    .unwrap_or_else(|| {
                        fallback_workspace_after_project_delete(
                            &project_root_for_thread,
                            &hidden_roots,
                            app.session_service.store.paths().data_dir.as_path(),
                        )
                    });
                    if let Some(parent) = next_root.parent() {
                        std::fs::create_dir_all(parent.as_std_path())
                            .map_err(|error| error.to_string())?;
                    }
                    std::fs::create_dir_all(next_root.as_std_path())
                        .map_err(|error| error.to_string())?;
                    let store = app.session_service.store.clone();
                    app = AppBootstrap::rebuild_for_directory(&next_root, store)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                let snapshot = load_snapshot_for_selection(&app, None)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(WorkspaceLoadResult { app, snapshot })
            });
            let _ = runtime_tx.send(RuntimeMessage::ProjectDeleted {
                project_id,
                project_root,
                result,
            });
        });
    }

    fn spawn_current_todos_refresh(&self, session_id: SessionId) {
        let service = self.app.session_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop todo runtime");
            let result = runtime.block_on(async move {
                service
                    .list_todos(session_id)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::CurrentTodosLoaded { session_id, result });
        });
    }

    fn start_run(&mut self) {
        let prompt = self.state.draft_prompt.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        let prompt_dispatch = crate::session::PromptDispatchPart::raw(&prompt);
        self.launch_run_with_options(prompt, prompt_dispatch, None);
    }

    fn start_review_uncommitted(&mut self) {
        let prompt = self.state.draft_prompt.trim().to_string();
        let prompt_dispatch = crate::session::PromptDispatchPart::raw(&prompt);
        self.launch_run_with_options(prompt, prompt_dispatch, Some(ReviewRequest::Uncommitted));
    }

    fn start_prompt_enhance(&mut self) {
        let raw_prompt = self.state.draft_prompt.trim().to_string();
        if raw_prompt.is_empty() || self.state.is_busy() {
            return;
        }
        let request_id = self.next_enhance_request_id;
        self.next_enhance_request_id += 1;
        self.state.begin_prompt_enhance(request_id, &raw_prompt);
        let runtime_tx = self.runtime_tx.clone();
        let config = self.state.effective_config.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop enhance runtime");
            let result = runtime.block_on(async move {
                crate::tui::prompt_enhance::enhance_prompt(&config, &raw_prompt)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::EnhanceFinished { request_id, result });
        });
    }

    fn send_prompt_review(&mut self, send_enhanced: bool) {
        let Some(prompt_dispatch) = self.state.build_prompt_dispatch(send_enhanced) else {
            self.state
                .set_status_message("enhanced draft is not ready yet");
            return;
        };
        let prompt = prompt_dispatch.dispatch_prompt_text.clone();
        self.state.cancel_prompt_review();
        self.launch_run_with_options(prompt, prompt_dispatch, None);
    }

    fn load_provider_models(&mut self) {
        let normalized = normalize_provider_base_url(&self.state.provider_base_url_input);
        if normalized.is_empty() {
            self.state.fail_provider_model_load("provider URL is empty");
            return;
        }
        self.state.begin_provider_model_load(normalized.clone());
        let runtime_tx = self.runtime_tx.clone();
        let config = self.state.effective_config.clone();
        std::thread::spawn(move || {
            let request_base_url = normalized.clone();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop model-discovery runtime");
            let result = runtime.block_on(async move {
                fetch_provider_model_infos(&config, &request_base_url)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::ModelCatalogLoaded {
                requested_base_url: normalized,
                result,
            });
        });
    }

    fn apply_provider_session(&mut self) {
        let Some(config) = self.apply_provider_selection_to_effective_config() else {
            return;
        };
        self.state.reset_effective_config(config);
        self.preferences.set_workspace_override(
            &self.app.workspace.root,
            full_effective_override(&self.state.effective_config),
        );
        self.preferences.last_workspace = Some(self.app.workspace.root.clone());
        self.persist_preferences();
        self.state
            .set_status_message("applied provider selection to this workspace session");
        self.state.hide_overlay();
    }

    fn save_provider_project(&mut self) {
        let Some(config) = self.apply_provider_selection_to_effective_config() else {
            return;
        };
        self.state.reset_effective_config(config);
        match self.state.config_editor.save_scope(
            &self.app.workspace.root,
            crate::tui::config_editor::ConfigSaveScope::Project,
        ) {
            Ok(message) => {
                self.preferences
                    .clear_workspace_override(&self.app.workspace.root);
                self.persist_preferences();
                self.reload_config();
                self.state.set_status_message(message);
            }
            Err(error) => self
                .state
                .set_status_message(format!("config save failed: {error}")),
        }
    }

    fn save_provider_global(&mut self) {
        let Some(config) = self.apply_provider_selection_to_effective_config() else {
            return;
        };
        self.state.reset_effective_config(config);
        match self.state.config_editor.save_scope(
            &self.app.workspace.root,
            crate::tui::config_editor::ConfigSaveScope::Global,
        ) {
            Ok(message) => {
                self.preferences
                    .clear_workspace_override(&self.app.workspace.root);
                self.persist_preferences();
                self.reload_config();
                self.state.set_status_message(message);
            }
            Err(error) => self
                .state
                .set_status_message(format!("config save failed: {error}")),
        }
    }

    fn apply_session_config(&mut self) {
        match self.state.config_editor.build_session_override() {
            Ok(patch) => {
                let config = apply_config_patch(self.app.config.clone(), patch.clone());
                self.state.reset_effective_config(config);
                self.preferences
                    .set_workspace_override(&self.app.workspace.root, patch);
                self.preferences.last_workspace = Some(self.app.workspace.root.clone());
                self.persist_preferences();
                self.state.set_status_message("applied session override");
            }
            Err(error) => self
                .state
                .set_status_message(format!("config error: {error}")),
        }
    }

    fn toggle_access_mode_session(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("access mode cannot change while a run is active");
            return;
        }

        let mut config = self.state.effective_config.clone();
        config.permissions.access_mode = config.permissions.access_mode.next();
        let access_mode = config.permissions.access_mode;
        self.state.reset_effective_config(config);
        self.preferences.set_workspace_override(
            &self.app.workspace.root,
            full_effective_override(&self.state.effective_config),
        );
        self.preferences.last_workspace = Some(self.app.workspace.root.clone());
        self.persist_preferences();
        self.state.set_status_message(format!(
            "session access mode set to {}",
            access_mode.label()
        ));
    }

    fn save_project_config(&mut self) {
        match self.state.config_editor.save_scope(
            &self.app.workspace.root,
            crate::tui::config_editor::ConfigSaveScope::Project,
        ) {
            Ok(message) => {
                self.preferences
                    .clear_workspace_override(&self.app.workspace.root);
                self.persist_preferences();
                self.reload_config();
                self.state.set_status_message(message);
            }
            Err(error) => self
                .state
                .set_status_message(format!("config save failed: {error}")),
        }
    }

    fn save_global_config(&mut self) {
        match self.state.config_editor.save_scope(
            &self.app.workspace.root,
            crate::tui::config_editor::ConfigSaveScope::Global,
        ) {
            Ok(message) => {
                self.preferences
                    .clear_workspace_override(&self.app.workspace.root);
                self.persist_preferences();
                self.reload_config();
                self.state.set_status_message(message);
            }
            Err(error) => self
                .state
                .set_status_message(format!("config save failed: {error}")),
        }
    }

    fn reload_config(&mut self) {
        match ConfigLoader::load(&self.app.workspace.root, None) {
            Ok(config) => {
                self.app.config = config.clone();
                let effective =
                    apply_preferences_override(&self.preferences, &self.app.workspace.root, config);
                self.state.reset_effective_config(effective);
                if !self.state.provider_base_url_input.trim().is_empty() {
                    self.load_provider_models();
                }
            }
            Err(error) => self
                .state
                .set_status_message(format!("failed to reload config: {error}")),
        }
    }

    fn switch_workspace(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("workspace cannot change while a run is active");
            return;
        }
        let Some(requested) = self.resolve_workspace_input() else {
            return;
        };
        self.spawn_workspace_load(requested);
    }

    fn spawn_workspace_load(&self, requested: Utf8PathBuf) {
        let store = self.app.session_service.store.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop workspace runtime");
            let result = runtime.block_on(async move {
                let app = AppBootstrap::rebuild_for_directory(&requested, store)
                    .await
                    .map_err(|error| error.to_string())?;
                let snapshot = load_snapshot_for_selection(&app, None)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(WorkspaceLoadResult { app, snapshot })
            });
            let _ = runtime_tx.send(RuntimeMessage::WorkspaceSwitched(result));
        });
    }

    fn browse_workspace_dialog(&mut self) {
        let start_dir = if self.state.workspace_input.trim().is_empty() {
            Some(self.app.workspace.root.clone())
        } else {
            self.resolve_workspace_input()
                .or_else(|| Some(self.app.workspace.root.clone()))
        };
        match pick_workspace_directory(start_dir.as_ref()) {
            Ok(Some(path)) => {
                self.state.set_workspace_input(path.to_string());
                self.state
                    .set_status_message(format!("selected workspace {}", path));
            }
            Ok(None) => {}
            Err(error) => self
                .state
                .set_status_message(format!("workspace browse failed: {error}")),
        }
    }

    fn browse_image_dialog(&mut self) {
        match pick_image_file(Some(&self.app.workspace.cwd)) {
            Ok(Some(path)) => self.state.attach_image_path(path),
            Ok(None) => {}
            Err(error) => self
                .state
                .set_status_message(format!("image browse failed: {error}")),
        }
    }

    fn resolve_workspace_input(&mut self) -> Option<camino::Utf8PathBuf> {
        let requested = self.state.workspace_input.trim().to_string();
        if requested.is_empty() {
            self.state.set_status_message("workspace path is empty");
            return None;
        }
        let requested_input = camino::Utf8PathBuf::from(requested);
        let requested = match normalize_path(&self.app.workspace.cwd, &requested_input) {
            Ok(value) => value,
            Err(error) => {
                self.state
                    .set_status_message(format!("invalid workspace path: {error}"));
                return None;
            }
        };
        let metadata = match std::fs::metadata(requested.as_std_path()) {
            Ok(value) => value,
            Err(error) => {
                self.state.set_status_message(format!(
                    "workspace path is not accessible: {} ({error})",
                    requested
                ));
                return None;
            }
        };
        if !metadata.is_dir() {
            self.state
                .set_status_message(format!("workspace path is not a directory: {}", requested));
            return None;
        }
        Some(requested)
    }

    fn open_current_workspace_in_file_manager(&mut self) {
        let root = self.app.workspace.root.clone();
        self.open_path_in_file_manager(&root);
    }

    fn open_project_config_folder(&mut self) {
        let [primary, secondary] = project_config_paths(&self.app.workspace.root);
        let config_path = if secondary.exists() {
            secondary
        } else if primary.exists() {
            primary
        } else {
            primary
        };
        let Some(folder) = config_path.parent().map(camino::Utf8Path::to_path_buf) else {
            self.state
                .set_status_message("project config folder could not be resolved");
            return;
        };
        self.open_path_in_file_manager(&folder);
    }

    fn open_global_config_folder(&mut self) {
        let config_path = match global_config_path() {
            Ok(path) => path,
            Err(error) => {
                self.state
                    .set_status_message(format!("global config path failed: {error}"));
                return;
            }
        };
        let Some(folder) = config_path.parent().map(camino::Utf8Path::to_path_buf) else {
            self.state
                .set_status_message("global config folder could not be resolved");
            return;
        };
        if let Err(error) = std::fs::create_dir_all(folder.as_std_path()) {
            self.state.set_status_message(format!(
                "failed to create global config folder {}: {error}",
                folder
            ));
            return;
        }
        self.open_path_in_file_manager(&folder);
    }

    fn open_typed_path_in_file_manager(&mut self) {
        if let Some(path) = self.resolve_workspace_input() {
            self.open_path_in_file_manager(&path);
        }
    }

    fn open_selected_artifact_folder(&mut self) {
        let Some(path_text) = self.state.selected_artifact_path() else {
            self.state.set_status_message("select an artifact first");
            return;
        };
        let path = Utf8PathBuf::from(path_text);
        let absolute_path = if path.is_absolute() {
            path
        } else {
            self.app.workspace.root.join(path)
        };
        let folder = if absolute_path.is_dir() {
            absolute_path
        } else if let Some(parent) = absolute_path.parent() {
            parent.to_path_buf()
        } else {
            self.app.workspace.root.clone()
        };
        self.open_path_in_file_manager(&folder);
    }

    fn open_path_in_file_manager(&mut self, path: &camino::Utf8Path) {
        let mut command = if cfg!(target_os = "windows") {
            ProcessCommand::new("explorer")
        } else if cfg!(target_os = "macos") {
            ProcessCommand::new("open")
        } else {
            ProcessCommand::new("xdg-open")
        };
        match command.arg(path.as_str()).spawn() {
            Ok(_) => self
                .state
                .set_status_message(format!("opened {} in file manager", path)),
            Err(error) => self
                .state
                .set_status_message(format!("failed to open {} in file manager: {error}", path)),
        }
    }

    fn provider_selection_patch(&mut self) -> Option<PartialResolvedConfig> {
        let base_url = normalize_provider_base_url(&self.state.provider_base_url_input);
        if base_url.is_empty() {
            self.state.set_status_message("provider URL is empty");
            return None;
        }
        let Some(model) = self.state.selected_provider_model() else {
            self.state
                .set_status_message("select one model before applying provider settings");
            return None;
        };
        let model = model.to_string();
        let mut hydrated_model_config = self.state.effective_config.model.clone();
        hydrated_model_config.base_url = base_url.clone();
        hydrated_model_config.model = model.clone();
        if let Some(info) = self.state.selected_provider_model_info() {
            apply_provider_model_info_to_config(&mut hydrated_model_config, info);
        }
        Some(PartialResolvedConfig {
            model: Some(PartialModelConfig {
                base_url: Some(base_url),
                model: Some(model),
                context_window: Some(hydrated_model_config.context_window),
                max_output_tokens: Some(hydrated_model_config.max_output_tokens),
                supports_tools: Some(hydrated_model_config.supports_tools),
                supports_reasoning: Some(hydrated_model_config.supports_reasoning),
                supports_images: Some(hydrated_model_config.supports_images),
                parallel_tool_calls: Some(hydrated_model_config.parallel_tool_calls),
                max_parallel_predictions: Some(hydrated_model_config.max_parallel_predictions),
                extra_body_json: hydrated_model_config.extra_body_json.clone(),
                ..PartialModelConfig::default()
            }),
            ..PartialResolvedConfig::default()
        })
    }

    fn apply_provider_selection_to_effective_config(&mut self) -> Option<ResolvedConfig> {
        let patch = self.provider_selection_patch()?;
        Some(apply_config_patch(
            self.state.effective_config.clone(),
            patch,
        ))
    }

    fn persist_preferences(&mut self) {
        if !self.persist_preferences_to_disk {
            return;
        }
        self.preferences.window_opacity_percent = Some(self.state.window_opacity_percent);
        self.preferences.last_workspace = Some(self.app.workspace.root.clone());
        if let Err(error) = self.preferences.save() {
            self.state
                .set_status_message(format!("failed to save desktop preferences: {error}"));
        }
    }

    fn answer_permission(&mut self, allow: bool) {
        if let Some(response) = self.permission_response.take() {
            if let Err(error) = response.send(allow) {
                self.state
                    .set_status_message(format!("failed to answer confirmation: {error}"));
            }
        }
        self.state.clear_permission();
    }

    fn launch_run_with_options(
        &mut self,
        prompt: String,
        prompt_dispatch: crate::session::PromptDispatchPart,
        review_request: Option<ReviewRequest>,
    ) {
        if prompt.trim().is_empty() && review_request.is_none() {
            return;
        }
        let image_paths = self.state.image_attachment_paths.clone();
        if !image_paths.is_empty() && !self.state.effective_config.model.supports_images {
            self.state.set_status_message(format!(
                "model `{}` does not advertise image support",
                self.state.effective_config.model.model
            ));
            return;
        }
        let request = RunRequest {
            prompt: prompt.clone(),
            session_id: self.state.app_state.current_session_id,
            continue_last: false,
            title: self
                .state
                .app_state
                .current_session_id
                .is_none()
                .then(|| title_from_prompt(&prompt)),
            cwd: self.app.workspace.cwd.clone(),
            model: self.state.effective_config.model.model.clone(),
            base_url: self.state.effective_config.model.base_url.clone(),
            config_override: Some(full_effective_override(&self.state.effective_config)),
            output_mode: OutputMode::Human,
            show_reasoning: true,
            prompt_dispatch: Some(prompt_dispatch.clone()),
            editor_context: Some(self.current_editor_context()),
            review_request,
            image_paths,
        };
        self.state.push_local_prompt_dispatch(&prompt_dispatch);
        self.state.draft_prompt.clear();
        self.state.image_attachment_paths.clear();
        self.state.image_attachment_input.clear();
        let run_service = self.app.run_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let mut renderer = DesktopRenderer {
                tx: runtime_tx.clone(),
            };
            let mut prompt = DesktopConfirmationPrompt {
                tx: runtime_tx.clone(),
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop worker runtime");
            runtime.block_on(async move {
                if let Err(error) = run_service
                    .execute(AppCommand::Run(request), &mut renderer, &mut prompt)
                    .await
                {
                    let _ = runtime_tx.send(RuntimeMessage::Finished(Err(error.to_string())));
                }
            });
        });
    }

    fn current_editor_context(&self) -> EditorContext {
        let shell_family = self
            .state
            .effective_config
            .shell
            .family
            .unwrap_or(if cfg!(windows) {
                ShellFamily::PowerShell
            } else {
                ShellFamily::Bash
            });
        let mut visible_files = self
            .state
            .app_state
            .session_state
            .as_ref()
            .map(|state| state.active_targets.clone())
            .unwrap_or_default();
        visible_files.sort();
        visible_files.dedup();
        let visible_files = visible_files.into_iter().take(8).collect::<Vec<_>>();
        EditorContext {
            active_file: visible_files.first().cloned(),
            open_tabs: visible_files.clone(),
            visible_files,
            shell_family,
            current_time_ms: SystemClock::now_ms(),
        }
    }

    fn drain_runtime_messages(&mut self) -> bool {
        let mut changed = false;
        while let Ok(message) = self.runtime_rx.try_recv() {
            changed = true;
            match message {
                RuntimeMessage::RunEvent(event) => {
                    self.state.apply_run_event(&event);
                    if event_requires_todo_refresh(&event) {
                        if let Some(session_id) = self.state.app_state.current_session_id {
                            self.spawn_current_todos_refresh(session_id);
                        }
                    }
                }
                RuntimeMessage::Finished(result) => match result {
                    Ok(summary) => {
                        self.state.app_state.set_summary(summary);
                        self.refresh_snapshot();
                        if let Some(session_id) = self.state.app_state.current_session_id {
                            self.spawn_session_load(session_id, SessionLoadReason::CurrentRefresh);
                        }
                    }
                    Err(error) => {
                        self.state.app_state.run_status = crate::tui::state::RunStatus::Failed;
                        self.state.set_status_message(error);
                    }
                },
                RuntimeMessage::Permission(request, response) => {
                    self.permission_response = Some(response);
                    self.state.set_permission(&request);
                }
                RuntimeMessage::EnhanceFinished { request_id, result } => match result {
                    Ok(draft) => {
                        if self.state.finish_prompt_enhance(request_id, draft) {
                            self.state.set_status_message("review enhanced draft");
                        }
                    }
                    Err(error) => {
                        self.state.cancel_prompt_review();
                        self.state
                            .set_status_message(format!("prompt enhancement failed: {error}"));
                    }
                },
                RuntimeMessage::SnapshotLoaded(result) => match result {
                    Ok(snapshot) => self.state.replace_snapshot(snapshot),
                    Err(error) => self.state.set_status_message(error),
                },
                RuntimeMessage::SessionLoaded {
                    session_id,
                    reason,
                    result,
                } => match result {
                    Ok(loaded) => {
                        if matches!(
                            self.state.app_state.run_status,
                            crate::tui::state::RunStatus::Running
                                | crate::tui::state::RunStatus::Confirming
                        ) {
                            continue;
                        }
                        let should_apply = match reason {
                            SessionLoadReason::UserSelection => {
                                self.state.selected_session_id() == Some(session_id)
                            }
                            SessionLoadReason::CurrentRefresh => {
                                self.state.app_state.current_session_id == Some(session_id)
                            }
                        };
                        if !should_apply {
                            continue;
                        }
                        self.state.load_open_session(
                            &loaded.session,
                            &loaded.transcript,
                            &loaded.turn_items,
                            loaded.state,
                            loaded.todos,
                        );
                        self.state
                            .set_status_message(format!("opened session {}", session_id));
                    }
                    Err(error) => self.state.set_status_message(error),
                },
                RuntimeMessage::SessionDeleted { session_id, result } => match result {
                    Ok(snapshot) => {
                        let deleted_was_current =
                            self.state.app_state.current_session_id == Some(session_id);
                        self.state.replace_snapshot(snapshot);
                        if deleted_was_current {
                            if let Some(next_session_id) = self.state.selected_session_id() {
                                self.state.set_status_message(format!(
                                    "deleted chat {}; opening {}...",
                                    session_id, next_session_id
                                ));
                                self.spawn_session_load(
                                    next_session_id,
                                    SessionLoadReason::UserSelection,
                                );
                            } else {
                                self.state.start_new_chat();
                                self.state
                                    .set_status_message(format!("deleted chat {}", session_id));
                            }
                        } else {
                            self.state
                                .set_status_message(format!("deleted chat {}", session_id));
                        }
                    }
                    Err(error) => self
                        .state
                        .set_status_message(format!("chat delete failed: {error}")),
                },
                RuntimeMessage::ProjectDeleted {
                    project_id,
                    project_root,
                    result,
                } => match result {
                    Ok(loaded) => {
                        let deleted_was_current = self.app.workspace.project_id == project_id;
                        self.preferences.mark_project_deleted(&project_root);
                        self.app = loaded.app.clone();
                        self.preferences
                            .unmark_project_deleted(&self.app.workspace.root);
                        if deleted_was_current {
                            let effective = apply_preferences_override(
                                &self.preferences,
                                &self.app.workspace.root,
                                self.app.config.clone(),
                            );
                            self.state = DesktopState::new(loaded.snapshot, effective);
                            self.state.workspace_input = self.app.workspace.cwd.to_string();
                            if let Some(opacity) = self.preferences.window_opacity_percent {
                                self.state.set_window_opacity_percent(opacity);
                            }
                            self.preferences.last_workspace = Some(self.app.workspace.root.clone());
                            self.persist_preferences();
                            if !self.state.provider_base_url_input.trim().is_empty() {
                                self.load_provider_models();
                            }
                        } else {
                            self.state.replace_snapshot(loaded.snapshot);
                            self.persist_preferences();
                        }
                        if let Some(next_session_id) = self.state.selected_session_id() {
                            self.state.set_status_message(format!(
                                "deleted project {}; opening {}...",
                                project_id, next_session_id
                            ));
                            self.spawn_session_load(
                                next_session_id,
                                SessionLoadReason::UserSelection,
                            );
                        } else {
                            self.state.start_new_chat();
                            self.state
                                .set_status_message(format!("deleted project {}", project_id));
                        }
                    }
                    Err(error) => self
                        .state
                        .set_status_message(format!("project delete failed: {error}")),
                },
                RuntimeMessage::CurrentTodosLoaded { session_id, result } => match result {
                    Ok(todos) => {
                        if self.state.app_state.current_session_id == Some(session_id) {
                            self.state.app_state.set_sidebar_todos(todos);
                        }
                    }
                    Err(error) => self.state.set_status_message(error),
                },
                RuntimeMessage::ModelCatalogLoaded {
                    requested_base_url,
                    result,
                } => {
                    if normalize_provider_base_url(&self.state.provider_base_url_input)
                        != requested_base_url
                    {
                        continue;
                    }
                    match result {
                        Ok(models) => self.state.finish_provider_model_load(models),
                        Err(error) => self.state.fail_provider_model_load(error),
                    }
                }
                RuntimeMessage::HistoryExported(result) => match result {
                    Ok(path) => self
                        .state
                        .set_status_message(format!("exported history markdown to {}", path)),
                    Err(error) => self
                        .state
                        .set_status_message(format!("history markdown export failed: {error}")),
                },
                RuntimeMessage::WorkspaceSwitched(result) => match result {
                    Ok(loaded) => {
                        self.app = loaded.app.clone();
                        self.preferences
                            .unmark_project_deleted(&self.app.workspace.root);
                        let effective = apply_preferences_override(
                            &self.preferences,
                            &self.app.workspace.root,
                            self.app.config.clone(),
                        );
                        self.state = DesktopState::new(loaded.snapshot, effective);
                        self.state.workspace_input = self.app.workspace.cwd.to_string();
                        if let Some(opacity) = self.preferences.window_opacity_percent {
                            self.state.set_window_opacity_percent(opacity);
                        }
                        self.preferences.last_workspace = Some(self.app.workspace.root.clone());
                        self.persist_preferences();
                        if !self.state.provider_base_url_input.trim().is_empty() {
                            self.load_provider_models();
                        }
                        self.state.set_status_message(format!(
                            "workspace set to {}",
                            self.app.workspace.root
                        ));
                    }
                    Err(error) => self.state.set_status_message(error),
                },
            }
        }
        changed
    }
}

fn title_from_prompt(prompt: &str) -> String {
    let first_line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("New Chat");
    let mut title = first_line
        .replace('`', "")
        .replace('\t', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let max_chars = 42;
    if title.chars().count() > max_chars {
        title = title.chars().take(max_chars - 1).collect::<String>();
        title.push('…');
    }
    if title.is_empty() {
        "New Chat".to_string()
    } else {
        title
    }
}

#[cfg(test)]
mod tests {
    use super::{
        fallback_workspace_after_project_delete, first_restorable_project_root, title_from_prompt,
    };
    use crate::session::{ProjectId, ProjectRecord};
    use camino::Utf8Path;

    fn project_record(id: ProjectId, root_path: &str) -> ProjectRecord {
        ProjectRecord {
            id,
            root_path: root_path.into(),
            display_name: root_path.to_string(),
            vcs_kind: "none".to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn desktop_title_from_prompt_is_short_and_human_readable() {
        let title =
            title_from_prompt("  `calculator.py` と `test_calculator.py` を作成してください。");
        assert!(title.starts_with("calculator.py と test_calculator.py"));
        assert!(title.chars().count() <= 42);
        assert_eq!(title_from_prompt("\n\n"), "New Chat");
        assert!(title_from_prompt("a ".repeat(80).as_str()).chars().count() <= 42);
    }

    #[test]
    fn desktop_composer_edit_does_not_rewrite_text_from_render_model() {
        let source = include_str!("app.rs");
        let callback = source
            .split("bridge.on_composer_changed")
            .nth(1)
            .expect("composer callback exists")
            .split("bridge.on_image_path_changed")
            .next()
            .expect("image path callback follows composer callback");
        assert!(callback.contains("render_composer_action_state"));
        assert!(!callback.contains("render_handle"));
    }

    #[test]
    fn project_delete_selects_only_non_deleted_remaining_project() {
        let deleted_id = ProjectId::new();
        let hidden_id = ProjectId::new();
        let kept_id = ProjectId::new();
        let hidden_root = camino::Utf8PathBuf::from("C:/workspace/hidden");
        let deleted_root = Utf8Path::new("C:/workspace/deleted");
        let projects = vec![
            project_record(deleted_id, "C:/workspace/deleted"),
            project_record(hidden_id, "C:/workspace/hidden"),
            project_record(kept_id, "C:/workspace/kept"),
        ];

        let selected =
            first_restorable_project_root(&projects, deleted_id, &[hidden_root], deleted_root)
                .expect("kept project should be restorable");

        assert_eq!(selected, camino::Utf8PathBuf::from("C:/workspace/kept"));
    }

    #[test]
    fn project_delete_fallback_never_returns_deleted_or_hidden_root() {
        let deleted_root = Utf8Path::new("C:/workspace/deleted");
        let hidden_root = camino::Utf8PathBuf::from("C:/workspace/hidden");
        let data_dir = Utf8Path::new("C:/moyai-data");

        let fallback =
            fallback_workspace_after_project_delete(deleted_root, &[hidden_root.clone()], data_dir);

        assert_ne!(fallback.as_path(), deleted_root);
        assert_ne!(fallback, hidden_root);
    }
}

struct DesktopRenderer {
    tx: tokio::sync::mpsc::UnboundedSender<RuntimeMessage>,
}

impl EventRenderer for DesktopRenderer {
    fn render(&mut self, event: &RunEvent) -> Result<(), CliRenderError> {
        self.tx
            .send(RuntimeMessage::RunEvent(event.clone()))
            .map_err(|error| CliRenderError::Message(error.to_string()))
    }

    fn finish(&mut self, summary: &RunSummary) -> Result<(), CliRenderError> {
        self.tx
            .send(RuntimeMessage::Finished(Ok(summary.clone())))
            .map_err(|error| CliRenderError::Message(error.to_string()))
    }

    fn render_session_list(&mut self, _sessions: &[SessionRecord]) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_session_show(
        &mut self,
        _transcript: &crate::session::Transcript,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_session_history_items(
        &mut self,
        _session: &SessionRecord,
        _history_items: &[crate::protocol::HistoryItem],
        _transcript: &crate::session::Transcript,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
}

struct DesktopConfirmationPrompt {
    tx: tokio::sync::mpsc::UnboundedSender<RuntimeMessage>,
}

impl ConfirmationPrompt for DesktopConfirmationPrompt {
    fn confirm(&mut self, request: &PermissionRequest) -> Result<bool, CliPromptError> {
        let (response_tx, response_rx) = mpsc::channel();
        self.tx
            .send(RuntimeMessage::Permission(request.clone(), response_tx))
            .map_err(|error| CliPromptError::Message(error.to_string()))?;
        response_rx
            .recv()
            .map_err(|error| CliPromptError::Message(error.to_string()))
    }
}

fn event_requires_todo_refresh(event: &RunEvent) -> bool {
    matches!(
        event,
        RunEvent::StateUpdated { .. }
            | RunEvent::ToolCallCompleted { .. }
            | RunEvent::ToolCallFailed { .. }
            | RunEvent::ToolProposalRejected { .. }
            | RunEvent::CandidateRepairEditRecorded { .. }
            | RunEvent::RecoverableRuntimeFeedback { .. }
            | RunEvent::SessionCompleted { .. }
            | RunEvent::SessionAwaitingUser { .. }
            | RunEvent::SessionFailed { .. }
    )
}

fn apply_preferences_override(
    preferences: &DesktopPreferences,
    workspace_root: &camino::Utf8Path,
    base_config: ResolvedConfig,
) -> ResolvedConfig {
    match preferences.workspace_override(workspace_root) {
        Some(patch) => apply_config_patch(base_config, patch),
        None => base_config,
    }
}

async fn purge_deleted_project_roots(
    app: &App,
    preferences: &DesktopPreferences,
) -> Result<(), String> {
    if preferences.deleted_project_roots.is_empty() {
        return Ok(());
    }
    let projects = app
        .session_service
        .list_projects(200)
        .await
        .map_err(|error| error.to_string())?;
    for project in projects {
        if preferences.is_project_deleted(&project.root_path) {
            app.session_service
                .delete_project(project.id)
                .await
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

async fn project_root_exists(app: &App, root: &Utf8Path) -> Result<bool, AppRunError> {
    let projects = app
        .session_service
        .list_projects(200)
        .await
        .map_err(|error| AppRunError::Message(error.to_string()))?;
    Ok(projects.iter().any(|project| project.root_path == root))
}

async fn next_project_root_after_delete(
    app: &App,
    deleted_project_id: ProjectId,
    hidden_roots: &[Utf8PathBuf],
    deleted_root: &Utf8Path,
) -> Result<Option<Utf8PathBuf>, String> {
    let projects = app
        .session_service
        .list_projects(30)
        .await
        .map_err(|error| error.to_string())?;
    Ok(first_restorable_project_root(
        &projects,
        deleted_project_id,
        hidden_roots,
        deleted_root,
    ))
}

fn first_restorable_project_root(
    projects: &[ProjectRecord],
    deleted_project_id: ProjectId,
    hidden_roots: &[Utf8PathBuf],
    deleted_root: &Utf8Path,
) -> Option<Utf8PathBuf> {
    projects
        .iter()
        .find(|project| {
            project.id != deleted_project_id
                && project.root_path != deleted_root
                && !hidden_roots.iter().any(|root| root == &project.root_path)
        })
        .map(|project| project.root_path.clone())
}

fn fallback_workspace_after_project_delete(
    deleted_root: &Utf8Path,
    hidden_roots: &[Utf8PathBuf],
    data_dir: &Utf8Path,
) -> Utf8PathBuf {
    let mut candidates = Vec::new();
    if let Some(default_workspace) = super::default_workspace_directory() {
        candidates.push(default_workspace);
    }
    candidates.push(data_dir.join("desktop-workspace"));
    candidates.push(data_dir.join("desktop-workspace-after-delete"));
    candidates
        .into_iter()
        .find(|candidate| {
            candidate != deleted_root && !hidden_roots.iter().any(|root| root == candidate)
        })
        .unwrap_or_else(|| data_dir.join("desktop-workspace-after-delete-2"))
}

fn pick_workspace_directory(
    start_dir: Option<&camino::Utf8PathBuf>,
) -> Result<Option<camino::Utf8PathBuf>, String> {
    let mut dialog = rfd::FileDialog::new();
    if let Some(directory) = start_dir {
        dialog = dialog.set_directory(directory.as_std_path());
    }
    match dialog.pick_folder() {
        Some(path) => camino::Utf8PathBuf::from_path_buf(path)
            .map(Some)
            .map_err(|_| "selected directory path is not valid UTF-8".to_string()),
        None => Ok(None),
    }
}

fn pick_image_file(start_dir: Option<&Utf8Path>) -> Result<Option<Utf8PathBuf>, String> {
    let mut dialog =
        rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "webp", "gif"]);
    if let Some(directory) = start_dir {
        dialog = dialog.set_directory(directory.as_std_path());
    }
    match dialog.pick_file() {
        Some(path) => Utf8PathBuf::from_path_buf(path)
            .map(Some)
            .map_err(|_| "selected image path is not valid UTF-8".to_string()),
        None => Ok(None),
    }
}

fn pick_history_markdown_path(default_file_name: &str) -> Result<Option<Utf8PathBuf>, String> {
    match rfd::FileDialog::new()
        .add_filter("Markdown", &["md"])
        .set_file_name(default_file_name)
        .save_file()
    {
        Some(path) => Utf8PathBuf::from_path_buf(path)
            .map(normalize_markdown_export_path)
            .map(Some)
            .map_err(|_| "selected history export path is not valid UTF-8".to_string()),
        None => Ok(None),
    }
}

fn normalize_markdown_export_path(path: Utf8PathBuf) -> Utf8PathBuf {
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        path
    } else {
        path.with_extension("md")
    }
}

fn full_effective_override(config: &ResolvedConfig) -> PartialResolvedConfig {
    PartialResolvedConfig {
        model: Some(PartialModelConfig {
            base_url: Some(config.model.base_url.clone()),
            model: Some(config.model.model.clone()),
            prompt_profile: Some(config.model.prompt_profile),
            api_key_env: None,
            extra_headers: Some(config.model.extra_headers.clone()),
            request_timeout_ms: Some(config.model.request_timeout_ms),
            stream_idle_timeout_ms: Some(config.model.stream_idle_timeout_ms),
            connect_timeout_ms: Some(config.model.connect_timeout_ms),
            max_retries: Some(config.model.max_retries),
            stream_max_retries: Some(config.model.stream_max_retries),
            context_window: Some(config.model.context_window),
            max_output_tokens: Some(config.model.max_output_tokens),
            temperature: config.model.temperature,
            top_p: config.model.top_p,
            top_k: config.model.top_k,
            presence_penalty: config.model.presence_penalty,
            frequency_penalty: config.model.frequency_penalty,
            seed: config.model.seed,
            stop_sequences: Some(config.model.stop_sequences.clone()),
            supports_tools: Some(config.model.supports_tools),
            supports_reasoning: Some(config.model.supports_reasoning),
            supports_images: Some(config.model.supports_images),
            parallel_tool_calls: Some(config.model.parallel_tool_calls),
            max_parallel_predictions: Some(config.model.max_parallel_predictions),
            extra_body_json: config.model.extra_body_json.clone(),
        }),
        session: Some(crate::config::model::PartialSessionConfig {
            default_title_max_len: None,
            transcript_limit_messages: None,
            auto_resume_last: None,
            max_steps_per_turn: Some(config.session.max_steps_per_turn),
            overflow_margin_tokens: None,
        }),
        inspection: Some(crate::config::model::PartialInspectionConfig {
            default_max_depth: Some(config.inspection.default_max_depth),
            default_max_entries_per_dir: Some(config.inspection.default_max_entries_per_dir),
            max_extensions_reported: Some(config.inspection.max_extensions_reported),
            include_hidden_by_default: Some(config.inspection.include_hidden_by_default),
        }),
        file_guard: Some(crate::config::model::PartialFileGuardConfig {
            max_inline_read_bytes: Some(config.file_guard.max_inline_read_bytes),
            large_file_warning_bytes: Some(config.file_guard.large_file_warning_bytes),
            blocked_read_extensions: Some(config.file_guard.blocked_read_extensions.clone()),
            structured_document_extensions: Some(
                config.file_guard.structured_document_extensions.clone(),
            ),
        }),
        docling: Some(crate::config::model::PartialDoclingConfig {
            enabled: Some(config.docling.enabled),
            base_url: Some(config.docling.base_url.clone()),
            timeout_ms: Some(config.docling.timeout_ms),
            api_key_env: Some(config.docling.api_key_env.clone()),
            headers: Some(config.docling.headers.clone()),
        }),
        mcp: Some(crate::config::model::PartialMcpConfig {
            enabled: Some(config.mcp.enabled),
            servers: Some(config.mcp.servers.clone()),
        }),
        permissions: Some(PartialPermissionsConfig {
            access_mode: Some(config.permissions.access_mode),
            additional_read_roots: Some(config.permissions.additional_read_roots.clone()),
            additional_write_roots: Some(config.permissions.additional_write_roots.clone()),
        }),
        agent: None,
        shell: None,
        format: None,
        instructions: None,
        workspace: None,
        tool_output: None,
        logging: None,
    }
}
