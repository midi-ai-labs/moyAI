use std::sync::Arc;

use tauri::{
    Manager, State, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
};
use tokio::sync::Mutex;

use crate::app::App;
use crate::cli::ReviewDecision;
use crate::error::AppRunError;

use super::app::{DesktopController, PendingPermissionResolution};
use super::args::DesktopArgs;
use super::web_model::DesktopWebState;

type SharedController = Arc<Mutex<DesktopController>>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct DesktopCommandConflict {
    message: String,
}

impl DesktopCommandConflict {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopCommandError {
    kind: &'static str,
    category: DesktopCommandErrorCategory,
    code: DesktopCommandErrorCode,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<DesktopWebState>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum DesktopCommandErrorCategory {
    Unknown,
    Provider,
    Model,
    Image,
    Permission,
    Runtime,
    Storage,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum DesktopCommandErrorCode {
    Unknown,
    ProviderTransport,
    ModelUnavailable,
    ImageUnsupported,
    PermissionPolicyDenied,
    RuntimeFailure,
    StorageFailure,
}

impl DesktopCommandError {
    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: "internal",
            category: DesktopCommandErrorCategory::Unknown,
            code: DesktopCommandErrorCode::Unknown,
            message: message.into(),
            state: None,
        }
    }

    fn internal_with_typed_state(
        category: DesktopCommandErrorCategory,
        code: DesktopCommandErrorCode,
        message: impl Into<String>,
        state: DesktopWebState,
    ) -> Self {
        Self {
            kind: "internal",
            category,
            code,
            message: message.into(),
            state: Some(state),
        }
    }
}

fn command_conflict_error(
    controller: &mut DesktopController,
    conflict: DesktopCommandConflict,
) -> DesktopCommandError {
    controller
        .state
        .set_status_message(conflict.message.clone());
    match controller.next_web_state() {
        Ok(state) => DesktopCommandError {
            kind: "conflict",
            category: DesktopCommandErrorCategory::Unknown,
            code: DesktopCommandErrorCode::Unknown,
            message: conflict.message,
            state: Some(state),
        },
        Err(error) => DesktopCommandError::internal(error),
    }
}

#[cfg(target_os = "windows")]
const HTCAPTION: usize = 2;
#[cfg(target_os = "windows")]
const WM_NCLBUTTONDOWN: u32 = 0x00A1;
#[cfg(target_os = "windows")]
const GWL_EXSTYLE: i32 = -20;
#[cfg(target_os = "windows")]
const LWA_ALPHA: u32 = 0x0000_0002;
#[cfg(target_os = "windows")]
const WS_EX_LAYERED: isize = 0x0008_0000;

#[cfg(target_os = "windows")]
#[link(name = "user32")]
unsafe extern "system" {
    fn ReleaseCapture() -> i32;
    fn SendMessageW(
        hwnd: *mut core::ffi::c_void,
        msg: u32,
        w_param: usize,
        l_param: isize,
    ) -> isize;
    fn GetWindowLongPtrW(hwnd: *mut core::ffi::c_void, index: i32) -> isize;
    fn SetWindowLongPtrW(hwnd: *mut core::ffi::c_void, index: i32, new_long: isize) -> isize;
    fn SetLayeredWindowAttributes(
        hwnd: *mut core::ffi::c_void,
        color_key: u32,
        alpha: u8,
        flags: u32,
    ) -> i32;
}

pub async fn run(app: App, args: DesktopArgs) -> Result<(), AppRunError> {
    let controller = DesktopController::new(app, args).await?;
    let shared: SharedController = Arc::new(Mutex::new(controller));
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            restore_main_window(app);
        }))
        .manage(shared)
        .setup(|app| {
            install_tray(app.handle())?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            desktop_state,
            submit_prompt,
            cancel_run,
            new_chat,
            new_project_session,
            review_uncommitted,
            enhance_prompt,
            send_prompt_review,
            cancel_prompt_review,
            refresh_desktop,
            select_project,
            select_session,
            rejoin_session,
            load_previous_turn_page,
            load_next_turn_page,
            select_chat_session,
            set_session_search,
            set_session_search_include_archived,
            archive_session,
            unarchive_session,
            rollback_session,
            fork_session,
            interrupt_session,
            delete_project,
            delete_session,
            delete_chat_session,
            select_artifact,
            export_history_markdown,
            export_transcript_markdown,
            attach_image,
            browse_image,
            clear_images,
            remove_image,
            show_file_menu,
            show_edit_menu,
            show_view_menu,
            show_help_menu,
            show_project_menu,
            create_project_from_picker,
            show_config_editor,
            show_provider_editor,
            show_workspace_picker,
            show_command_palette,
            show_shortcuts,
            close_overlay,
            switch_workspace,
            browse_workspace,
            open_workspace_folder,
            open_global_config_folder,
            import_global_config_toml,
            open_typed_path,
            open_artifact_folder,
            set_local_search,
            insert_command,
            load_provider_models,
            apply_provider_session,
            save_provider_global,
            reset_config_draft,
            apply_session_config,
            save_global_config,
            toggle_access_mode,
            preview_window_opacity,
            set_window_opacity,
            answer_permission,
            start_window_drag,
            minimize_window,
            is_window_maximized,
            toggle_maximize_window,
            hide_to_tray,
            exit_app
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .map_err(|error| AppRunError::Message(format!("tauri desktop runtime failed: {error}")))
}

#[tauri::command]
fn start_window_drag(window: tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        start_windows_caption_drag(&window)?;
        return Ok(());
    }

    #[cfg(not(target_os = "windows"))]
    window
        .start_dragging()
        .map_err(|error| format!("failed to start window drag: {error}"))
}

#[cfg(target_os = "windows")]
fn start_windows_caption_drag(window: &tauri::WebviewWindow) -> Result<(), String> {
    let hwnd = window
        .hwnd()
        .map_err(|error| format!("failed to get native window handle: {error}"))?;
    unsafe {
        let _ = ReleaseCapture();
        let _ = SendMessageW(hwnd.0 as _, WM_NCLBUTTONDOWN, HTCAPTION, 0);
    }
    Ok(())
}

fn install_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open_moyai", "Open moyAI", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit_moyai", "終了", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut builder = TrayIconBuilder::with_id("moyai-tray")
        .tooltip("moyAI")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open_moyai" => restore_main_window(app),
            "quit_moyai" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                restore_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn restore_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[tauri::command]
fn hide_to_tray(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

#[tauri::command]
fn minimize_window(window: tauri::WebviewWindow) -> Result<(), String> {
    window
        .minimize()
        .map_err(|error| format!("failed to minimize window: {error}"))
}

#[tauri::command]
fn is_window_maximized(window: tauri::WebviewWindow) -> Result<bool, String> {
    window
        .is_maximized()
        .map_err(|error| format!("failed to read maximize state: {error}"))
}

#[tauri::command]
fn toggle_maximize_window(window: tauri::WebviewWindow) -> Result<bool, String> {
    let is_maximized = window
        .is_maximized()
        .map_err(|error| format!("failed to read maximize state: {error}"))?;
    if is_maximized {
        window
            .unmaximize()
            .map_err(|error| format!("failed to restore window: {error}"))?;
    } else {
        window
            .maximize()
            .map_err(|error| format!("failed to maximize window: {error}"))?;
    }
    Ok(!is_maximized)
}

#[tauri::command]
fn exit_app(app: tauri::AppHandle) {
    app.exit(0);
}

async fn mutate_controller<F>(
    controller: State<'_, SharedController>,
    action: F,
) -> Result<DesktopWebState, String>
where
    F: FnOnce(&mut DesktopController),
{
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    action(&mut controller);
    controller.drain_runtime_messages();
    controller.next_web_state()
}

async fn mutate_controller_checked<F>(
    controller: State<'_, SharedController>,
    action: F,
) -> Result<DesktopWebState, DesktopCommandError>
where
    F: FnOnce(&mut DesktopController) -> Result<(), DesktopCommandConflict>,
{
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if let Err(conflict) = action(&mut controller) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    controller.drain_runtime_messages();
    controller
        .next_web_state()
        .map_err(DesktopCommandError::internal)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopDraftActionTarget {
    workspace_path: String,
    session_id: Option<String>,
    owner_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopRunMutationTarget {
    workspace_path: String,
    session_id: Option<String>,
    runtime_owner_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopSessionSearchTarget {
    workspace_path: String,
    project_id: Option<String>,
}

fn validate_session_search_target(
    expected: &DesktopSessionSearchTarget,
    workspace_path: &str,
    project_id: Option<String>,
) -> Result<(), DesktopCommandConflict> {
    if expected.workspace_path != workspace_path || expected.project_id != project_id {
        return Err(DesktopCommandConflict::new(
            "the session search owner changed before the query was applied; review the current project and try again",
        ));
    }
    Ok(())
}

fn ensure_session_search_target(
    controller: &DesktopController,
    expected: &DesktopSessionSearchTarget,
) -> Result<(), DesktopCommandConflict> {
    validate_session_search_target(
        expected,
        controller.app.workspace.root.as_str(),
        controller
            .state
            .selected_project_id()
            .map(|project_id| project_id.to_string()),
    )
}

fn ensure_draft_action_target(
    controller: &DesktopController,
    expected: &DesktopDraftActionTarget,
) -> Result<(), DesktopCommandConflict> {
    let session_id = controller
        .state
        .app_state
        .current_session_id
        .map(|session_id| session_id.to_string());
    validate_draft_action_target(
        expected,
        controller.app.workspace.root.as_str(),
        session_id,
        controller.state.composer.owner_generation(),
    )
}

fn validate_draft_action_target(
    expected: &DesktopDraftActionTarget,
    workspace_path: &str,
    session_id: Option<String>,
    owner_generation: u64,
) -> Result<(), DesktopCommandConflict> {
    if expected.workspace_path != workspace_path
        || expected.session_id != session_id
        || expected.owner_generation != owner_generation
    {
        return Err(DesktopCommandConflict::new(
            "the request draft owner changed before the action was applied; review the current chat and try again",
        ));
    }
    Ok(())
}

fn validate_run_mutation_target(
    expected: &DesktopRunMutationTarget,
    workspace_path: &str,
    session_id: Option<String>,
    runtime_owner_token: String,
) -> Result<(), DesktopCommandConflict> {
    if expected.workspace_path != workspace_path
        || expected.session_id != session_id
        || expected.runtime_owner_token != runtime_owner_token
    {
        return Err(DesktopCommandConflict::new(
            "the active run owner changed before Stop was applied; review the current task and try again",
        ));
    }
    Ok(())
}

fn ensure_run_mutation_target(
    controller: &DesktopController,
    expected: &DesktopRunMutationTarget,
) -> Result<(), DesktopCommandConflict> {
    let (runtime_owner_token, _) = controller.access_mode_mutation_runtime_contract();
    validate_run_mutation_target(
        expected,
        controller.app.workspace.root.as_str(),
        controller
            .state
            .app_state
            .current_session_id
            .map(|session_id| session_id.to_string()),
        runtime_owner_token,
    )
}

fn rejected_action(controller: &DesktopController, fallback: &str) -> DesktopCommandConflict {
    DesktopCommandConflict::new(
        controller
            .state
            .app_state
            .status_message
            .as_deref()
            .filter(|message| !message.trim().is_empty())
            .unwrap_or(fallback),
    )
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopRowMutationTarget {
    workspace_path: String,
    owner_project_id: Option<String>,
    owner_session_id: Option<String>,
    row_id: String,
}

fn validate_row_mutation_target(
    expected: &DesktopRowMutationTarget,
    workspace_path: &str,
    owner_project_id: Option<String>,
    owner_session_id: Option<String>,
    actual_row_id: Option<&str>,
) -> Result<(), DesktopCommandConflict> {
    if expected.workspace_path != workspace_path
        || expected.owner_project_id != owner_project_id
        || expected.owner_session_id != owner_session_id
        || actual_row_id != Some(expected.row_id.as_str())
    {
        return Err(DesktopCommandConflict::new(
            "the selected view or row changed before the operation was applied; review the current row and try again",
        ));
    }
    Ok(())
}

fn ensure_row_mutation_target(
    controller: &DesktopController,
    expected: &DesktopRowMutationTarget,
    actual_row_id: Option<String>,
) -> Result<(), DesktopCommandConflict> {
    if controller.state.snapshot.workspace_path != controller.app.workspace.root.as_str() {
        return Err(DesktopCommandConflict::new(
            "the workspace projection changed before the operation was applied; refresh and try again",
        ));
    }
    validate_row_mutation_target(
        expected,
        controller.app.workspace.root.as_str(),
        controller
            .state
            .selected_project_id()
            .map(|project_id| project_id.to_string()),
        controller
            .state
            .selected_session_id()
            .map(|session_id| session_id.to_string()),
        actual_row_id.as_deref(),
    )
}

#[derive(Debug, Clone, Copy)]
enum DesktopRowCollection {
    Project,
    Session,
    QuickChatSession,
    Artifact,
    Attachment,
    Command,
}

fn ensure_indexed_row_mutation_target(
    controller: &DesktopController,
    expected: &DesktopRowMutationTarget,
    collection: DesktopRowCollection,
    index: usize,
) -> Result<(), DesktopCommandConflict> {
    let actual = match collection {
        DesktopRowCollection::Project => controller
            .state
            .snapshot
            .project_rows
            .get(index)
            .map(|row| row.project_id.to_string()),
        DesktopRowCollection::Session => controller
            .state
            .snapshot
            .session_rows
            .get(index)
            .map(|row| row.session_id.to_string()),
        DesktopRowCollection::QuickChatSession => controller
            .state
            .snapshot
            .chat_session_rows
            .get(index)
            .map(|row| row.session_id.to_string()),
        DesktopRowCollection::Artifact => controller
            .state
            .selected_detail()
            .artifacts
            .get(index)
            .map(|row| row.path.clone()),
        DesktopRowCollection::Attachment => controller
            .state
            .composer
            .image_attachment_paths
            .get(index)
            .map(|path| path.to_string()),
        DesktopRowCollection::Command => controller
            .state
            .snapshot
            .command_rows
            .get(index)
            .map(|row| row.path.clone()),
    };
    ensure_row_mutation_target(controller, expected, actual)
}

fn validated_session_id(
    controller: &DesktopController,
    index: usize,
) -> Result<crate::session::SessionId, DesktopCommandConflict> {
    controller
        .state
        .snapshot
        .session_rows
        .get(index)
        .map(|row| row.session_id)
        .ok_or_else(|| DesktopCommandConflict::new("the session row is no longer available"))
}

fn validated_quick_chat_session_id(
    controller: &DesktopController,
    index: usize,
) -> Result<crate::session::SessionId, DesktopCommandConflict> {
    controller
        .state
        .snapshot
        .chat_session_rows
        .get(index)
        .map(|row| row.session_id)
        .ok_or_else(|| DesktopCommandConflict::new("the quick-chat row is no longer available"))
}

fn validated_project_id(
    controller: &DesktopController,
    index: usize,
) -> Result<crate::session::ProjectId, DesktopCommandConflict> {
    controller
        .state
        .snapshot
        .project_rows
        .get(index)
        .map(|row| row.project_id)
        .ok_or_else(|| DesktopCommandConflict::new("the project row is no longer available"))
}

fn ensure_stable_view_admission(
    controller: &DesktopController,
    action: &str,
) -> Result<(), DesktopCommandConflict> {
    if controller.state.can_begin_navigation() {
        return Ok(());
    }
    Err(DesktopCommandConflict::new(format!(
        "{action} cannot start while the current view is changing"
    )))
}

fn ensure_session_archive_admission(
    controller: &DesktopController,
    index: usize,
) -> Result<(), DesktopCommandConflict> {
    let row = controller
        .state
        .snapshot
        .session_rows
        .get(index)
        .ok_or_else(|| DesktopCommandConflict::new("the session row is no longer available"))?;
    validate_session_archive_loaded_status(row.loaded_status)
}

fn validate_session_archive_loaded_status(
    loaded_status: crate::session::LoadedSessionStatus,
) -> Result<(), DesktopCommandConflict> {
    if loaded_status == crate::session::LoadedSessionStatus::Active {
        return Err(DesktopCommandConflict::new(
            "an active session must be stopped before it can be archived",
        ));
    }
    Ok(())
}

#[tauri::command]
async fn desktop_state(
    window: tauri::WebviewWindow,
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    apply_native_window_opacity(&window, controller.state.view.window_opacity_percent)?;
    controller.next_web_state()
}

#[tauri::command]
async fn submit_prompt(
    controller: State<'_, SharedController>,
    text: String,
    expected_target: DesktopDraftActionTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_draft_action_target(controller, &expected_target)?;
        if !controller.start_run(text) {
            return Err(rejected_action(controller, "the prompt was not submitted"));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn cancel_run(
    controller: State<'_, SharedController>,
    expected_target: DesktopRunMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_run_mutation_target(controller, &expected_target)?;
        controller.cancel_active_run();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn new_chat(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        if !controller.start_quick_chat() {
            return Err(rejected_action(controller, "new chat was not started"));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn new_project_session(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Project,
            index,
        )?;
        if !controller.start_project_session(index) {
            return Err(rejected_action(
                controller,
                "new project chat was not started",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn review_uncommitted(
    controller: State<'_, SharedController>,
    text: String,
    expected_target: DesktopDraftActionTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_draft_action_target(controller, &expected_target)?;
        if !controller.start_review_uncommitted(text) {
            return Err(rejected_action(controller, "the review was not started"));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn enhance_prompt(
    controller: State<'_, SharedController>,
    text: String,
    expected_target: DesktopDraftActionTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_draft_action_target(controller, &expected_target)?;
        if !controller.start_prompt_enhance(text) {
            return Err(rejected_action(
                controller,
                "prompt enhancement was not started",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn send_prompt_review(
    controller: State<'_, SharedController>,
    enhanced: bool,
    text: String,
    expected_target: DesktopDraftActionTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_draft_action_target(controller, &expected_target)?;
        if !controller.send_prompt_review(enhanced, text) {
            return Err(rejected_action(
                controller,
                "the reviewed prompt was not sent",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn cancel_prompt_review(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.state.cancel_prompt_review();
    })
    .await
}

#[tauri::command]
async fn refresh_desktop(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::refresh_snapshot).await
}

#[tauri::command]
async fn select_project(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Project,
            index,
        )?;
        if !controller.select_project_and_open(index) {
            return Err(rejected_action(
                controller,
                "project navigation was not started",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn select_session(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Session,
            index,
        )?;
        if !controller.select_session_and_open(index) {
            return Err(rejected_action(
                controller,
                "session navigation was not started",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn rejoin_session(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Session,
            index,
        )?;
        if !controller.rejoin_session_if_admitted(index) {
            return Err(rejected_action(
                controller,
                "session rejoin was not started",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn load_previous_turn_page(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
    expected_offset: usize,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Session,
            index,
        )?;
        ensure_turn_page_offset(controller, expected_offset)?;
        controller.load_previous_turn_page();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn load_next_turn_page(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
    expected_offset: usize,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Session,
            index,
        )?;
        ensure_turn_page_offset(controller, expected_offset)?;
        controller.load_next_turn_page();
        Ok(())
    })
    .await
}

fn ensure_turn_page_offset(
    controller: &DesktopController,
    expected_offset: usize,
) -> Result<(), DesktopCommandConflict> {
    validate_turn_page_offset(
        expected_offset,
        controller.state.selected_detail().turn_page_offset,
    )
}

fn validate_turn_page_offset(
    expected_offset: usize,
    actual_offset: usize,
) -> Result<(), DesktopCommandConflict> {
    if actual_offset != expected_offset {
        return Err(DesktopCommandConflict::new(
            "the displayed turn page changed before the operation was applied; review the current page and try again",
        ));
    }
    Ok(())
}

#[tauri::command]
async fn select_chat_session(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::QuickChatSession,
            index,
        )?;
        if !controller.open_quick_chat_session(index) {
            return Err(rejected_action(
                controller,
                "quick-chat navigation was not started",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn set_session_search(
    controller: State<'_, SharedController>,
    text: String,
    expected_target: DesktopSessionSearchTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_session_search_target(controller, &expected_target)?;
        if !controller.set_session_search(text) {
            return Err(rejected_action(
                controller,
                "the session search was not started",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn set_session_search_include_archived(
    controller: State<'_, SharedController>,
    include_archived: bool,
    expected_target: DesktopSessionSearchTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_session_search_target(controller, &expected_target)?;
        if !controller.set_session_search_include_archived(include_archived) {
            return Err(rejected_action(
                controller,
                "the session search was not started",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn archive_session(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Session,
            index,
        )?;
        ensure_session_archive_admission(controller, index)?;
        let session_id = validated_session_id(controller, index)?;
        if !controller.archive_session(session_id, true) {
            return Err(rejected_action(controller, "chat archive was not started"));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn unarchive_session(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Session,
            index,
        )?;
        let session_id = validated_session_id(controller, index)?;
        if !controller.archive_session(session_id, false) {
            return Err(rejected_action(
                controller,
                "chat unarchive was not started",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn rollback_session(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Session,
            index,
        )?;
        let session_id = validated_session_id(controller, index)?;
        if !controller.rollback_session(session_id) {
            return Err(rejected_action(controller, "chat rollback was not started"));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn fork_session(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Session,
            index,
        )?;
        let session_id = validated_session_id(controller, index)?;
        if !controller.fork_session(session_id) {
            return Err(rejected_action(controller, "chat fork was not started"));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn interrupt_session(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Session,
            index,
        )?;
        let session_id = validated_session_id(controller, index)?;
        if !controller.interrupt_session(session_id) {
            return Err(rejected_action(
                controller,
                "chat interrupt was not started",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn delete_project(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Project,
            index,
        )?;
        let project_id = validated_project_id(controller, index)?;
        if !controller.delete_project(project_id) {
            return Err(rejected_action(
                controller,
                "project deletion was not started",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn delete_session(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Session,
            index,
        )?;
        let session_id = validated_session_id(controller, index)?;
        if !controller.delete_session(session_id) {
            return Err(rejected_action(controller, "chat deletion was not started"));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn delete_chat_session(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::QuickChatSession,
            index,
        )?;
        let session_id = validated_quick_chat_session_id(controller, index)?;
        if !controller.delete_quick_chat_session(session_id) {
            return Err(rejected_action(
                controller,
                "quick-chat deletion was not started",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn select_artifact(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_stable_view_admission(controller, "artifact selection")?;
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Artifact,
            index,
        )?;
        controller.state.select_artifact(index);
        Ok(())
    })
    .await
}

#[tauri::command]
async fn export_history_markdown(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Session,
            index,
        )?;
        let session_id = validated_session_id(controller, index)?;
        controller.export_history_markdown_auto(session_id);
        Ok(())
    })
    .await
}

#[tauri::command]
async fn export_transcript_markdown(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Session,
            index,
        )?;
        controller.export_open_transcript_markdown_auto();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn attach_image(
    app: tauri::AppHandle,
    controller: State<'_, SharedController>,
    text: String,
    expected_target: DesktopDraftActionTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if let Err(conflict) = ensure_draft_action_target(&controller, &expected_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    controller.state.set_image_attachment_input(text);
    let Some(path) = controller.prepare_image_attachment_from_input() else {
        return Err(command_conflict_error(
            &mut controller,
            DesktopCommandConflict::new("the image path was not attached"),
        ));
    };
    controller
        .authorize_attachment_asset(&app, &path)
        .map_err(DesktopCommandError::internal)?;
    controller.state.attach_image_path(path);
    controller.drain_runtime_messages();
    controller
        .next_web_state()
        .map_err(DesktopCommandError::internal)
}

#[tauri::command]
async fn browse_image(
    app: tauri::AppHandle,
    controller: State<'_, SharedController>,
    expected_target: DesktopDraftActionTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if let Err(conflict) = ensure_draft_action_target(&controller, &expected_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    let Some(path) = controller.browse_image_dialog() else {
        return controller
            .next_web_state()
            .map_err(DesktopCommandError::internal);
    };
    controller.drain_runtime_messages();
    if let Err(conflict) = ensure_draft_action_target(&controller, &expected_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    controller
        .authorize_attachment_asset(&app, &path)
        .map_err(DesktopCommandError::internal)?;
    controller.state.attach_image_path(path);
    controller.drain_runtime_messages();
    controller
        .next_web_state()
        .map_err(DesktopCommandError::internal)
}

#[tauri::command]
async fn clear_images(
    controller: State<'_, SharedController>,
    expected_target: DesktopDraftActionTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_draft_action_target(controller, &expected_target)?;
        controller.state.clear_image_attachments();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn remove_image(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Attachment,
            index,
        )?;
        controller.state.remove_image_attachment(index);
        Ok(())
    })
    .await
}

#[tauri::command]
async fn show_file_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| controller.state.show_file_menu()).await
}

#[tauri::command]
async fn show_edit_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| controller.state.show_edit_menu()).await
}

#[tauri::command]
async fn show_view_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| controller.state.show_view_menu()).await
}

#[tauri::command]
async fn show_help_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| controller.state.show_help_menu()).await
}

#[tauri::command]
async fn show_project_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.state.show_project_menu()
    })
    .await
}

#[tauri::command]
async fn create_project_from_picker(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::create_project_from_picker).await
}

#[tauri::command]
async fn show_config_editor(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.state.show_config_editor();
    })
    .await
}

#[tauri::command]
async fn show_provider_editor(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.state.show_provider_editor();
    })
    .await
}

#[tauri::command]
async fn show_workspace_picker(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::show_workspace_picker).await
}

#[tauri::command]
async fn show_command_palette(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.state.show_command_palette();
    })
    .await
}

#[tauri::command]
async fn show_shortcuts(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.state.show_keyboard_shortcuts();
    })
    .await
}

#[tauri::command]
async fn close_overlay(controller: State<'_, SharedController>) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| controller.state.hide_overlay()).await
}

#[tauri::command]
async fn switch_workspace(
    controller: State<'_, SharedController>,
    text: String,
    expected_target: DesktopDraftActionTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_draft_action_target(controller, &expected_target)?;
        controller.state.set_workspace_input(text);
        if !controller.switch_workspace() {
            return Err(rejected_action(
                controller,
                "the workspace was not switched",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn browse_workspace(
    controller: State<'_, SharedController>,
    text: String,
    expected_target: DesktopDraftActionTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if let Err(conflict) = ensure_draft_action_target(&controller, &expected_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    controller.state.set_workspace_input(text);
    let selected = controller.browse_workspace_dialog();
    controller.drain_runtime_messages();
    if let Err(conflict) = ensure_draft_action_target(&controller, &expected_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Some(path) = selected {
        controller.state.set_workspace_input(path.to_string());
    }
    controller
        .next_web_state()
        .map_err(DesktopCommandError::internal)
}

#[tauri::command]
async fn open_workspace_folder(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(
        controller,
        DesktopController::open_current_workspace_in_file_manager,
    )
    .await
}

#[tauri::command]
async fn open_global_config_folder(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::open_global_config_folder).await
}

#[tauri::command]
async fn import_global_config_toml(
    controller: State<'_, SharedController>,
    draft_values: Vec<DesktopConfigValueInput>,
    expected_target: DesktopConfigMutationTarget,
) -> Result<(DesktopWebState, bool), DesktopCommandError> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if let Err(conflict) = ensure_config_mutation_target(&controller, &expected_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) = ensure_external_config_owner_mutation_open(&controller, &draft_values) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    let selected = controller.pick_global_config_toml_dialog();
    controller.drain_runtime_messages();
    if let Err(conflict) = ensure_config_mutation_target(&controller, &expected_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    let imported = selected
        .as_deref()
        .is_some_and(|path| controller.import_global_config_toml_path(path));
    controller.drain_runtime_messages();
    Ok((
        controller
            .next_web_state()
            .map_err(DesktopCommandError::internal)?,
        imported,
    ))
}

#[tauri::command]
async fn open_typed_path(
    controller: State<'_, SharedController>,
    text: String,
    expected_target: DesktopDraftActionTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_draft_action_target(controller, &expected_target)?;
        controller.state.set_workspace_input(text);
        if !controller.open_typed_path_in_file_manager() {
            return Err(rejected_action(controller, "the path was not opened"));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn open_artifact_folder(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_stable_view_admission(controller, "artifact folder open")?;
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Artifact,
            index,
        )?;
        controller.state.select_artifact(index);
        controller.open_selected_artifact_folder();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn set_local_search(
    controller: State<'_, SharedController>,
    text: String,
    expected_target: DesktopDraftActionTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_draft_action_target(controller, &expected_target)?;
        controller.state.set_local_search_text(text);
        Ok(())
    })
    .await
}

#[tauri::command]
async fn insert_command(
    controller: State<'_, SharedController>,
    index: usize,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Command,
            index,
        )?;
        controller.state.insert_command_from_palette(index);
        Ok(())
    })
    .await
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopProviderActionInput {
    base_url: String,
    metadata_mode: String,
    context_window: String,
    max_output_tokens: String,
    selected_model_id: String,
}

impl std::fmt::Debug for DesktopProviderActionInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DesktopProviderActionInput")
            .field(
                "base_url",
                &crate::config::sanitize_provider_endpoint(&self.base_url),
            )
            .field("metadata_mode", &self.metadata_mode)
            .field("context_window", &self.context_window)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("selected_model_id", &self.selected_model_id)
            .finish()
    }
}

fn accept_provider_action_input(
    controller: &mut DesktopController,
    mut input: DesktopProviderActionInput,
) -> Result<(), DesktopCommandConflict> {
    input.base_url = match crate::config::ProviderEndpoint::parse(&input.base_url) {
        Ok(endpoint) => endpoint.as_str().to_string(),
        Err(error) => {
            controller.state.set_status_message(error.to_string());
            return Err(rejected_action(
                controller,
                "the provider endpoint is invalid",
            ));
        }
    };
    let metadata_mode = match input.metadata_mode.as_str() {
        "lm_studio_native_required" | "lm-studio-native-required" | "lm_studio" | "lm-studio" => {
            crate::config::ProviderMetadataMode::LmStudioNativeRequired
        }
        "openai_compatible_only" | "openai-compatible-only" | "openai" => {
            crate::config::ProviderMetadataMode::OpenAiCompatibleOnly
        }
        _ => {
            controller.state.set_status_message(format!(
                "unknown provider metadata mode: {}",
                input.metadata_mode
            ));
            return Err(rejected_action(
                controller,
                "the provider metadata mode is invalid",
            ));
        }
    };
    controller.accept_provider_action_input(
        input.base_url,
        metadata_mode,
        input.context_window,
        input.max_output_tokens,
        input.selected_model_id,
    );
    Ok(())
}

#[tauri::command]
async fn load_provider_models(
    controller: State<'_, SharedController>,
    input: DesktopProviderActionInput,
    expected_target: DesktopConfigMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_config_mutation_target(controller, &expected_target)?;
        if controller.provider_model_load_pending() {
            return Err(DesktopCommandConflict::new(
                "provider model load is already in progress",
            ));
        }
        accept_provider_action_input(controller, input)?;
        if !controller.load_provider_models() {
            return Err(rejected_action(
                controller,
                "the provider model list was not loaded",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn apply_provider_session(
    controller: State<'_, SharedController>,
    input: DesktopProviderActionInput,
    draft_values: Vec<DesktopConfigValueInput>,
    expected_target: DesktopConfigMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_config_mutation_target(controller, &expected_target)?;
        ensure_external_config_owner_mutation_open(controller, &draft_values)?;
        accept_provider_action_input(controller, input)?;
        if !controller.apply_provider_session() {
            return Err(rejected_action(
                controller,
                "the provider settings were not applied",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn save_provider_global(
    controller: State<'_, SharedController>,
    input: DesktopProviderActionInput,
    draft_values: Vec<DesktopConfigValueInput>,
    expected_target: DesktopConfigMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_config_mutation_target(controller, &expected_target)?;
        ensure_external_config_owner_mutation_open(controller, &draft_values)?;
        accept_provider_action_input(controller, input)?;
        if !controller.save_provider_global() {
            return Err(rejected_action(
                controller,
                "the provider settings were not saved",
            ));
        }
        Ok(())
    })
    .await
}

#[derive(Debug, Clone, serde::Deserialize)]
struct DesktopConfigValueInput {
    key: String,
    text: String,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopConfigMutationTarget {
    workspace_path: String,
    session_id: Option<String>,
    config_generation: String,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopAccessModeMutationTarget {
    workspace_path: String,
    session_id: Option<String>,
    config_generation: String,
    access_mode: crate::config::AccessMode,
    runtime_owner_token: String,
}

fn validate_config_mutation_target(
    expected: &DesktopConfigMutationTarget,
    workspace_path: &str,
    session_id: Option<String>,
    config_generation: u64,
) -> Result<(), DesktopCommandConflict> {
    if expected.workspace_path != workspace_path
        || expected.session_id != session_id
        || expected.config_generation != config_generation.to_string()
    {
        return Err(DesktopCommandConflict::new(
            "configuration owner changed before the mutation was applied; reopen settings and try again",
        ));
    }
    Ok(())
}

fn validate_complete_config_draft(
    controller: &DesktopController,
    values: &[DesktopConfigValueInput],
) -> Result<bool, DesktopCommandConflict> {
    complete_config_draft_is_dirty(&controller.state.provider_config.effective_config, values)
}

fn complete_config_draft_is_dirty(
    effective_config: &crate::config::ResolvedConfig,
    values: &[DesktopConfigValueInput],
) -> Result<bool, DesktopCommandConflict> {
    if values.len() != crate::tui::config_editor::ConfigField::ALL.len() {
        return Err(DesktopCommandConflict::new(
            "the complete settings draft must accompany the configuration owner target",
        ));
    }
    let editor = crate::tui::config_editor::ConfigEditorState::from_config_values(
        effective_config,
        values
            .iter()
            .map(|value| (value.key.clone(), value.text.clone()))
            .collect(),
    )
    .map_err(DesktopCommandConflict::new)?;
    Ok(editor.fields.iter().any(|field| field.dirty))
}

fn ensure_external_config_owner_mutation_open(
    controller: &DesktopController,
    draft_values: &[DesktopConfigValueInput],
) -> Result<(), DesktopCommandConflict> {
    if validate_complete_config_draft(controller, draft_values)? {
        return Err(DesktopCommandConflict::new(
            "finish or discard the current settings draft before changing configuration from another surface",
        ));
    }
    Ok(())
}

fn ensure_config_mutation_target(
    controller: &DesktopController,
    expected: &DesktopConfigMutationTarget,
) -> Result<(), DesktopCommandConflict> {
    validate_config_mutation_target(
        expected,
        controller.app.workspace.root.as_str(),
        controller
            .state
            .app_state
            .current_session_id
            .map(|session_id| session_id.to_string()),
        controller.state.provider_config.config_generation,
    )
}

fn ensure_config_draft_commit_admission(
    controller: &DesktopController,
) -> Result<(), DesktopCommandConflict> {
    if !controller.config_draft_mutation_admission_open() {
        return Err(DesktopCommandConflict::new(
            "configuration cannot be committed while a run, Agent Tree, navigation, or owner mutation is active",
        ));
    }
    Ok(())
}

fn validate_access_mode_mutation_target(
    expected: &DesktopAccessModeMutationTarget,
    workspace_path: &str,
    session_id: Option<String>,
    config_generation: u64,
    access_mode: crate::config::AccessMode,
    runtime_owner_token: String,
) -> Result<(), DesktopCommandConflict> {
    if expected.workspace_path != workspace_path
        || expected.session_id != session_id
        || expected.config_generation != config_generation.to_string()
        || expected.access_mode != access_mode
        || expected.runtime_owner_token != runtime_owner_token
    {
        return Err(DesktopCommandConflict::new(
            "access-mode owner changed before the mutation was applied; review the current chat and try again",
        ));
    }
    Ok(())
}

fn ensure_access_mode_mutation_target(
    controller: &DesktopController,
    expected: &DesktopAccessModeMutationTarget,
    draft_values: &[DesktopConfigValueInput],
) -> Result<(), DesktopCommandConflict> {
    let (runtime_owner_token, admission_open) = controller.access_mode_mutation_runtime_contract();
    validate_access_mode_mutation_target(
        expected,
        controller.app.workspace.root.as_str(),
        controller
            .state
            .app_state
            .current_session_id
            .map(|session_id| session_id.to_string()),
        controller.state.provider_config.config_generation,
        controller
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode,
        runtime_owner_token,
    )?;
    ensure_external_config_owner_mutation_open(controller, draft_values)?;
    if !admission_open {
        return Err(DesktopCommandConflict::new(
            "access mode cannot change while navigation or an owner mutation is active",
        ));
    }
    Ok(())
}

#[tauri::command]
async fn reset_config_draft(
    controller: State<'_, SharedController>,
    values: Vec<DesktopConfigValueInput>,
    expected_target: DesktopConfigMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if let Err(conflict) = ensure_config_mutation_target(&controller, &expected_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) = validate_complete_config_draft(&controller, &values) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    controller
        .next_web_state()
        .map_err(DesktopCommandError::internal)
}

#[tauri::command]
async fn apply_session_config(
    controller: State<'_, SharedController>,
    values: Vec<DesktopConfigValueInput>,
    expected_target: DesktopConfigMutationTarget,
) -> Result<(DesktopWebState, bool), DesktopCommandError> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if let Err(conflict) = ensure_config_mutation_target(&controller, &expected_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) = ensure_config_draft_commit_admission(&controller) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) = validate_complete_config_draft(&controller, &values) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    let applied = controller.apply_session_config(
        values
            .into_iter()
            .map(|value| (value.key, value.text))
            .collect(),
    );
    controller.drain_runtime_messages();
    Ok((
        controller
            .next_web_state()
            .map_err(DesktopCommandError::internal)?,
        applied,
    ))
}

#[tauri::command]
async fn save_global_config(
    controller: State<'_, SharedController>,
    values: Vec<DesktopConfigValueInput>,
    expected_target: DesktopConfigMutationTarget,
) -> Result<(DesktopWebState, bool), DesktopCommandError> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if let Err(conflict) = ensure_config_mutation_target(&controller, &expected_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) = ensure_config_draft_commit_admission(&controller) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) = validate_complete_config_draft(&controller, &values) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    let saved = controller.save_global_config(
        values
            .into_iter()
            .map(|value| (value.key, value.text))
            .collect(),
    );
    controller.drain_runtime_messages();
    Ok((
        controller
            .next_web_state()
            .map_err(DesktopCommandError::internal)?,
        saved,
    ))
}

#[tauri::command]
async fn toggle_access_mode(
    controller: State<'_, SharedController>,
    draft_values: Vec<DesktopConfigValueInput>,
    expected_target: DesktopAccessModeMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_access_mode_mutation_target(controller, &expected_target, &draft_values)?;
        let expected_session_id = expected_target.session_id.clone();
        if !controller.toggle_access_mode_remembered() {
            return Err(rejected_action(
                controller,
                "the access mode was not changed",
            ));
        }
        let committed_session_id = controller
            .state
            .app_state
            .current_session_id
            .map(|session_id| session_id.to_string());
        if committed_session_id != expected_session_id {
            return Err(DesktopCommandConflict::new(
                "the current root session changed before the access mode commit completed",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn preview_window_opacity(
    window: tauri::WebviewWindow,
    controller: State<'_, SharedController>,
    percent: i32,
) -> Result<(), String> {
    let mut controller = controller.lock().await;
    controller.state.set_window_opacity_percent(percent);
    apply_native_window_opacity(&window, controller.state.view.window_opacity_percent)
}

#[tauri::command]
async fn set_window_opacity(
    window: tauri::WebviewWindow,
    controller: State<'_, SharedController>,
    percent: i32,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.set_window_opacity_percent(percent);
    controller.drain_runtime_messages();
    apply_native_window_opacity(&window, controller.state.view.window_opacity_percent)?;
    controller.next_web_state()
}

fn apply_native_window_opacity(window: &tauri::WebviewWindow, percent: i32) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        apply_windows_window_opacity(window, percent)?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (window, percent);
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn apply_windows_window_opacity(window: &tauri::WebviewWindow, percent: i32) -> Result<(), String> {
    let hwnd = window
        .hwnd()
        .map_err(|error| format!("failed to get native window handle: {error}"))?;
    let hwnd = hwnd.0 as *mut core::ffi::c_void;
    let opacity = percent.clamp(
        super::state::MIN_WINDOW_OPACITY_PERCENT,
        super::state::MAX_WINDOW_OPACITY_PERCENT,
    );
    let alpha = ((opacity as f64 / 100.0) * 255.0).round().clamp(0.0, 255.0) as u8;
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_LAYERED);
        if SetLayeredWindowAttributes(hwnd, 0, alpha, LWA_ALPHA) == 0 {
            return Err("failed to apply native window opacity".to_string());
        }
    }
    Ok(())
}

#[tauri::command]
async fn answer_permission(
    controller: State<'_, SharedController>,
    decision: ReviewDecision,
    confirmation_id: String,
) -> Result<DesktopWebState, DesktopCommandError> {
    let confirmation_id = parse_permission_confirmation_id(&confirmation_id)
        .map_err(DesktopCommandError::internal)?;
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    match controller.answer_permission(confirmation_id, decision) {
        PendingPermissionResolution::Resolved => {
            controller.drain_runtime_messages();
            controller
                .next_web_state()
                .map_err(DesktopCommandError::internal)
        }
        PendingPermissionResolution::NotCurrent => Err(command_conflict_error(
            &mut controller,
            DesktopCommandConflict::new("the permission confirmation is no longer current"),
        )),
        PendingPermissionResolution::AlreadyTerminal(cause) => {
            let message = crate::tui::state::run_cancellation_status_message(&cause);
            let state = controller
                .next_web_state()
                .map_err(DesktopCommandError::internal)?;
            Err(DesktopCommandError {
                kind: "conflict",
                category: DesktopCommandErrorCategory::Unknown,
                code: DesktopCommandErrorCode::Unknown,
                message,
                state: Some(state),
            })
        }
        PendingPermissionResolution::AlreadySettled => Err(command_conflict_error(
            &mut controller,
            DesktopCommandConflict::new("the permission confirmation was already settled"),
        )),
        PendingPermissionResolution::Failed(cause) => {
            let message = crate::tui::state::run_cancellation_status_message(&cause);
            let state = controller
                .next_web_state()
                .map_err(DesktopCommandError::internal)?;
            Err(DesktopCommandError::internal_with_typed_state(
                DesktopCommandErrorCategory::Runtime,
                DesktopCommandErrorCode::RuntimeFailure,
                message,
                state,
            ))
        }
    }
}

fn parse_permission_confirmation_id(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| "permission confirmation id must be an unsigned decimal integer".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_action_target_rejects_workspace_and_current_session_drift() {
        let expected = DesktopDraftActionTarget {
            workspace_path: "C:/workspace".to_string(),
            session_id: Some("session-a".to_string()),
            owner_generation: 7,
        };
        assert!(
            validate_draft_action_target(
                &expected,
                "C:/workspace",
                Some("session-a".to_string()),
                7,
            )
            .is_ok()
        );
        assert!(
            validate_draft_action_target(&expected, "C:/other", Some("session-a".to_string()), 7,)
                .is_err()
        );
        assert!(
            validate_draft_action_target(
                &expected,
                "C:/workspace",
                Some("session-b".to_string()),
                7,
            )
            .is_err()
        );
        assert!(
            validate_draft_action_target(
                &expected,
                "C:/workspace",
                Some("session-a".to_string()),
                8,
            )
            .is_err()
        );
    }

    #[test]
    fn stop_target_rejects_workspace_session_and_runtime_owner_drift() {
        let expected = DesktopRunMutationTarget {
            workspace_path: "C:/workspace".to_string(),
            session_id: Some("session-a".to_string()),
            runtime_owner_token: "root:11".to_string(),
        };
        assert!(
            validate_run_mutation_target(
                &expected,
                "C:/workspace",
                Some("session-a".to_string()),
                "root:11".to_string(),
            )
            .is_ok()
        );
        for (workspace, session_id, runtime_owner_token) in [
            (
                "C:/other",
                Some("session-a".to_string()),
                "root:11".to_string(),
            ),
            (
                "C:/workspace",
                Some("session-b".to_string()),
                "root:11".to_string(),
            ),
            (
                "C:/workspace",
                Some("session-a".to_string()),
                "root:12".to_string(),
            ),
        ] {
            assert!(
                validate_run_mutation_target(
                    &expected,
                    workspace,
                    session_id,
                    runtime_owner_token,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn session_search_target_rejects_workspace_and_project_drift() {
        let expected = DesktopSessionSearchTarget {
            workspace_path: "C:/workspace".to_string(),
            project_id: Some("project-a".to_string()),
        };
        assert!(
            validate_session_search_target(
                &expected,
                "C:/workspace",
                Some("project-a".to_string()),
            )
            .is_ok()
        );
        assert!(
            validate_session_search_target(&expected, "C:/other", Some("project-a".to_string()),)
                .is_err()
        );
        assert!(
            validate_session_search_target(
                &expected,
                "C:/workspace",
                Some("project-b".to_string()),
            )
            .is_err()
        );
    }

    #[test]
    fn config_mutation_target_rejects_workspace_session_and_generation_changes() {
        let expected = DesktopConfigMutationTarget {
            workspace_path: "C:/workspace".to_string(),
            session_id: Some("session-a".to_string()),
            config_generation: "7".to_string(),
        };

        assert!(
            validate_config_mutation_target(
                &expected,
                "C:/workspace",
                Some("session-a".to_string()),
                7,
            )
            .is_ok()
        );
        assert!(
            validate_config_mutation_target(
                &expected,
                "C:/other",
                Some("session-a".to_string()),
                7,
            )
            .is_err()
        );
        assert!(
            validate_config_mutation_target(
                &expected,
                "C:/workspace",
                Some("session-b".to_string()),
                7,
            )
            .is_err()
        );
        assert!(
            validate_config_mutation_target(
                &expected,
                "C:/workspace",
                Some("session-a".to_string()),
                8,
            )
            .is_err()
        );
        assert!(
            validate_config_mutation_target(
                &expected,
                "C:/workspace",
                Some("session-a".to_string()),
                9,
            )
            .is_err(),
            "returning to the same workspace/session must not admit an older generation"
        );
    }

    #[test]
    fn config_generation_round_trips_as_an_exact_decimal_string() {
        const GENERATION: u64 = 9_007_199_254_740_993;
        let projection = super::super::web_model::DesktopConfigMutationTargetProjection {
            workspace_path: "C:/workspace".to_string(),
            session_id: Some("session-a".to_string()),
            config_generation: GENERATION.to_string(),
        };
        let json = serde_json::to_value(projection).expect("serialize config target");
        assert_eq!(
            json.get("configGeneration")
                .and_then(serde_json::Value::as_str),
            Some("9007199254740993")
        );

        let expected: DesktopConfigMutationTarget =
            serde_json::from_value(json).expect("deserialize exact config target");
        assert!(
            validate_config_mutation_target(
                &expected,
                "C:/workspace",
                Some("session-a".to_string()),
                GENERATION,
            )
            .is_ok()
        );
        assert!(
            validate_config_mutation_target(
                &expected,
                "C:/workspace",
                Some("session-a".to_string()),
                GENERATION + 1,
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<DesktopConfigMutationTarget>(serde_json::json!({
                "workspacePath": "C:/workspace",
                "sessionId": "session-a",
                "configGeneration": GENERATION,
            }))
            .is_err(),
            "JSON numbers must not cross the Rust/TypeScript generation boundary"
        );
    }

    #[test]
    fn full_config_draft_is_compared_without_creating_a_rust_draft_owner() {
        let config = crate::config::ResolvedConfig::default();
        let editor = crate::tui::config_editor::ConfigEditorState::from_config(&config);
        let mut values = editor
            .fields
            .iter()
            .map(|field| DesktopConfigValueInput {
                key: field.key.label().to_string(),
                text: field.value.clone(),
            })
            .collect::<Vec<_>>();

        assert!(!complete_config_draft_is_dirty(&config, &values).expect("clean full draft"));
        values
            .iter_mut()
            .find(|value| value.key == "model.model")
            .expect("model field")
            .text = "locally-edited-model".to_string();
        assert!(complete_config_draft_is_dirty(&config, &values).expect("dirty full draft"));
        values.pop();
        assert!(complete_config_draft_is_dirty(&config, &values).is_err());
    }

    #[test]
    fn access_mode_target_rejects_owner_generation_and_mode_drift() {
        let expected = DesktopAccessModeMutationTarget {
            workspace_path: "C:/workspace".to_string(),
            session_id: Some("session-a".to_string()),
            config_generation: "7".to_string(),
            access_mode: crate::config::AccessMode::Default,
            runtime_owner_token: "root:11".to_string(),
        };

        assert!(
            validate_access_mode_mutation_target(
                &expected,
                "C:/workspace",
                Some("session-a".to_string()),
                7,
                crate::config::AccessMode::Default,
                "root:11".to_string(),
            )
            .is_ok()
        );
        for (workspace, session, generation, access_mode, runtime_owner_token) in [
            (
                "C:/other",
                Some("session-a".to_string()),
                7,
                crate::config::AccessMode::Default,
                "root:11".to_string(),
            ),
            (
                "C:/workspace",
                Some("session-b".to_string()),
                7,
                crate::config::AccessMode::Default,
                "root:11".to_string(),
            ),
            (
                "C:/workspace",
                Some("session-a".to_string()),
                8,
                crate::config::AccessMode::Default,
                "root:11".to_string(),
            ),
            (
                "C:/workspace",
                Some("session-a".to_string()),
                7,
                crate::config::AccessMode::FullAccess,
                "root:11".to_string(),
            ),
            (
                "C:/workspace",
                Some("session-a".to_string()),
                7,
                crate::config::AccessMode::Default,
                "tree:11".to_string(),
            ),
        ] {
            assert!(
                validate_access_mode_mutation_target(
                    &expected,
                    workspace,
                    session,
                    generation,
                    access_mode,
                    runtime_owner_token,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn row_mutation_target_rejects_stale_owner_and_reused_index() {
        let expected = DesktopRowMutationTarget {
            workspace_path: "C:/workspace".to_string(),
            owner_project_id: Some("project-a".to_string()),
            owner_session_id: Some("session-a".to_string()),
            row_id: "session-b".to_string(),
        };

        assert!(
            validate_row_mutation_target(
                &expected,
                "C:/workspace",
                Some("project-a".to_string()),
                Some("session-a".to_string()),
                Some("session-b"),
            )
            .is_ok()
        );
        assert!(
            validate_row_mutation_target(
                &expected,
                "C:/workspace",
                Some("project-a".to_string()),
                Some("session-a".to_string()),
                Some("session-c"),
            )
            .is_err(),
            "the same index must not authorize a replacement row"
        );
        assert!(
            validate_row_mutation_target(
                &expected,
                "C:/workspace",
                Some("project-a".to_string()),
                Some("session-new".to_string()),
                Some("session-b"),
            )
            .is_err(),
            "a stale row must not cross a session-owner barrier"
        );
        assert!(
            validate_row_mutation_target(
                &expected,
                "C:/other",
                Some("project-a".to_string()),
                Some("session-a".to_string()),
                Some("session-b"),
            )
            .is_err(),
            "a stale row must not cross a workspace-owner barrier"
        );
    }

    #[test]
    fn turn_page_target_rejects_a_reordered_page_command() {
        assert!(validate_turn_page_offset(40, 40).is_ok());
        assert!(validate_turn_page_offset(40, 60).is_err());
    }

    #[test]
    fn archive_command_rejects_active_projection_before_dispatch() {
        assert!(
            validate_session_archive_loaded_status(crate::session::LoadedSessionStatus::Active)
                .is_err()
        );
        for status in [
            crate::session::LoadedSessionStatus::Idle,
            crate::session::LoadedSessionStatus::NotLoaded,
            crate::session::LoadedSessionStatus::SystemError,
        ] {
            assert!(validate_session_archive_loaded_status(status).is_ok());
        }
    }

    #[test]
    fn permission_confirmation_id_parses_full_u64_decimal_range() {
        assert_eq!(
            parse_permission_confirmation_id("18446744073709551615"),
            Ok(u64::MAX)
        );
        assert!(parse_permission_confirmation_id("9007199254740993.0").is_err());
    }

    #[test]
    fn permission_decision_uses_the_snake_case_tauri_contract() {
        assert_eq!(
            serde_json::from_str::<ReviewDecision>(r#""approved""#).expect("approved decision"),
            ReviewDecision::Approved
        );
        assert_eq!(
            serde_json::from_str::<ReviewDecision>(r#""abort""#).expect("abort decision"),
            ReviewDecision::Abort
        );
        assert_eq!(
            serde_json::from_str::<ReviewDecision>(r#""denied""#).expect("denied decision"),
            ReviewDecision::Denied
        );
        assert!(serde_json::from_str::<ReviewDecision>("true").is_err());
    }

    #[test]
    fn command_error_contract_does_not_classify_free_form_message_text() {
        let error = DesktopCommandError::internal(
            "storage connection refused while loading model 404: access denied",
        );
        let json = serde_json::to_value(error).expect("serialize command error");
        assert_eq!(json["category"], "unknown");
        assert_eq!(json["code"], "unknown");
        assert_eq!(
            serde_json::to_string(&DesktopCommandErrorCode::ProviderTransport)
                .expect("provider code"),
            r#""provider_transport""#
        );
        assert_eq!(
            serde_json::to_string(&DesktopCommandErrorCode::ModelUnavailable).expect("model code"),
            r#""model_unavailable""#
        );
        assert_eq!(
            serde_json::to_string(&DesktopCommandErrorCode::ImageUnsupported).expect("image code"),
            r#""image_unsupported""#
        );
        assert_eq!(
            serde_json::to_string(&DesktopCommandErrorCode::PermissionPolicyDenied)
                .expect("permission code"),
            r#""permission_policy_denied""#
        );
    }
}
