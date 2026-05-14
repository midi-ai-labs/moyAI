use std::sync::Arc;

use tauri::{Manager, State, WindowEvent};
use tokio::sync::Mutex;

use crate::app::App;
use crate::error::AppRunError;

use super::app::DesktopController;
use super::args::DesktopArgs;
use super::web_model::{DesktopWebState, desktop_web_state};

type SharedController = Arc<Mutex<DesktopController>>;

pub async fn run(app: App, args: DesktopArgs) -> Result<(), AppRunError> {
    let controller = DesktopController::new(app, args).await?;
    let shared: SharedController = Arc::new(Mutex::new(controller));
    tauri::Builder::default()
        .manage(shared)
        .invoke_handler(tauri::generate_handler![
            desktop_state,
            set_prompt,
            submit_prompt,
            new_chat,
            review_uncommitted,
            enhance_prompt,
            set_review_draft,
            send_prompt_review,
            cancel_prompt_review,
            refresh_desktop,
            select_project,
            select_session,
            delete_project,
            delete_session,
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
            open_project_config_folder,
            open_global_config_folder,
            open_typed_path,
            open_artifact_folder,
            set_local_search,
            insert_command,
            set_provider_base_url,
            load_provider_models,
            select_provider_model,
            apply_provider_session,
            save_provider_project,
            save_provider_global,
            set_config_selection,
            set_config_value,
            apply_session_config,
            save_project_config,
            save_global_config,
            toggle_access_mode,
            set_window_opacity,
            answer_permission,
            exit_app
        ])
        .on_window_event(|window, event| {
            if matches!(event, WindowEvent::CloseRequested { .. }) {
                window.app_handle().exit(0);
            }
        })
        .run(tauri::generate_context!())
        .map_err(|error| AppRunError::Message(format!("tauri desktop runtime failed: {error}")))
}

#[tauri::command]
fn exit_app(app: tauri::AppHandle) {
    app.exit(0);
}

#[tauri::command]
async fn desktop_state(controller: State<'_, SharedController>) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn set_prompt(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.set_draft_prompt(text);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn submit_prompt(controller: State<'_, SharedController>) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.start_run();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn new_chat(controller: State<'_, SharedController>) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.start_quick_chat();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn review_uncommitted(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.start_review_uncommitted();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn enhance_prompt(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.start_prompt_enhance();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn set_review_draft(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.set_review_draft(text);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn send_prompt_review(
    controller: State<'_, SharedController>,
    enhanced: bool,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.send_prompt_review(enhanced);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn cancel_prompt_review(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.cancel_prompt_review();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn refresh_desktop(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.refresh_snapshot();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn select_project(
    controller: State<'_, SharedController>,
    index: usize,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.select_project(index);
    controller.open_selected_project();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn select_session(
    controller: State<'_, SharedController>,
    index: usize,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.select_session(index);
    controller.open_selected_session();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn delete_project(
    controller: State<'_, SharedController>,
    index: usize,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.select_project(index);
    controller.delete_selected_project();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn delete_session(
    controller: State<'_, SharedController>,
    index: usize,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.select_session(index);
    controller.delete_selected_session();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn select_artifact(
    controller: State<'_, SharedController>,
    index: usize,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.select_artifact(index);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn export_history_markdown(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.export_selected_history_markdown_auto();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn export_transcript_markdown(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.export_open_transcript_markdown_auto();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn set_image_input(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.set_image_attachment_input(text);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn attach_image(controller: State<'_, SharedController>) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.attach_image_from_input();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn browse_image(controller: State<'_, SharedController>) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.browse_image_dialog();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn clear_images(controller: State<'_, SharedController>) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.clear_image_attachments();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn remove_image(
    controller: State<'_, SharedController>,
    index: usize,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.remove_image_attachment(index);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn show_file_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.show_file_menu();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn show_edit_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.show_edit_menu();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn show_view_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.show_view_menu();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn show_help_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.show_help_menu();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn show_project_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.show_project_menu();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn create_project_from_picker(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.create_project_from_picker();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn show_config_editor(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.show_config_editor();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn show_provider_editor(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.show_provider_editor();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn show_workspace_picker(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    let path = controller.app.workspace.root.to_string();
    controller.state.show_workspace_picker(&path);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn show_command_palette(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.show_command_palette();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn show_shortcuts(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.show_keyboard_shortcuts();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn close_overlay(controller: State<'_, SharedController>) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.hide_overlay();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn set_workspace_input(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.set_workspace_input(text);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn switch_workspace(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.switch_workspace();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn browse_workspace(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.browse_workspace_dialog();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn open_workspace_folder(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.open_current_workspace_in_file_manager();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn open_project_config_folder(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.open_project_config_folder();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn open_global_config_folder(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.open_global_config_folder();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn open_typed_path(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.open_typed_path_in_file_manager();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn open_artifact_folder(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.open_selected_artifact_folder();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn set_local_search(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.set_local_search_text(text);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn insert_command(
    controller: State<'_, SharedController>,
    index: usize,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.insert_command_from_palette(index);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn set_provider_base_url(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.set_provider_base_url_input(text);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn load_provider_models(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.load_provider_models();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn select_provider_model(
    controller: State<'_, SharedController>,
    index: i32,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.set_provider_model_selection(index);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn apply_provider_session(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.apply_provider_session();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn save_provider_project(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.save_provider_project();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn save_provider_global(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.save_provider_global();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn set_config_selection(
    controller: State<'_, SharedController>,
    index: usize,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.set_config_selection(index);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn set_config_value(
    controller: State<'_, SharedController>,
    text: String,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.set_config_value(text);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn apply_session_config(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.apply_session_config();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn save_project_config(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.save_project_config();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn save_global_config(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.save_global_config();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn toggle_access_mode(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.toggle_access_mode_session();
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn set_window_opacity(
    controller: State<'_, SharedController>,
    percent: i32,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.state.set_window_opacity_percent(percent);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}

#[tauri::command]
async fn answer_permission(
    controller: State<'_, SharedController>,
    allow: bool,
) -> Result<DesktopWebState, String> {
    let mut controller = controller.lock().await;
    controller.answer_permission(allow);
    controller.drain_runtime_messages();
    Ok(desktop_web_state(&controller.state))
}
