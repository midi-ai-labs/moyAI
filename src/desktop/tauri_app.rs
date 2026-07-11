use std::sync::Arc;

use tauri::{
    Manager, State, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
};
use tokio::sync::Mutex;

use crate::app::App;
use crate::error::AppRunError;

use super::app::DesktopController;
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
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<DesktopWebState>,
}

impl DesktopCommandError {
    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: "internal",
            message: message.into(),
            state: None,
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
            set_prompt,
            submit_prompt,
            cancel_run,
            new_chat,
            new_project_session,
            review_uncommitted,
            enhance_prompt,
            set_review_draft,
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
            enable_session_memory,
            disable_session_memory,
            delete_project,
            delete_session,
            delete_chat_session,
            select_artifact,
            export_history_markdown,
            export_transcript_markdown,
            set_image_input,
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
            set_workspace_input,
            switch_workspace,
            browse_workspace,
            open_workspace_folder,
            open_global_config_folder,
            import_global_config_toml,
            open_typed_path,
            open_artifact_folder,
            set_local_search,
            insert_command,
            set_provider_base_url,
            set_provider_metadata_mode,
            set_provider_context_window,
            set_provider_max_output_tokens,
            load_provider_models,
            select_provider_model,
            apply_provider_session,
            save_provider_global,
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
    ProviderModel,
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
        DesktopRowCollection::ProviderModel => controller
            .state
            .provider_config
            .provider_models
            .get(index)
            .cloned(),
    };
    ensure_row_mutation_target(controller, expected, actual)
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
async fn set_prompt(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.state.set_draft_prompt(text);
    })
    .await
}

#[tauri::command]
async fn submit_prompt(controller: State<'_, SharedController>) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::start_run).await
}

#[tauri::command]
async fn cancel_run(controller: State<'_, SharedController>) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::cancel_active_run).await
}

#[tauri::command]
async fn new_chat(controller: State<'_, SharedController>) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::start_quick_chat).await
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
        controller.start_project_session(index);
        Ok(())
    })
    .await
}

#[tauri::command]
async fn review_uncommitted(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::start_review_uncommitted).await
}

#[tauri::command]
async fn enhance_prompt(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::start_prompt_enhance).await
}

#[tauri::command]
async fn set_review_draft(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.state.set_review_draft(text);
    })
    .await
}

#[tauri::command]
async fn send_prompt_review(
    controller: State<'_, SharedController>,
    enhanced: bool,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.send_prompt_review(enhanced)
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
        controller.select_project_and_open(index);
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
        controller.select_session_and_open(index);
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
        if controller.select_session_if_admitted(index) {
            controller.rejoin_selected_session();
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
        controller.open_quick_chat_session(index);
        Ok(())
    })
    .await
}

#[tauri::command]
async fn set_session_search(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.set_session_search(text);
    })
    .await
}

#[tauri::command]
async fn set_session_search_include_archived(
    controller: State<'_, SharedController>,
    include_archived: bool,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.set_session_search_include_archived(include_archived);
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
        if controller.select_session_if_admitted(index) {
            controller.archive_selected_session(true);
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
        if controller.select_session_if_admitted(index) {
            controller.archive_selected_session(false);
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
        if controller.select_session_if_admitted(index) {
            controller.rollback_selected_session();
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
        if controller.select_session_if_admitted(index) {
            controller.fork_selected_session();
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
        if controller.select_session_if_admitted(index) {
            controller.interrupt_selected_session();
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn enable_session_memory(
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
        if controller.select_session_if_admitted(index) {
            controller.set_selected_session_memory_mode(crate::session::SessionMemoryMode::Enabled);
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn disable_session_memory(
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
        if controller.select_session_if_admitted(index) {
            controller
                .set_selected_session_memory_mode(crate::session::SessionMemoryMode::Disabled);
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
        if controller.select_project_if_admitted(index) {
            controller.delete_selected_project();
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
        if controller.select_session_if_admitted(index) {
            controller.delete_selected_session();
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
        controller.delete_quick_chat_session(index);
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
        controller.export_selected_history_markdown_auto();
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
async fn set_image_input(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.state.set_image_attachment_input(text);
    })
    .await
}

#[tauri::command]
async fn attach_image(
    app: tauri::AppHandle,
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    let Some(path) = controller.prepare_image_attachment_from_input() else {
        return controller.next_web_state();
    };
    controller.authorize_attachment_asset(&app, &path)?;
    controller.state.attach_image_path(path);
    controller.drain_runtime_messages();
    controller.next_web_state()
}

#[tauri::command]
async fn browse_image(
    app: tauri::AppHandle,
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    let Some(path) = controller.browse_image_dialog() else {
        return controller.next_web_state();
    };
    controller.authorize_attachment_asset(&app, &path)?;
    controller.state.attach_image_path(path);
    controller.drain_runtime_messages();
    controller.next_web_state()
}

#[tauri::command]
async fn clear_images(controller: State<'_, SharedController>) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.state.clear_image_attachments();
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
    mutate_controller(controller, |controller| {
        let path = controller.app.workspace.root.to_string();
        controller.state.show_workspace_picker(&path);
    })
    .await
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
async fn set_workspace_input(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.state.set_workspace_input(text);
    })
    .await
}

#[tauri::command]
async fn switch_workspace(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::switch_workspace).await
}

#[tauri::command]
async fn browse_workspace(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::browse_workspace_dialog).await
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
    expected_target: DesktopConfigMutationTarget,
) -> Result<(DesktopWebState, bool), DesktopCommandError> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if let Err(conflict) = ensure_config_mutation_target(&controller, &expected_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    let imported = controller.import_global_config_toml_dialog();
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
) -> Result<DesktopWebState, String> {
    mutate_controller(
        controller,
        DesktopController::open_typed_path_in_file_manager,
    )
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
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.state.set_local_search_text(text);
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

#[tauri::command]
async fn set_provider_base_url(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.set_provider_base_url_input(text);
    })
    .await
}

#[tauri::command]
async fn set_provider_metadata_mode(
    controller: State<'_, SharedController>,
    mode: String,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        let parsed = match mode.as_str() {
            "lm_studio_native_required"
            | "lm-studio-native-required"
            | "lm_studio"
            | "lm-studio" => crate::config::ProviderMetadataMode::LmStudioNativeRequired,
            "openai_compatible_only" | "openai-compatible-only" | "openai" => {
                crate::config::ProviderMetadataMode::OpenAiCompatibleOnly
            }
            _ => {
                controller
                    .state
                    .set_status_message(format!("unknown provider metadata mode: {mode}"));
                return;
            }
        };
        controller.set_provider_metadata_mode_input(parsed);
    })
    .await
}

#[tauri::command]
async fn set_provider_context_window(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.state.set_provider_context_window_input(text);
    })
    .await
}

#[tauri::command]
async fn set_provider_max_output_tokens(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.state.set_provider_max_output_tokens_input(text);
    })
    .await
}

#[tauri::command]
async fn load_provider_models(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::load_provider_models).await
}

#[tauri::command]
async fn select_provider_model(
    controller: State<'_, SharedController>,
    index: i32,
    expected_target: DesktopRowMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        let index_usize = usize::try_from(index).map_err(|_| {
            DesktopCommandConflict::new(
                "the provider model selection is no longer available; reload the model list",
            )
        })?;
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::ProviderModel,
            index_usize,
        )?;
        controller.state.set_provider_model_selection(index);
        Ok(())
    })
    .await
}

#[tauri::command]
async fn apply_provider_session(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::apply_provider_session).await
}

#[tauri::command]
async fn save_provider_global(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::save_provider_global).await
}

#[derive(Debug, serde::Deserialize)]
struct DesktopConfigValueInput {
    key: String,
    text: String,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopConfigMutationTarget {
    workspace_path: String,
    session_id: Option<String>,
    config_generation: u64,
}

fn validate_config_mutation_target(
    expected: &DesktopConfigMutationTarget,
    workspace_path: &str,
    session_id: Option<String>,
    config_generation: u64,
) -> Result<(), DesktopCommandConflict> {
    if expected.workspace_path != workspace_path
        || expected.session_id != session_id
        || expected.config_generation != config_generation
    {
        return Err(DesktopCommandConflict::new(
            "configuration owner changed before the mutation was applied; reopen settings and try again",
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
            .selected_session_id()
            .map(|session_id| session_id.to_string()),
        controller.state.provider_config.config_generation,
    )
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
    controller
        .state
        .set_config_values_by_key(
            values
                .into_iter()
                .map(|value| (value.key, value.text))
                .collect(),
        )
        .map_err(DesktopCommandError::internal)?;
    let applied = controller.apply_session_config();
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
    controller
        .state
        .set_config_values_by_key(
            values
                .into_iter()
                .map(|value| (value.key, value.text))
                .collect(),
        )
        .map_err(DesktopCommandError::internal)?;
    let saved = controller.save_global_config();
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
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::toggle_access_mode_session).await
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
    allow: bool,
    confirmation_id: u64,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, |controller| {
        controller.answer_permission(confirmation_id, allow);
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_mutation_target_rejects_workspace_session_and_generation_changes() {
        let expected = DesktopConfigMutationTarget {
            workspace_path: "C:/workspace".to_string(),
            session_id: Some("session-a".to_string()),
            config_generation: 7,
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
}
