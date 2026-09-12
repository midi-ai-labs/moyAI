#![deny(dead_code)]

use std::sync::Arc;

use tauri::{
    Manager, State, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
};
use tokio::sync::Mutex;

use crate::app::App;
use crate::cli::ReviewDecision;
use crate::config::{ProviderEndpoint, ProviderProfile, ReasoningSummary, ResolvedConfig};
use crate::device_network::{DeviceNetworkProjection, DeviceNetworkService, SharedHubConfig};
use crate::error::AppRunError;
use crate::hub::{
    CatalogRevision, HubConnection, HubConnectionProjection, HubReviewContext, HubSelection,
    HubSettingsStore,
};
use crate::llm::{ProviderModelInfo, ProviderModelLoadState, fetch_provider_model_infos};
use crate::mcp_publish::{PublishProfileId, PublishService};
use crate::protocol::TurnId;
use crate::session::{ActiveTurnExpectation, SessionId, SessionSettingsPatch, SessionSpawnEdge};
use crate::tool::shell::ManagedShells;

use super::app::{
    DesktopController, PendingPermissionResolution, RootSessionSettingsApplyError,
    RootSessionSettingsPersistenceOutcome,
};
use super::args::DesktopArgs;
use super::models::DesktopStopMutationTarget;
use super::query::{
    DESKTOP_HISTORY_PROJECTION_LIMIT, DESKTOP_TURN_PAGE_LIMIT, build_session_detail_with_roots,
    load_latest_session_detail,
};
use super::side_chat::{SideChatQuoteRequest, SideChatQuoteSourceKind};
use super::state::{DesktopOverlay, DesktopStatusCode};
use super::web_model::{
    DesktopAgentExecutionProjection, DesktopAgentInterruptTarget, DesktopWebState,
    access_runtime_owner_terminal_settlement_matches,
};

type SharedController = Arc<Mutex<DesktopController>>;

#[derive(Default)]
struct McpHistoryExportGate(Mutex<()>);

// Keep the wire-visible Desktop command registry in one place. The handler
// and its contract tests consume this same identifier list, so a renamed or
// unannotated command fails at compile time instead of becoming a runtime-only
// IPC failure. The module-level dead-code denial also rejects a new private
// command function that was omitted from this manifest.
macro_rules! desktop_command_manifest {
    ($consumer:ident) => {
        $consumer! {
            desktop_state,
            submit_prompt,
            cancel_run,
            ensure_side_chat,
            capture_side_chat_direct_provider,
            load_side_chat_models,
            save_side_chat_draft,
            submit_side_chat,
            cancel_side_chat,
            delete_side_chat,
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
            load_agent_execution,
            load_previous_agent_execution_page,
            interrupt_agent,
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
            show_about,
            show_project_menu,
            create_project_from_picker,
            show_config_editor,
            show_hub_editor,
            hub_projection,
            hub_connect,
            hub_refresh,
            hub_save_review,
            hub_disconnect,
            hub_set_route_mode,
            show_mcp_history,
            mcp_history_list,
            mcp_history_detail,
            mcp_history_export,
            mcp_history_stop,
            remote_job_cancel,
            device_network_projection,
            device_network_import,
            device_network_initial_setup_import,
            device_network_join,
            device_network_request_join,
            device_network_receiver,
            device_network_select,
            device_network_refresh,
            device_network_leave,
            device_network_jobs,
            device_network_diagnose,
            device_network_artifacts,
            device_network_export_artifacts,
            device_network_cancel,
            mcp_peer_projection,
            mcp_peer_add,
            mcp_peer_remove,
            mcp_peer_check,
            show_session_settings,
            show_provider_editor,
            show_workspace_picker,
            show_command_palette,
            show_shortcuts,
            close_overlay,
            switch_workspace,
            browse_workspace,
            open_workspace_folder,
            open_global_config_folder,
            open_user_data_folder,
            import_global_config_toml,
            load_initial_setup_config_toml,
            open_typed_path,
            open_artifact_folder,
            set_local_search,
            insert_command,
            load_provider_models,
            check_docling_readiness,
            check_initial_setup_docling_readiness,
            apply_provider_session,
            save_provider_global,
            reset_config_draft,
            apply_session_config,
            save_global_config,
            finish_initial_setup,
            apply_session_settings,
            toggle_access_mode,
            preview_window_opacity,
            set_window_opacity,
            answer_permission,
            start_window_drag,
            minimize_window,
            is_window_maximized,
            toggle_maximize_window,
            hide_to_tray,
            exit_app,
        }
    };
}

macro_rules! generate_desktop_invoke_handler {
    ($($command:ident),+ $(,)?) => {
        tauri::generate_handler![$($command),+]
    };
}

#[cfg(test)]
macro_rules! desktop_command_wire_names {
    ($($command:ident),+ $(,)?) => {
        &[$(stringify!($command)),+]
    };
}

#[cfg(test)]
const DESKTOP_COMMAND_WIRE_NAMES: &[&str] = desktop_command_manifest!(desktop_command_wire_names);

#[derive(Debug, Clone, PartialEq, Eq)]
struct DesktopCommandConflict {
    message: String,
    status_code: DesktopStatusCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AlreadyProjectedSessionSettingsWrite {
    settings_revision: u64,
    next_config_generation: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionSettingsSettlementPolicy {
    access_only: bool,
}

impl DesktopCommandConflict {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status_code: DesktopStatusCode::Plain,
        }
    }

    fn with_status(code: DesktopStatusCode, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status_code: code,
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

    fn storage(message: impl Into<String>) -> Self {
        Self {
            kind: "internal",
            category: DesktopCommandErrorCategory::Storage,
            code: DesktopCommandErrorCode::StorageFailure,
            message: message.into(),
            state: None,
        }
    }

    fn provider_transport(message: impl Into<String>) -> Self {
        Self {
            kind: "internal",
            category: DesktopCommandErrorCategory::Provider,
            code: DesktopCommandErrorCode::ProviderTransport,
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
        .set_typed_status_message(conflict.status_code, conflict.message.clone());
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

fn read_only_command_conflict_error(conflict: DesktopCommandConflict) -> DesktopCommandError {
    DesktopCommandError {
        kind: "conflict",
        category: DesktopCommandErrorCategory::Unknown,
        code: DesktopCommandErrorCode::Unknown,
        message: conflict.message,
        state: None,
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
    let mut controller = DesktopController::new(app, args).await?;
    let hub_path = crate::config::loader::global_config_path()
        .map_err(|_| AppRunError::Message("failed to resolve Hub settings directory".into()))?
        .with_file_name("hub-settings.json");
    let hub_connection = HubConnection::new(HubSettingsStore::new(hub_path));
    controller.state.hub_connection = Some(hub_connection.clone());
    let publish_path = crate::config::loader::global_config_path()
        .map_err(|_| {
            AppRunError::Message("failed to resolve MCP publish settings directory".into())
        })?
        .with_file_name("mcp-publish.json");
    let remote_jobs =
        crate::remote_agent::RemoteJobService::new(controller.app.process_runtime.clone())
            .map_err(|error| AppRunError::Message(error.to_string()))?;
    let mcp_publish = PublishService::new(
        publish_path,
        controller.app.store.clone(),
        controller.app.config.clone(),
    )
    .with_remote_jobs(remote_jobs.clone());
    controller.state.mcp_publish = Some(mcp_publish.clone());
    let network_directory = crate::config::loader::global_config_path()
        .map_err(|_| AppRunError::Message("failed to resolve device settings directory".into()))?
        .with_file_name("device-network");
    let device_network = DeviceNetworkService::new(
        network_directory,
        controller.app.store.clone(),
        controller.state.global_config().clone(),
        remote_jobs.clone(),
        mcp_publish.clone(),
    );
    device_network.attach_hub_connection(hub_connection.clone());
    controller.state.device_network = Some(device_network.clone());
    let managed_shells = controller.app.process_runtime.managed_shells();
    let shared: SharedController = Arc::new(Mutex::new(controller));
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            restore_main_window(app);
        }))
        .manage(shared)
        .manage(hub_connection)
        .manage(mcp_publish)
        .manage(remote_jobs)
        .manage(McpHistoryExportGate::default())
        .manage(device_network)
        .manage(managed_shells.clone())
        .setup(|app| {
            install_tray(app.handle())?;
            let network = app.state::<DeviceNetworkService>().inner().clone();
            tauri::async_runtime::spawn(async move {
                let _ = network.resume().await;
            });
            Ok(())
        })
        .invoke_handler(desktop_command_manifest!(generate_desktop_invoke_handler))
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                disconnect_hub(window.app_handle(), false);
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!());
    managed_shells.shutdown().await;
    result.map_err(|error| AppRunError::Message(format!("tauri desktop runtime failed: {error}")))
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
            "quit_moyai" => disconnect_hub(app, true),
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
    if let Some(network) = app.try_state::<DeviceNetworkService>() {
        network.window_shown();
    }
    if let Some(publish) = app.try_state::<PublishService>() {
        publish.window_shown();
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[tauri::command]
fn hide_to_tray(app: tauri::AppHandle) {
    disconnect_hub(&app, false);
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
    disconnect_hub(&app, true);
}

fn disconnect_hub(app: &tauri::AppHandle, exit: bool) {
    let connection = app.state::<HubConnection>().inner().clone();
    let publish = app.state::<PublishService>().inner().clone();
    let network = app.state::<DeviceNetworkService>().inner().clone();
    let managed_shells = app.state::<ManagedShells>().inner().clone();
    let hidden_receiver = if exit {
        managed_shells.begin_shutdown();
        network.begin_shutdown();
        false
    } else {
        network.window_hide_requested()
    };
    let hidden_profiles = if exit {
        publish.begin_shutdown();
        Vec::new()
    } else {
        publish.window_hide_requested()
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        connection.shutdown().await;
        if exit {
            network.shutdown().await;
            // Keep ownership until in-flight tools and listener tasks have settled.
            while !publish.shutdown().await {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            managed_shells.shutdown().await;
            app.exit(0);
        } else {
            network.finish_window_hide(hidden_receiver).await;
            publish.finish_window_hide(hidden_profiles).await;
        }
    });
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

async fn mutate_side_chat_controller_checked<F>(
    controller: State<'_, SharedController>,
    owner_session_id: String,
    action: F,
) -> Result<DesktopWebState, DesktopCommandError>
where
    F: FnOnce(&mut DesktopController) -> Result<(), DesktopCommandConflict>,
{
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if let Err(conflict) = action(&mut controller) {
        if let Ok(owner_session_id) = owner_session_id.parse::<SessionId>() {
            controller.set_side_chat_command_error(owner_session_id, conflict.message.clone());
        }
        let state = controller
            .next_web_state()
            .map_err(DesktopCommandError::internal)?;
        return Err(DesktopCommandError {
            kind: "conflict",
            category: DesktopCommandErrorCategory::Unknown,
            code: DesktopCommandErrorCode::Unknown,
            message: conflict.message,
            state: Some(state),
        });
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
    owner_generation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DesktopPromptReviewMutationTarget {
    workspace_path: String,
    session_id: Option<String>,
    owner_generation: String,
    request_id: String,
    expected_state: DesktopRunExpectedState,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DesktopRunExpectedState {
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

impl DesktopRunExpectedState {
    fn parse(&self) -> Result<ActiveTurnExpectation, DesktopCommandConflict> {
        let parse_turn_id = |value: &str| {
            value
                .parse::<TurnId>()
                .ok()
                .filter(|turn_id| turn_id.to_string() == value)
                .ok_or_else(|| {
                    DesktopCommandConflict::new(
                        "the active turn identity is invalid; refresh the current task and try again",
                    )
                })
        };
        let parse_revision = |value: &str| {
            value
                .parse::<u64>()
                .ok()
                .filter(|revision| revision.to_string() == value)
                .ok_or_else(|| {
                    DesktopCommandConflict::new(
                        "the admission revision is invalid; refresh the current task and try again",
                    )
                })
        };
        match self {
            Self::Idle {
                latest_turn_id,
                admission_revision,
            } => Ok(ActiveTurnExpectation::Idle {
                latest_turn_id: latest_turn_id.as_deref().map(parse_turn_id).transpose()?,
                revision: parse_revision(admission_revision)?,
            }),
            Self::Turn {
                turn_id,
                admission_revision,
            } => Ok(ActiveTurnExpectation::Turn {
                turn_id: parse_turn_id(turn_id)?,
                revision: parse_revision(admission_revision)?,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DesktopRunMutationTarget {
    workspace_path: String,
    session_id: Option<String>,
    runtime_owner_token: String,
    permission_confirmation_id: Option<String>,
    expected_state: DesktopRunExpectedState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DesktopStopAdmission {
    Root {
        generation: u64,
    },
    Turn {
        turn_id: TurnId,
        admission_revision: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopSessionSearchTarget {
    workspace_path: String,
    project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DesktopAgentExecutionTarget {
    workspace_path: String,
    root_session_id: String,
    agent_path: String,
    child_session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentExecutionReadRequest {
    Latest,
    Previous { expected_end: usize, offset: usize },
}

#[derive(Debug)]
enum AgentExecutionReadFailure {
    Storage(String),
    Conflict(String),
}

fn previous_agent_execution_page_request(
    expected_offset: usize,
    expected_end: usize,
) -> Result<AgentExecutionReadRequest, DesktopCommandConflict> {
    if expected_offset == 0 || expected_end <= expected_offset {
        return Err(DesktopCommandConflict::new(
            "the complete Sub Agent execution history is already loaded",
        ));
    }
    let limit = expected_offset.min(DESKTOP_TURN_PAGE_LIMIT);
    let offset = expected_offset - limit;
    Ok(AgentExecutionReadRequest::Previous {
        expected_end,
        offset,
    })
}

fn validate_previous_agent_execution_page(
    requested_offset: usize,
    expected_end: usize,
    actual_offset: usize,
    item_count: usize,
    total: usize,
) -> Result<(), DesktopCommandConflict> {
    let actual_end = actual_offset.checked_add(item_count);
    if actual_offset != requested_offset || actual_end != Some(expected_end) || total < expected_end
    {
        return Err(DesktopCommandConflict::new(
            "the Sub Agent execution history changed before the previous page was loaded; retry from the current history",
        ));
    }
    Ok(())
}

fn validate_agent_execution_snapshot_end(
    requested_offset: usize,
    expected_end: usize,
    snapshot_end: usize,
) -> Result<(), DesktopCommandConflict> {
    if snapshot_end != expected_end || snapshot_end <= requested_offset {
        return Err(DesktopCommandConflict::new(
            "the Sub Agent execution history changed before the previous page was loaded; retry from the current history",
        ));
    }
    Ok(())
}

fn agent_execution_has_previous(offset: usize, end: usize) -> bool {
    offset > 0 && end > offset
}

fn validate_agent_execution_owner(
    expected: &DesktopAgentExecutionTarget,
    workspace_path: &str,
    current_root_session_id: Option<SessionId>,
) -> Result<(SessionId, SessionId), DesktopCommandConflict> {
    let root_session_id = expected.root_session_id.parse::<SessionId>().map_err(|_| {
        DesktopCommandConflict::new(
            "the requested Sub Agent root is invalid; review the current task and try again",
        )
    })?;
    let child_session_id = expected
        .child_session_id
        .parse::<SessionId>()
        .map_err(|_| {
            DesktopCommandConflict::new(
                "the requested Sub Agent session is invalid; review the current task and try again",
            )
        })?;
    if expected.workspace_path != workspace_path || current_root_session_id != Some(root_session_id)
    {
        return Err(DesktopCommandConflict::new(
            "the Sub Agent owner changed before its execution history was loaded; review the current task and try again",
        ));
    }
    Ok((root_session_id, child_session_id))
}

fn validate_agent_execution_edge(
    expected: &DesktopAgentExecutionTarget,
    root_session_id: SessionId,
    child_session_id: SessionId,
    edge: Option<&SessionSpawnEdge>,
) -> Result<(), DesktopCommandConflict> {
    let matches_target = edge.is_some_and(|edge| {
        edge.root_session_id == root_session_id
            && edge.child_session_id == child_session_id
            && edge.agent_path == expected.agent_path
    });
    if !matches_target {
        return Err(DesktopCommandConflict::new(
            "the requested Sub Agent no longer belongs to the current task; refresh the activity list and try again",
        ));
    }
    Ok(())
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
        controller.app.workspace.authority_root().as_str(),
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
        controller.app.workspace.authority_root().as_str(),
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
    let expected_owner_generation = parse_canonical_owner_generation(&expected.owner_generation)?;
    if expected.workspace_path != workspace_path
        || expected.session_id != session_id
        || expected_owner_generation != owner_generation
    {
        return Err(DesktopCommandConflict::new(
            "the request draft owner changed before the action was applied; review the current chat and try again",
        ));
    }
    Ok(())
}

fn ensure_prompt_review_mutation_target(
    controller: &DesktopController,
    expected: &DesktopPromptReviewMutationTarget,
) -> Result<u64, DesktopCommandConflict> {
    validate_prompt_review_mutation_target(
        expected,
        controller.app.workspace.authority_root().as_str(),
        controller
            .state
            .app_state
            .current_session_id
            .map(|session_id| session_id.to_string()),
        controller.state.composer.owner_generation(),
        controller
            .state
            .app_state
            .prompt_review
            .as_ref()
            .map(|review| review.request_id),
        controller
            .state
            .app_state
            .prompt_review
            .as_ref()
            .and_then(|review| {
                controller
                    .state
                    .prompt_review_expected_active_turn(review.request_id)
            }),
    )
}

fn validate_prompt_review_mutation_target(
    expected: &DesktopPromptReviewMutationTarget,
    workspace_path: &str,
    session_id: Option<String>,
    owner_generation: u64,
    request_id: Option<u64>,
    expected_active_turn: Option<ActiveTurnExpectation>,
) -> Result<u64, DesktopCommandConflict> {
    let expected_owner_generation = parse_canonical_owner_generation(&expected.owner_generation)?;
    let expected_request_id = expected.request_id.parse::<u64>().map_err(|_| {
        DesktopCommandConflict::new(
            "the prompt review request identity is invalid; refresh the current review and try again",
        )
    })?;
    let parsed_expected_state = expected.expected_state.parse()?;
    if expected_request_id.to_string() != expected.request_id
        || expected.workspace_path != workspace_path
        || expected.session_id != session_id
        || expected_owner_generation != owner_generation
        || request_id != Some(expected_request_id)
        || expected_active_turn != Some(parsed_expected_state)
    {
        return Err(DesktopCommandConflict::new(
            "the prompt review owner changed before the action was applied; review the current prompt and try again",
        ));
    }
    Ok(expected_request_id)
}

fn parse_canonical_owner_generation(value: &str) -> Result<u64, DesktopCommandConflict> {
    value
        .parse::<u64>()
        .ok()
        .filter(|generation| generation.to_string() == value)
        .ok_or_else(|| {
            DesktopCommandConflict::new(
                "the request draft owner generation is invalid; refresh the current chat and try again",
            )
        })
}

fn validate_unscoped_overlay_close(overlay: DesktopOverlay) -> Result<(), DesktopCommandConflict> {
    if overlay == DesktopOverlay::PromptReview {
        return Err(DesktopCommandConflict::new(
            "Prompt Review must be closed through its current request target",
        ));
    }
    Ok(())
}

fn ensure_unscoped_prompt_review_action(
    controller: &mut DesktopController,
    target: &str,
) -> Result<(), DesktopCommandConflict> {
    if controller.ensure_unscoped_prompt_review_action(target) {
        Ok(())
    } else {
        Err(DesktopCommandConflict::new(format!(
            "{target} cannot replace the active Prompt Review"
        )))
    }
}

fn ensure_attachment_state_mutation(admitted: bool) -> Result<(), DesktopCommandConflict> {
    if admitted {
        Ok(())
    } else {
        Err(DesktopCommandConflict::new(
            "image attachment cannot change while Prompt Review owns the composer draft",
        ))
    }
}

fn validate_run_mutation_target(
    expected: &DesktopRunMutationTarget,
    workspace_path: &str,
    session_id: Option<String>,
    runtime_owner_token: String,
    permission_confirmation_id: Option<String>,
    active_turn_expectation: ActiveTurnExpectation,
) -> Result<ActiveTurnExpectation, DesktopCommandConflict> {
    let parsed_expected_state = expected.expected_state.parse()?;
    if expected.workspace_path != workspace_path
        || expected.session_id != session_id
        || expected.runtime_owner_token != runtime_owner_token
        || expected.permission_confirmation_id != permission_confirmation_id
        || parsed_expected_state != active_turn_expectation
    {
        return Err(DesktopCommandConflict::new(
            "the active run owner changed before the action was applied; review the current task and try again",
        ));
    }
    Ok(parsed_expected_state)
}

fn ensure_run_mutation_target(
    controller: &DesktopController,
    expected: &DesktopRunMutationTarget,
) -> Result<ActiveTurnExpectation, DesktopCommandConflict> {
    let (runtime_owner_token, _) = controller.access_mode_mutation_runtime_contract();
    validate_run_mutation_target(
        expected,
        controller.app.workspace.authority_root().as_str(),
        controller
            .state
            .app_state
            .current_session_id
            .map(|session_id| session_id.to_string()),
        runtime_owner_token,
        controller
            .pending_permission_confirmation_id()
            .map(|confirmation_id| confirmation_id.to_string()),
        controller.current_active_turn_expectation(),
    )
}

async fn ensure_durable_idle_composer_preflight(
    controller: &SharedController,
    expected_target: &DesktopDraftActionTarget,
    expected_run_target: &DesktopRunMutationTarget,
    action: &str,
) -> Result<(), DesktopCommandError> {
    let (session_service, session_id, expected_active_turn) = {
        let mut controller = controller.lock().await;
        controller.drain_runtime_messages();
        let validated = (|| {
            ensure_draft_action_target(&controller, expected_target)?;
            let expected_active_turn =
                ensure_run_mutation_target(&controller, expected_run_target)?;
            if !matches!(expected_active_turn, ActiveTurnExpectation::Idle { .. }) {
                return Err(DesktopCommandConflict::new(format!(
                    "{action} requires the captured idle session owner"
                )));
            }
            Ok(expected_active_turn)
        })();
        let expected_active_turn = match validated {
            Ok(expected) => expected,
            Err(conflict) => return Err(command_conflict_error(&mut controller, conflict)),
        };
        (
            controller.app.session_service.clone(),
            controller.state.app_state.current_session_id,
            expected_active_turn,
        )
    };
    let actual = match session_id {
        Some(session_id) => session_service
            .active_turn_expectation_for_session(session_id)
            .await
            .map_err(|error| DesktopCommandError::storage(error.to_string()))?,
        None => None,
    };
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    let local_validation = (|| {
        ensure_draft_action_target(&controller, expected_target)?;
        let current = ensure_run_mutation_target(&controller, expected_run_target)?;
        if current != expected_active_turn {
            return Err(DesktopCommandConflict::new(format!(
                "{action} owner changed during durable validation"
            )));
        }
        if session_id.is_some() && actual != Some(expected_active_turn) {
            return Err(DesktopCommandConflict::new(format!(
                "the durable session owner changed before {action} started"
            )));
        }
        Ok(())
    })();
    match local_validation {
        Ok(()) => Ok(()),
        Err(conflict) => Err(command_conflict_error(&mut controller, conflict)),
    }
}

fn validate_stop_mutation_target(
    expected: &DesktopStopMutationTarget,
    workspace_path: &str,
    session_id: Option<String>,
    root_run_generation: Option<u64>,
    last_root_run_epoch: u64,
    permission_confirmation_id: Option<String>,
    active_turn_expectation: ActiveTurnExpectation,
    root_admission_snapshot: Option<crate::runtime::RootAdmissionSnapshot>,
) -> Result<DesktopStopAdmission, DesktopCommandConflict> {
    let conflict = || {
        DesktopCommandConflict::new(
            "the active Stop owner changed before cancellation was applied; review the current task and try again",
        )
    };
    let parse_u64 = |value: &str| {
        value
            .parse::<u64>()
            .ok()
            .filter(|parsed| parsed.to_string() == value)
            .ok_or_else(conflict)
    };
    let parse_turn = |value: &str| {
        value
            .parse::<TurnId>()
            .ok()
            .filter(|parsed| parsed.to_string() == value)
            .ok_or_else(conflict)
    };

    match expected {
        DesktopStopMutationTarget::Root {
            workspace_path: expected_workspace,
            session_id: expected_session,
            root_generation,
            latest_turn_id,
            admission_revision,
            permission_confirmation_id: expected_permission,
        } => {
            let generation = parse_u64(root_generation)?;
            let captured_idle = ActiveTurnExpectation::Idle {
                latest_turn_id: latest_turn_id.as_deref().map(parse_turn).transpose()?,
                revision: parse_u64(admission_revision)?,
            };
            if expected_workspace != workspace_path
                || root_run_generation != Some(generation)
                || expected_permission != &permission_confirmation_id
            {
                return Err(conflict());
            }
            let canonical_owner_session = root_admission_snapshot.and_then(|snapshot| {
                snapshot
                    .pending
                    .map(|pending| pending.session_id)
                    .or_else(|| snapshot.last_admitted.map(|receipt| receipt.session_id))
            });
            if let Some(admitted_session_id) = canonical_owner_session {
                let admitted_session = admitted_session_id.to_string();
                let session_matches = match expected_session.as_deref() {
                    Some(captured_session) => {
                        captured_session == admitted_session
                            && session_id.as_deref() == Some(captured_session)
                    }
                    None => session_id
                        .as_deref()
                        .is_none_or(|session| session == admitted_session),
                };
                if !session_matches {
                    return Err(conflict());
                }
                return Ok(DesktopStopAdmission::Root { generation });
            }
            if active_turn_expectation == captured_idle && expected_session == &session_id {
                return Ok(DesktopStopAdmission::Root { generation });
            }
            Err(conflict())
        }
        DesktopStopMutationTarget::Turn {
            workspace_path: expected_workspace,
            session_id: expected_session,
            turn_id,
            admission_revision,
            root_epoch,
        } => {
            let turn_id = parse_turn(turn_id)?;
            let admission_revision = parse_u64(admission_revision)?;
            let root_epoch = parse_u64(root_epoch)?;
            if expected_workspace != workspace_path
                || session_id.as_deref() != Some(expected_session.as_str())
            {
                return Err(conflict());
            }
            if root_run_generation == Some(root_epoch) {
                return Ok(DesktopStopAdmission::Root {
                    generation: root_epoch,
                });
            }
            let current_epoch = root_run_generation.unwrap_or(last_root_run_epoch);
            let turn_matches = match active_turn_expectation {
                ActiveTurnExpectation::Turn {
                    turn_id: actual,
                    revision,
                }
                | ActiveTurnExpectation::Idle {
                    latest_turn_id: Some(actual),
                    revision,
                } => actual == turn_id && revision == admission_revision,
                ActiveTurnExpectation::Idle {
                    latest_turn_id: None,
                    ..
                } => false,
            };
            if current_epoch != root_epoch || !turn_matches {
                return Err(conflict());
            }
            Ok(DesktopStopAdmission::Turn {
                turn_id,
                admission_revision,
            })
        }
    }
}

fn ensure_stop_mutation_target(
    controller: &DesktopController,
    expected: &DesktopStopMutationTarget,
) -> Result<DesktopStopAdmission, DesktopCommandConflict> {
    let root_run_generation = controller.root_run_generation();
    let root_admission_snapshot = root_run_generation
        .and_then(|generation| controller.root_run_admission_snapshot(generation));
    validate_stop_mutation_target(
        expected,
        controller.app.workspace.authority_root().as_str(),
        controller
            .state
            .app_state
            .current_session_id
            .map(|session_id| session_id.to_string()),
        root_run_generation,
        controller.last_root_run_epoch(),
        controller
            .pending_permission_confirmation_id()
            .map(|confirmation_id| confirmation_id.to_string()),
        controller.current_active_turn_expectation(),
        root_admission_snapshot,
    )
}

impl DesktopAgentInterruptTarget {
    fn execution_target(&self) -> Result<DesktopAgentExecutionTarget, DesktopCommandConflict> {
        let parse_session_id = |value: &str| {
            value
                .parse::<SessionId>()
                .ok()
                .filter(|session_id| session_id.to_string() == value)
                .ok_or_else(|| {
                    DesktopCommandConflict::new(
                        "the requested Sub Agent session identity is invalid; refresh the activity list and try again",
                    )
                })
        };
        parse_session_id(&self.root_session_id)?;
        parse_session_id(&self.child_session_id)?;
        Ok(DesktopAgentExecutionTarget {
            workspace_path: self.workspace_path.clone(),
            root_session_id: self.root_session_id.clone(),
            agent_path: self.agent_path.clone(),
            child_session_id: self.child_session_id.clone(),
        })
    }

    fn expected_turn_id(&self) -> Result<TurnId, DesktopCommandConflict> {
        self.expected_turn_id
            .parse::<TurnId>()
            .ok()
            .filter(|turn_id| turn_id.to_string() == self.expected_turn_id)
            .ok_or_else(|| {
                DesktopCommandConflict::new(
                    "the requested Sub Agent turn is invalid; refresh the activity list and try again",
                )
            })
    }

    fn admission_revision(&self) -> Result<u64, DesktopCommandConflict> {
        self.admission_revision
            .parse::<u64>()
            .ok()
            .filter(|revision| revision.to_string() == self.admission_revision)
            .ok_or_else(|| {
                DesktopCommandConflict::new(
                    "the requested Sub Agent admission revision is invalid; refresh the activity list and try again",
                )
            })
    }
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

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopCommandPaletteInsertionResult {
    state: DesktopWebState,
    insertion_text: String,
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
    if controller.state.snapshot.workspace_path
        != controller.app.workspace.authority_root().as_str()
    {
        return Err(DesktopCommandConflict::new(
            "the workspace projection changed before the operation was applied; refresh and try again",
        ));
    }
    validate_row_mutation_target(
        expected,
        controller.app.workspace.authority_root().as_str(),
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
    expected_run_target: DesktopRunMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_draft_action_target(controller, &expected_target)?;
        let expected_active_turn = ensure_run_mutation_target(controller, &expected_run_target)?;
        if !controller.start_run_at(text, expected_active_turn) {
            return Err(rejected_action(controller, "the prompt was not submitted"));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn cancel_run(
    controller: State<'_, SharedController>,
    expected_target: DesktopStopMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        let admission = ensure_stop_mutation_target(controller, &expected_target)?;
        apply_stop_admission(controller, admission)
    })
    .await
}

fn apply_stop_admission(
    controller: &mut DesktopController,
    admission: DesktopStopAdmission,
) -> Result<(), DesktopCommandConflict> {
    match admission {
        DesktopStopAdmission::Root { generation } => {
            if !controller.cancel_root_run_at_generation(generation) {
                return Err(rejected_action(
                    controller,
                    "the captured root task was not stopped",
                ));
            }
        }
        DesktopStopAdmission::Turn {
            turn_id,
            admission_revision,
        } => {
            controller.cancel_exact_turn_at(turn_id, admission_revision);
        }
    }
    Ok(())
}

#[tauri::command]
async fn ensure_side_chat(
    controller: State<'_, SharedController>,
    owner_session_id: String,
    expected_config_generation: String,
) -> Result<DesktopWebState, DesktopCommandError> {
    let conflict_owner = owner_session_id.clone();
    mutate_side_chat_controller_checked(controller, conflict_owner, |controller| {
        let owner_session_id = owner_session_id.parse::<SessionId>().map_err(|error| {
            DesktopCommandConflict::new(format!("invalid side chat owner: {error}"))
        })?;
        validate_side_chat_config_owner(
            owner_session_id,
            &expected_config_generation,
            controller.state.app_state.current_session_id,
            controller.state.provider_config.config_generation,
        )?;
        controller
            .ensure_side_chat(owner_session_id)
            .map_err(DesktopCommandConflict::new)
    })
    .await
}

#[tauri::command]
async fn capture_side_chat_direct_provider(
    controller: State<'_, SharedController>,
    owner_session_id: String,
    side_chat_id: String,
    expected_generation: String,
    expected_draft_revision: String,
    expected_config_generation: String,
) -> Result<DesktopWebState, DesktopCommandError> {
    let conflict_owner = owner_session_id.clone();
    mutate_side_chat_controller_checked(controller, conflict_owner, |controller| {
        let owner_session_id = owner_session_id.parse::<SessionId>().map_err(|error| {
            DesktopCommandConflict::new(format!("invalid side chat owner: {error}"))
        })?;
        validate_side_chat_config_owner(
            owner_session_id,
            &expected_config_generation,
            controller.state.app_state.current_session_id,
            controller.state.provider_config.config_generation,
        )?;
        let side_chat_id = side_chat_id
            .parse::<crate::storage::SideChatId>()
            .map_err(|error| {
                DesktopCommandConflict::new(format!("invalid side chat id: {error}"))
            })?;
        let expected_generation = parse_canonical_owner_generation(&expected_generation)?;
        let expected_draft_revision = parse_canonical_owner_generation(&expected_draft_revision)?;
        controller
            .capture_side_chat_direct_provider(
                owner_session_id,
                side_chat_id,
                expected_generation,
                expected_draft_revision,
            )
            .map_err(DesktopCommandConflict::new)
    })
    .await
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SideChatCatalogRequestTarget {
    base_url: String,
    provider_profile: ProviderProfile,
    config_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SideChatCatalogModelProjection {
    id: String,
    label: String,
    load_state: ProviderModelLoadState,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SideChatCatalogProjection {
    base_url: String,
    provider_profile: String,
    config_generation: String,
    models: Vec<SideChatCatalogModelProjection>,
}

#[tauri::command]
async fn load_side_chat_models(
    controller: State<'_, SharedController>,
    base_url: String,
    provider_profile: String,
    expected_config_generation: String,
) -> Result<SideChatCatalogProjection, DesktopCommandError> {
    let (target, probe_config) = {
        let mut controller = controller.lock().await;
        controller.drain_runtime_messages();
        validate_side_chat_global_config_target(
            &expected_config_generation,
            controller.state.provider_config.config_generation,
        )
        .map_err(read_only_command_conflict_error)?;
        let provider_profile = parse_provider_profile_input(&provider_profile)
            .map_err(read_only_command_conflict_error)?;
        let (probe_config, canonical_base_url) = side_chat_catalog_probe_config(
            controller.state.provider_config.effective_config.clone(),
            &base_url,
            provider_profile,
        )
        .map_err(read_only_command_conflict_error)?;
        let target = SideChatCatalogRequestTarget {
            base_url: canonical_base_url,
            provider_profile,
            config_generation: controller.state.provider_config.config_generation,
        };
        (target, probe_config)
    };

    // Provider I/O deliberately runs without the controller mutex. The exact
    // global config generation is checked again before any result is returned.
    let result = fetch_provider_model_infos(&probe_config, &target.base_url).await;

    {
        let mut controller = controller.lock().await;
        controller.drain_runtime_messages();
        validate_side_chat_catalog_target(
            &target,
            controller.state.provider_config.config_generation,
        )
        .map_err(read_only_command_conflict_error)?;
    }

    let models = result
        .map_err(|error| DesktopCommandError::provider_transport(error.to_string()))?
        .into_iter()
        .map(side_chat_catalog_model_projection)
        .collect();
    Ok(SideChatCatalogProjection {
        base_url: target.base_url,
        provider_profile: target.provider_profile.as_str().to_string(),
        config_generation: target.config_generation.to_string(),
        models,
    })
}

fn validate_side_chat_config_owner(
    expected_owner_session_id: SessionId,
    expected_config_generation: &str,
    current_owner_session_id: Option<SessionId>,
    current_config_generation: u64,
) -> Result<(), DesktopCommandConflict> {
    if current_owner_session_id != Some(expected_owner_session_id) {
        return Err(DesktopCommandConflict::new(
            "the selected main session changed before the side chat operation",
        ));
    }
    if expected_config_generation != current_config_generation.to_string() {
        return Err(DesktopCommandConflict::new(
            "configuration changed before the side chat operation; retry from current settings",
        ));
    }
    Ok(())
}

fn validate_side_chat_catalog_target(
    target: &SideChatCatalogRequestTarget,
    current_config_generation: u64,
) -> Result<(), DesktopCommandConflict> {
    validate_side_chat_global_config_target(
        &target.config_generation.to_string(),
        current_config_generation,
    )
}

fn validate_side_chat_global_config_target(
    expected_config_generation: &str,
    current_config_generation: u64,
) -> Result<(), DesktopCommandConflict> {
    if expected_config_generation != current_config_generation.to_string() {
        return Err(DesktopCommandConflict::new(
            "configuration changed before the side chat operation; retry from current settings",
        ));
    }
    Ok(())
}

fn parse_provider_profile_input(value: &str) -> Result<ProviderProfile, DesktopCommandConflict> {
    ProviderProfile::parse(value).ok_or_else(|| {
        DesktopCommandConflict::new(format!(
            "unsupported connection type `{}`",
            value.trim().to_ascii_lowercase()
        ))
    })
}

fn side_chat_catalog_probe_config(
    mut config: ResolvedConfig,
    base_url: &str,
    provider_profile: ProviderProfile,
) -> Result<(ResolvedConfig, String), DesktopCommandConflict> {
    let canonical_base_url = ProviderEndpoint::parse(base_url)
        .map_err(|error| DesktopCommandConflict::new(error.to_string()))?
        .catalog_root()
        .as_str()
        .to_string();
    config.model.base_url = canonical_base_url.clone();
    config.model.provider_profile = provider_profile;
    config.model.request_timeout_ms = config.side_chat.request_timeout_ms;
    config.model.connect_timeout_ms = config.side_chat.connect_timeout_ms;
    config.model.max_retries = config.side_chat.max_retries;
    config.model.context_window = config.side_chat.context_window;
    config.model.system_prompt.clear();
    config.model.supports_tools = false;
    config.model.supports_images = false;
    config.model.parallel_tool_calls = false;

    // Side Chat has no credential surface and must never inherit the main
    // provider's authentication material or generation-body customization.
    config.model.api_key_env = None;
    config.model.extra_headers.clear();
    config.model.chat_completions_reasoning_parameters = None;
    config.model.reasoning_effort = None;
    config.model.reasoning_summary = ReasoningSummary::None;
    config.model.temperature = None;
    config.model.top_p = None;
    config.model.top_k = None;
    config.model.presence_penalty = None;
    config.model.frequency_penalty = None;
    config.model.seed = None;
    config.model.stop_sequences.clear();
    config.model.extra_body_json = None;
    Ok((config, canonical_base_url))
}

fn side_chat_catalog_model_projection(info: ProviderModelInfo) -> SideChatCatalogModelProjection {
    let summary = super::state::provider_model_summary(&info);
    let label = if summary.is_empty() {
        info.id.clone()
    } else {
        format!("{}  [{}]", info.id, summary)
    };
    SideChatCatalogModelProjection {
        id: info.id,
        label,
        load_state: info.load_state,
    }
}

#[tauri::command]
async fn save_side_chat_draft(
    controller: State<'_, SharedController>,
    owner_session_id: String,
    chat_id: String,
    expected_draft_revision: String,
    text: String,
    quote: Option<SideChatQuoteInput>,
) -> Result<DesktopWebState, DesktopCommandError> {
    let conflict_owner = owner_session_id.clone();
    mutate_side_chat_controller_checked(controller, conflict_owner, |controller| {
        let owner_session_id = owner_session_id.parse::<SessionId>().map_err(|error| {
            DesktopCommandConflict::new(format!("invalid side chat owner: {error}"))
        })?;
        let side_chat_id = chat_id
            .parse::<crate::storage::SideChatId>()
            .map_err(|error| {
                DesktopCommandConflict::new(format!("invalid side chat id: {error}"))
            })?;
        let expected_draft_revision = expected_draft_revision.parse::<u64>().map_err(|error| {
            DesktopCommandConflict::new(format!("invalid side chat draft revision: {error}"))
        })?;
        let quote = quote.map(parse_side_chat_quote).transpose()?;
        controller
            .save_side_chat_draft(
                owner_session_id,
                side_chat_id,
                expected_draft_revision,
                text,
                quote,
            )
            .map_err(DesktopCommandConflict::new)
    })
    .await
}

#[tauri::command]
async fn submit_side_chat(
    controller: State<'_, SharedController>,
    owner_session_id: String,
    chat_id: String,
    expected_generation: String,
    expected_draft_revision: String,
    expected_owner_append_position: Option<String>,
    quote: Option<SideChatQuoteInput>,
    text: String,
) -> Result<DesktopWebState, DesktopCommandError> {
    let conflict_owner = owner_session_id.clone();
    mutate_side_chat_controller_checked(controller, conflict_owner, |controller| {
        let (owner_session_id, side_chat_id, expected_generation) =
            parse_side_chat_target(&owner_session_id, &chat_id, &expected_generation)?;
        let expected_draft_revision = expected_draft_revision.parse::<u64>().map_err(|error| {
            DesktopCommandConflict::new(format!("invalid side chat draft revision: {error}"))
        })?;
        let expected_owner_append_position = parse_side_chat_append_position(
            expected_owner_append_position.as_deref(),
            "owner context",
        )?;
        let quote = quote.map(parse_side_chat_quote).transpose()?;
        controller
            .start_side_chat(
                owner_session_id,
                side_chat_id,
                expected_generation,
                expected_draft_revision,
                expected_owner_append_position,
                quote,
                text,
            )
            .map_err(DesktopCommandConflict::new)
    })
    .await
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SideChatQuoteInput {
    source_kind: String,
    source_history_item_id: String,
    source_append_position: Option<String>,
    selected_text: String,
}

fn parse_side_chat_quote(
    quote: SideChatQuoteInput,
) -> Result<SideChatQuoteRequest, DesktopCommandConflict> {
    let source_kind =
        SideChatQuoteSourceKind::parse(&quote.source_kind).map_err(DesktopCommandConflict::new)?;
    let source_history_item_id = quote
        .source_history_item_id
        .parse::<crate::protocol::HistoryItemId>()
        .map_err(|error| {
            DesktopCommandConflict::new(format!("invalid side chat quote source: {error}"))
        })?;
    let source_append_position =
        parse_side_chat_append_position(quote.source_append_position.as_deref(), "quote source")?;
    Ok(SideChatQuoteRequest {
        source_kind,
        source_history_item_id,
        source_append_position,
        selected_text: quote.selected_text,
    })
}

fn parse_side_chat_append_position(
    value: Option<&str>,
    label: &str,
) -> Result<Option<i64>, DesktopCommandConflict> {
    let Some(value) = value else {
        return Ok(None);
    };
    let position = value.parse::<i64>().map_err(|error| {
        DesktopCommandConflict::new(format!("invalid side chat {label} revision: {error}"))
    })?;
    if position < 0 || position.to_string() != value {
        return Err(DesktopCommandConflict::new(format!(
            "invalid side chat {label} revision"
        )));
    }
    Ok(Some(position))
}

#[tauri::command]
async fn cancel_side_chat(
    controller: State<'_, SharedController>,
    owner_session_id: String,
    chat_id: String,
    expected_generation: String,
) -> Result<DesktopWebState, DesktopCommandError> {
    let conflict_owner = owner_session_id.clone();
    mutate_side_chat_controller_checked(controller, conflict_owner, |controller| {
        let (owner_session_id, side_chat_id, expected_generation) =
            parse_side_chat_target(&owner_session_id, &chat_id, &expected_generation)?;
        controller
            .cancel_side_chat(owner_session_id, side_chat_id, expected_generation)
            .map_err(DesktopCommandConflict::new)
    })
    .await
}

#[tauri::command]
async fn delete_side_chat(
    controller: State<'_, SharedController>,
    owner_session_id: String,
    chat_id: String,
    expected_generation: String,
) -> Result<DesktopWebState, DesktopCommandError> {
    let conflict_owner = owner_session_id.clone();
    mutate_side_chat_controller_checked(controller, conflict_owner, |controller| {
        let (owner_session_id, side_chat_id, expected_generation) =
            parse_side_chat_target(&owner_session_id, &chat_id, &expected_generation)?;
        controller
            .delete_side_chat(owner_session_id, side_chat_id, expected_generation)
            .map_err(DesktopCommandConflict::new)
    })
    .await
}

fn parse_side_chat_target(
    owner_session_id: &str,
    side_chat_id: &str,
    expected_generation: &str,
) -> Result<(SessionId, crate::storage::SideChatId, u64), DesktopCommandConflict> {
    let owner_session_id = owner_session_id.parse::<SessionId>().map_err(|error| {
        DesktopCommandConflict::new(format!("invalid side chat owner: {error}"))
    })?;
    let side_chat_id = side_chat_id
        .parse::<crate::storage::SideChatId>()
        .map_err(|error| DesktopCommandConflict::new(format!("invalid side chat id: {error}")))?;
    let expected_generation = expected_generation.parse::<u64>().map_err(|error| {
        DesktopCommandConflict::new(format!("invalid side chat generation: {error}"))
    })?;
    Ok((owner_session_id, side_chat_id, expected_generation))
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
    expected_run_target: DesktopRunMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    ensure_durable_idle_composer_preflight(
        controller.inner(),
        &expected_target,
        &expected_run_target,
        "uncommitted review",
    )
    .await?;
    mutate_controller_checked(controller, |controller| {
        ensure_draft_action_target(controller, &expected_target)?;
        let expected_active_turn = ensure_run_mutation_target(controller, &expected_run_target)?;
        if !controller.start_review_uncommitted_at(text, expected_active_turn) {
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
    expected_run_target: DesktopRunMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    ensure_durable_idle_composer_preflight(
        controller.inner(),
        &expected_target,
        &expected_run_target,
        "prompt enhancement",
    )
    .await?;
    mutate_controller_checked(controller, |controller| {
        ensure_draft_action_target(controller, &expected_target)?;
        let expected_active_turn = ensure_run_mutation_target(controller, &expected_run_target)?;
        if !controller.start_prompt_enhance_at(text, expected_active_turn) {
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
    expected_target: DesktopPromptReviewMutationTarget,
    expected_run_target: DesktopRunMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        let request_id = ensure_prompt_review_mutation_target(controller, &expected_target)?;
        let expected_active_turn = ensure_run_mutation_target(controller, &expected_run_target)?;
        if !controller.send_prompt_review_at(request_id, enhanced, text, expected_active_turn) {
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
    expected_target: DesktopPromptReviewMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        let request_id = ensure_prompt_review_mutation_target(controller, &expected_target)?;
        if !controller
            .state
            .cancel_prompt_review_if_current(request_id)
        {
            return Err(DesktopCommandConflict::new(
                "the prompt review changed before cancellation; review the current prompt and try again",
            ));
        }
        Ok(())
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
async fn load_agent_execution(
    controller: State<'_, SharedController>,
    expected_target: DesktopAgentExecutionTarget,
) -> Result<DesktopAgentExecutionProjection, DesktopCommandError> {
    load_agent_execution_projection(
        controller.inner(),
        expected_target,
        AgentExecutionReadRequest::Latest,
    )
    .await
}

#[tauri::command]
async fn load_previous_agent_execution_page(
    controller: State<'_, SharedController>,
    expected_target: DesktopAgentExecutionTarget,
    expected_offset: usize,
    expected_end: usize,
) -> Result<DesktopAgentExecutionProjection, DesktopCommandError> {
    let read_request = previous_agent_execution_page_request(expected_offset, expected_end)
        .map_err(read_only_command_conflict_error)?;
    load_agent_execution_projection(controller.inner(), expected_target, read_request).await
}

#[tauri::command]
async fn interrupt_agent(
    controller: State<'_, SharedController>,
    expected_target: DesktopAgentInterruptTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    let execution_target = expected_target
        .execution_target()
        .map_err(read_only_command_conflict_error)?;
    let expected_turn_id = expected_target
        .expected_turn_id()
        .map_err(read_only_command_conflict_error)?;
    let expected_admission_revision = expected_target
        .admission_revision()
        .map_err(read_only_command_conflict_error)?;
    let (app, root_session_id, child_session_id) = {
        let mut controller = controller.lock().await;
        controller.drain_runtime_messages();
        let (root_session_id, child_session_id) = validate_agent_execution_owner(
            &execution_target,
            controller.app.workspace.authority_root().as_str(),
            controller.state.app_state.current_session_id,
        )
        .map_err(|conflict| command_conflict_error(&mut controller, conflict))?;
        (controller.app.clone(), root_session_id, child_session_id)
    };

    let edge = load_agent_execution_edge(app.clone(), child_session_id).await?;
    validate_agent_execution_edge(
        &execution_target,
        root_session_id,
        child_session_id,
        edge.as_ref(),
    )
    .map_err(read_only_command_conflict_error)?;

    {
        let mut controller = controller.lock().await;
        controller.drain_runtime_messages();
        validate_agent_execution_owner(
            &execution_target,
            controller.app.workspace.authority_root().as_str(),
            controller.state.app_state.current_session_id,
        )
        .map_err(|conflict| command_conflict_error(&mut controller, conflict))?;
    }

    let accepted = app
        .run_service
        .interrupt_agent_turn_exact(
            root_session_id,
            &execution_target.agent_path,
            child_session_id,
            expected_turn_id,
            expected_admission_revision,
        )
        .await
        .map_err(|error| {
            DesktopCommandError::storage(format!(
                "failed to interrupt the exact Sub Agent turn: {error}"
            ))
        })?;

    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if !accepted {
        return Err(command_conflict_error(
            &mut controller,
            DesktopCommandConflict::new(
                "the Sub Agent turn changed before interrupt was applied; refresh the activity list and try again",
            ),
        ));
    }
    controller
        .next_web_state()
        .map_err(DesktopCommandError::internal)
}

async fn load_agent_execution_projection(
    controller: &SharedController,
    expected_target: DesktopAgentExecutionTarget,
    read_request: AgentExecutionReadRequest,
) -> Result<DesktopAgentExecutionProjection, DesktopCommandError> {
    let (app, root_session_id, child_session_id) = {
        let mut controller = controller.lock().await;
        controller.drain_runtime_messages();
        let (root_session_id, child_session_id) = validate_agent_execution_owner(
            &expected_target,
            controller.app.workspace.authority_root().as_str(),
            controller.state.app_state.current_session_id,
        )
        .map_err(read_only_command_conflict_error)?;
        (controller.app.clone(), root_session_id, child_session_id)
    };

    let edge = load_agent_execution_edge(app.clone(), child_session_id).await?;
    if let Err(edge_conflict) = validate_agent_execution_edge(
        &expected_target,
        root_session_id,
        child_session_id,
        edge.as_ref(),
    ) {
        let mut controller = controller.lock().await;
        controller.drain_runtime_messages();
        if let Err(owner_conflict) = validate_agent_execution_owner(
            &expected_target,
            controller.app.workspace.authority_root().as_str(),
            controller.state.app_state.current_session_id,
        ) {
            return Err(read_only_command_conflict_error(owner_conflict));
        }
        return Err(read_only_command_conflict_error(edge_conflict));
    }

    let detail_app = app.clone();
    let read = tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                AgentExecutionReadFailure::Storage(format!(
                    "failed to start the Sub Agent read worker: {error}"
                ))
            })?;
        runtime.block_on(async move {
            match read_request {
                AgentExecutionReadRequest::Latest => {
                    load_latest_session_detail(&detail_app, child_session_id)
                        .await
                        .map(|loaded| loaded.read)
                        .map_err(|error| AgentExecutionReadFailure::Storage(error.to_string()))
                }
                AgentExecutionReadRequest::Previous {
                    expected_end,
                    offset,
                    ..
                } => {
                    let mut snapshot = detail_app
                        .session_service
                        .canonical_session_snapshot(
                            child_session_id,
                            0,
                            DESKTOP_HISTORY_PROJECTION_LIMIT,
                            offset,
                            DESKTOP_TURN_PAGE_LIMIT,
                        )
                        .await
                        .map_err(|error| AgentExecutionReadFailure::Storage(error.to_string()))?;
                    let fence = snapshot.fence;
                    let target_end = fence.turn_count;
                    validate_agent_execution_snapshot_end(offset, expected_end, target_end)
                        .map_err(|conflict| {
                            AgentExecutionReadFailure::Conflict(conflict.message)
                        })?;
                    let mut next_offset = offset;
                    let mut combined = snapshot.read;
                    let first_page_len = combined.turns.items.len();
                    if combined.turns.offset != next_offset || first_page_len == 0 {
                        return Err(AgentExecutionReadFailure::Conflict(
                            "the Sub Agent execution history page is no longer contiguous".to_string(),
                        ));
                    }
                    next_offset = next_offset.saturating_add(first_page_len);
                    while next_offset < target_end {
                        let page_limit = (target_end - next_offset).min(DESKTOP_TURN_PAGE_LIMIT);
                        snapshot = detail_app
                            .session_service
                            .canonical_session_snapshot(
                                child_session_id,
                                0,
                                DESKTOP_HISTORY_PROJECTION_LIMIT,
                                next_offset,
                                page_limit,
                            )
                            .await
                            .map_err(|error| {
                                AgentExecutionReadFailure::Storage(error.to_string())
                            })?;
                        if snapshot.fence != fence || snapshot.read.turns.offset != next_offset {
                            return Err(AgentExecutionReadFailure::Conflict(
                                "the Sub Agent execution history changed while its previous page was loading; retry from the current history".to_string(),
                            ));
                        }
                        let mut page = snapshot.read;
                        let page_len = page.turns.items.len();
                        if page_len == 0 {
                            return Err(AgentExecutionReadFailure::Conflict(
                                "the Sub Agent execution history page is no longer contiguous".to_string(),
                            ));
                        }
                        combined.session = page.session;
                        combined.latest_turn_id = page.latest_turn_id;
                        combined.active_turn_id = page.active_turn_id;
                        combined.active_turn_sequence_no = page.active_turn_sequence_no;
                        combined.turns.items.append(&mut page.turns.items);
                        next_offset = next_offset.saturating_add(page_len);
                    }

                    let final_snapshot = detail_app
                        .session_service
                        .canonical_latest_session_snapshot(
                            child_session_id,
                            DESKTOP_HISTORY_PROJECTION_LIMIT,
                            1,
                        )
                        .await
                        .map_err(|error| AgentExecutionReadFailure::Storage(error.to_string()))?;
                    if final_snapshot.fence != fence || next_offset != target_end {
                        return Err(AgentExecutionReadFailure::Conflict(
                            "the Sub Agent execution history changed while its previous page was loading; retry from the current history".to_string(),
                        ));
                    }
                    combined.session = final_snapshot.read.session;
                    combined.latest_turn_id = final_snapshot.read.latest_turn_id;
                    combined.active_turn_id = final_snapshot.read.active_turn_id;
                    combined.active_turn_sequence_no = final_snapshot.read.active_turn_sequence_no;
                    combined.turns.offset = offset;
                    combined.turns.limit = target_end - offset;
                    combined.turns.total = target_end;
                    combined.turns.has_more = false;
                    Ok(combined)
                }
            }
        })
    })
    .await
    .map_err(|error| {
        DesktopCommandError::storage(format!(
            "failed to join the Sub Agent execution reader: {error}"
        ))
    })?
    .map_err(|error| match error {
        AgentExecutionReadFailure::Storage(error) => DesktopCommandError::storage(format!(
            "failed to load the Sub Agent execution history: {error}"
        )),
        AgentExecutionReadFailure::Conflict(message) => {
            read_only_command_conflict_error(DesktopCommandConflict::new(message))
        }
    })?;

    if let AgentExecutionReadRequest::Previous { offset, .. } = read_request {
        let target_end = read.turns.total;
        validate_previous_agent_execution_page(
            offset,
            target_end,
            read.turns.offset,
            read.turns.items.len(),
            read.turns.total,
        )
        .map_err(read_only_command_conflict_error)?;
    }
    let detail = build_session_detail_with_roots(
        &read,
        None,
        Some(&app.workspace.root),
        Some(app.workspace.authority_root()),
    );
    let turn_page_end = read.turns.offset.saturating_add(read.turns.items.len());

    // The detail read runs outside the controller lock. Re-read the durable edge
    // afterward so a concurrent root/child deletion cannot return an orphaned
    // child transcript under an otherwise unchanged visible owner.
    let edge = load_agent_execution_edge(app.clone(), child_session_id).await?;
    {
        let mut controller = controller.lock().await;
        controller.drain_runtime_messages();
        validate_agent_execution_owner(
            &expected_target,
            controller.app.workspace.authority_root().as_str(),
            controller.state.app_state.current_session_id,
        )
        .map_err(read_only_command_conflict_error)?;
    }
    validate_agent_execution_edge(
        &expected_target,
        root_session_id,
        child_session_id,
        edge.as_ref(),
    )
    .map_err(read_only_command_conflict_error)?;
    let edge = edge.expect("validated Sub Agent edge must be present");

    Ok(DesktopAgentExecutionProjection {
        workspace_path: app.workspace.authority_root().to_string(),
        root_session_id: root_session_id.to_string(),
        agent_path: edge.agent_path,
        session_id: child_session_id.to_string(),
        task_name: edge.task_name,
        transcript_rows: detail.transcript_rows,
        turn_page_offset: detail.turn_page_offset,
        turn_page_end,
        turn_page_total: detail.turn_page_total,
        turn_page_has_previous: agent_execution_has_previous(
            detail.turn_page_offset,
            turn_page_end,
        ),
    })
}

async fn load_agent_execution_edge(
    app: App,
    child_session_id: SessionId,
) -> Result<Option<SessionSpawnEdge>, DesktopCommandError> {
    tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("failed to start the Sub Agent read worker: {error}"))?;
        runtime.block_on(async move {
            app.store
                .session_repo()
                .session_spawn_edge_for_child(child_session_id)
                .await
                .map_err(|error| error.to_string())
        })
    })
    .await
    .map_err(|error| {
        DesktopCommandError::storage(format!(
            "failed to join the Sub Agent ownership reader: {error}"
        ))
    })?
    .map_err(|error| {
        DesktopCommandError::storage(format!(
            "failed to load the Sub Agent ownership record: {error}"
        ))
    })
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
    validate_turn_page_load_admission(controller.state.turn_page_load_pending())?;
    validate_turn_page_offset(
        expected_offset,
        controller.state.selected_detail().turn_page_offset,
    )
}

fn validate_turn_page_load_admission(pending: bool) -> Result<(), DesktopCommandConflict> {
    if pending {
        return Err(DesktopCommandConflict::new(
            "a turn page load is already active; wait for the current history request to finish",
        ));
    }
    Ok(())
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
    expected_stop_target: DesktopStopMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_indexed_row_mutation_target(
            controller,
            &expected_target,
            DesktopRowCollection::Session,
            index,
        )?;
        let session_id = validated_session_id(controller, index)?;
        let current_turn = controller
            .state
            .snapshot
            .session_rows
            .get(index)
            .and_then(|row| {
                row.active_turn_id.map(|turn_id| {
                    row.admission_revision
                        .parse::<u64>()
                        .ok()
                        .filter(|revision| revision.to_string() == row.admission_revision)
                        .map(|revision| (turn_id, revision))
                })
            })
            .flatten();
        if controller.state.app_state.current_session_id == Some(session_id) {
            let admission = ensure_stop_mutation_target(controller, &expected_stop_target)?;
            return apply_stop_admission(controller, admission);
        }
        let (expected_turn_id, expected_admission_revision) =
            validate_background_session_interrupt_target(
                &expected_stop_target,
                controller.app.workspace.authority_root().as_str(),
                session_id,
                current_turn,
            )?;
        if !controller.interrupt_session(session_id, expected_turn_id, expected_admission_revision)
        {
            return Err(rejected_action(
                controller,
                "chat interrupt was not started",
            ));
        }
        Ok(())
    })
    .await
}

fn validate_background_session_interrupt_target(
    expected: &DesktopStopMutationTarget,
    workspace_path: &str,
    session_id: SessionId,
    current_turn: Option<(TurnId, u64)>,
) -> Result<(TurnId, u64), DesktopCommandConflict> {
    let DesktopStopMutationTarget::Turn {
        workspace_path: expected_workspace,
        session_id: expected_session,
        turn_id: expected_turn_id,
        admission_revision,
        root_epoch,
    } = expected
    else {
        return Err(DesktopCommandConflict::new(
            "a background chat interrupt requires an exact turn target",
        ));
    };
    let canonical_epoch = root_epoch
        .parse::<u64>()
        .ok()
        .is_some_and(|epoch| epoch.to_string() == *root_epoch);
    let session_id_text = session_id.to_string();
    if expected_workspace != workspace_path
        || expected_session != &session_id_text
        || !canonical_epoch
    {
        return Err(DesktopCommandConflict::new(
            "the running chat owner changed before interrupt was applied",
        ));
    }
    let parsed = expected_turn_id
        .parse::<TurnId>()
        .ok()
        .filter(|turn_id| turn_id.to_string() == *expected_turn_id)
        .ok_or_else(|| {
            DesktopCommandConflict::new(
                "the running chat turn identity is invalid; refresh and try again",
            )
        })?;
    let revision = admission_revision
        .parse::<u64>()
        .ok()
        .filter(|parsed| parsed.to_string() == *admission_revision)
        .ok_or_else(|| {
            DesktopCommandConflict::new("the running chat admission revision is invalid")
        })?;
    if current_turn.is_some_and(|current| current != (parsed, revision)) {
        return Err(DesktopCommandConflict::new(
            "the running chat turn changed before interrupt was applied",
        ));
    }
    Ok((parsed, revision))
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
    if let Err(conflict) = ensure_unscoped_prompt_review_action(&mut controller, "image attachment")
    {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) =
        ensure_attachment_state_mutation(controller.state.set_image_attachment_input(text))
    {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    let path = match controller.prepare_image_attachment_from_input() {
        Ok(path) => path,
        Err(error) => {
            return Err(command_conflict_error(
                &mut controller,
                DesktopCommandConflict::with_status(
                    DesktopStatusCode::ImageAttachmentInvalid,
                    error,
                ),
            ));
        }
    };
    controller
        .authorize_attachment_asset(&app, &path)
        .map_err(DesktopCommandError::internal)?;
    if let Err(conflict) = ensure_unscoped_prompt_review_action(&mut controller, "image attachment")
    {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) =
        ensure_attachment_state_mutation(controller.state.attach_image_path(path))
    {
        return Err(command_conflict_error(&mut controller, conflict));
    }
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
    if let Err(conflict) = ensure_unscoped_prompt_review_action(&mut controller, "image attachment")
    {
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
    if let Err(conflict) = ensure_unscoped_prompt_review_action(&mut controller, "image attachment")
    {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    controller
        .authorize_attachment_asset(&app, &path)
        .map_err(DesktopCommandError::internal)?;
    if let Err(conflict) = ensure_unscoped_prompt_review_action(&mut controller, "image attachment")
    {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) =
        ensure_attachment_state_mutation(controller.state.attach_image_path(path))
    {
        return Err(command_conflict_error(&mut controller, conflict));
    }
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
        ensure_unscoped_prompt_review_action(controller, "image attachment")?;
        ensure_attachment_state_mutation(controller.state.clear_image_attachments())?;
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
        ensure_unscoped_prompt_review_action(controller, "image attachment")?;
        ensure_attachment_state_mutation(controller.state.remove_image_attachment(index))?;
        Ok(())
    })
    .await
}

#[tauri::command]
async fn show_file_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "file menu")?;
        controller.state.show_file_menu();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn show_edit_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "edit menu")?;
        controller.state.show_edit_menu();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn show_view_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "view menu")?;
        controller.state.show_view_menu();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn show_help_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "help menu")?;
        controller.state.show_help_menu();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn show_about(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "about dialog")?;
        controller.state.show_about();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn show_project_menu(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "project menu")?;
        controller.state.show_project_menu();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn create_project_from_picker(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        if !controller.create_project_from_picker() {
            return Err(rejected_action(
                controller,
                "project creation was not started",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn show_config_editor(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "configuration editor")?;
        controller.state.show_config_editor();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn show_hub_editor(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "Hub connection editor")?;
        controller.state.show_hub_editor();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn show_mcp_history(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "MCP history")?;
        if !controller.state.show_mcp_history() {
            return Err(rejected_action(
                controller,
                "MCP history could not be opened",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn mcp_history_list(
    jobs: State<'_, crate::remote_agent::RemoteJobService>,
    direction: crate::remote_agent::McpHistoryDirection,
    offset: usize,
    anchor: Option<String>,
) -> Result<crate::remote_agent::McpHistoryPage, String> {
    jobs.history_page(direction, offset, 20, anchor.as_deref())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn mcp_history_detail(
    jobs: State<'_, crate::remote_agent::RemoteJobService>,
    direction: crate::remote_agent::McpHistoryDirection,
    id: String,
) -> Result<crate::remote_agent::McpHistoryDetail, String> {
    jobs.history_detail(direction, &id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn mcp_history_export(
    jobs: State<'_, crate::remote_agent::RemoteJobService>,
    gate: State<'_, McpHistoryExportGate>,
    direction: crate::remote_agent::McpHistoryDirection,
    id: String,
) -> Result<serde_json::Value, String> {
    let _pending = gate
        .0
        .try_lock()
        .map_err(|_| "MCP履歴の保存先を選択中です。".to_string())?;
    // Capture this exact task before opening the native dialog. A selection or
    // runtime update cannot replace the document being saved while it is open.
    let detail = jobs
        .history_detail(direction, &id)
        .await
        .map_err(|error| error.to_string())?;
    let direction_name = match direction {
        crate::remote_agent::McpHistoryDirection::Instruction => "instruction",
        crate::remote_agent::McpHistoryDirection::Execution => "execution",
    };
    let file_name = format!("mcp-{direction_name}-{}.md", detail.row.id);
    let selected = tokio::task::spawn_blocking(move || {
        rfd::FileDialog::new()
            .set_title("MCP履歴をMarkdownで保存")
            .add_filter("Markdown", &["md"])
            .set_file_name(file_name)
            .save_file()
    })
    .await
    .map_err(|_| "保存先を選択できません。".to_string())?;
    let Some(selected) = selected else {
        return Ok(serde_json::json!({"path":null}));
    };
    let destination = camino::Utf8PathBuf::from_path_buf(selected)
        .map_err(|_| "保存先のパスをUTF-8で読み取れません。".to_string())?;
    // Do not change the filename after the native overwrite confirmation.
    if !destination
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        return Err("保存先には拡張子 .md のファイル名を指定してください。".into());
    }
    let receipt = serde_json::json!({"path":destination});
    tokio::task::spawn_blocking(move || {
        super::app::write_markdown_export_atomic(&destination, &detail.markdown)
    })
    .await
    .map_err(|_| "MCP履歴を保存できません。".to_string())??;
    Ok(receipt)
}

#[tauri::command]
async fn mcp_history_stop(
    jobs: State<'_, crate::remote_agent::RemoteJobService>,
    service: State<'_, DeviceNetworkService>,
    direction: crate::remote_agent::McpHistoryDirection,
    id: String,
) -> Result<crate::remote_agent::McpHistoryDetail, String> {
    let detail = jobs
        .history_detail(direction, &id)
        .await
        .map_err(|error| error.to_string())?;
    if !detail.row.can_stop {
        return Err("この履歴のタスクは現在停止できません。".into());
    }
    match direction {
        crate::remote_agent::McpHistoryDirection::Instruction => {
            service
                .cancel(&id)
                .await
                .map_err(|error| error.to_string())?;
        }
        crate::remote_agent::McpHistoryDirection::Execution => {
            let profile = detail
                .row
                .profile_id
                .parse::<ulid::Ulid>()
                .map_err(|_| "実行履歴の公開対象を確認できません。".to_string())?;
            let job = detail
                .row
                .id
                .parse::<ulid::Ulid>()
                .map_err(|_| "実行履歴のタスクを確認できません。".to_string())?;
            jobs.cancel_job(PublishProfileId(profile), job)
                .await
                .map_err(|error| error.to_string())?;
        }
    }
    jobs.history_detail(direction, &id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn remote_job_cancel(
    jobs: State<'_, crate::remote_agent::RemoteJobService>,
    profile_id: PublishProfileId,
    job_id: ulid::Ulid,
) -> Result<crate::remote_agent::RemoteJobRow, String> {
    jobs.cancel_job(profile_id, job_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn mcp_peer_projection(
    controller: State<'_, SharedController>,
) -> Result<serde_json::Value, String> {
    let controller = controller.lock().await;
    serde_json::to_value(super::mcp_peers::projection(
        controller.state.global_config(),
    ))
    .map_err(|_| "接続一覧を表示できません。".into())
}

#[tauri::command]
async fn device_network_projection(
    service: State<'_, DeviceNetworkService>,
    publish: State<'_, PublishService>,
) -> Result<DeviceNetworkProjection, String> {
    let _ = publish.refresh().await;
    Ok(service.projection_now())
}

#[tauri::command]
async fn device_network_refresh(
    service: State<'_, DeviceNetworkService>,
) -> Result<DeviceNetworkProjection, String> {
    service.refresh().await.map_err(|error| error.to_string())
}

#[tauri::command]
async fn device_network_join(
    service: State<'_, DeviceNetworkService>,
    code: String,
    confirmed: bool,
    expected_revision: String,
    expected_generation: String,
) -> Result<DeviceNetworkProjection, String> {
    service
        .join(code, confirmed, &expected_revision, &expected_generation)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn device_network_request_join(
    service: State<'_, DeviceNetworkService>,
    expected_revision: String,
    expected_generation: String,
) -> Result<DeviceNetworkProjection, String> {
    service
        .request_join(&expected_revision, &expected_generation)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn device_network_diagnose(
    service: State<'_, DeviceNetworkService>,
    scope: crate::device_network::DiagnosticScope,
    device_id: Option<String>,
    profile_id: Option<String>,
    expected_revision: String,
    expected_generation: String,
) -> Result<crate::device_network::DeviceDiagnostic, String> {
    service
        .diagnose(
            scope,
            device_id,
            profile_id,
            &expected_revision,
            &expected_generation,
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn device_network_artifacts(
    service: State<'_, DeviceNetworkService>,
    reference_id: String,
    version: Option<String>,
) -> Result<serde_json::Value, String> {
    let manifest = service
        .artifacts(&reference_id, version.as_deref())
        .await
        .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({"reference_id":reference_id,"manifest":manifest}))
}

#[tauri::command]
async fn device_network_export_artifacts(
    service: State<'_, DeviceNetworkService>,
    reference_id: String,
    version: String,
) -> Result<Option<serde_json::Value>, String> {
    let manifest = service
        .cached_artifact_manifest(&reference_id, &version)
        .map_err(|error| error.to_string())?;
    let selected = tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("成果物を書き出す親フォルダを選択")
            .pick_folder()
    })
    .await
    .map_err(|_| "保存先を選択できません。".to_string())?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let parent = camino::Utf8PathBuf::from_path_buf(selected)
        .map_err(|_| "保存先のパスをUTF-8で読み取れません。".to_string())?;
    let current = service
        .cached_artifact_manifest(&reference_id, &version)
        .map_err(|error| error.to_string())?;
    if current != manifest {
        return Err("artifacts_unavailable".into());
    }
    let directory = parent.join(format!(
        "moyai-{}-{}",
        manifest.job_id,
        &manifest.version[..12]
    ));
    let destination = directory.clone();
    let service = service.inner().clone();
    let receipt = serde_json::json!({"reference_id":reference_id,"job_id":manifest.job_id,"version":version,"directory":directory});
    tokio::task::spawn_blocking(move || {
        service.export_artifacts(&reference_id, &version, &destination)
    })
    .await
    .map_err(|_| "成果物の書き出しを完了できません。".to_string())??;
    Ok(Some(receipt))
}

#[tauri::command]
async fn device_network_receiver(
    controller: State<'_, SharedController>,
    service: State<'_, DeviceNetworkService>,
    enabled: bool,
    target: crate::mcp_publish::PublishTarget,
    access_mode: crate::config::AccessMode,
    model_mode: crate::hub::HubRouteMode,
    confirmed: bool,
    start_on_launch: bool,
    keep_when_hidden: bool,
    bind_ip: Option<std::net::Ipv4Addr>,
    port: Option<u16>,
    expected_revision: String,
    expected_generation: String,
) -> Result<DeviceNetworkProjection, String> {
    let config = controller.lock().await.state.global_config().clone();
    service.update_runtime_config(config);
    service
        .receiver_with_bind(
            enabled,
            target,
            access_mode,
            model_mode,
            confirmed,
            start_on_launch,
            keep_when_hidden,
            crate::device_network::ReceiverBindSettings { bind_ip, port },
            &expected_revision,
            &expected_generation,
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn device_network_select(
    service: State<'_, DeviceNetworkService>,
    device_id: String,
    profile_id: String,
    enabled: bool,
    expected_revision: String,
    expected_generation: String,
) -> Result<DeviceNetworkProjection, String> {
    service
        .select(
            device_id,
            profile_id,
            enabled,
            &expected_revision,
            &expected_generation,
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn device_network_leave(
    service: State<'_, DeviceNetworkService>,
    expected_revision: String,
    expected_generation: String,
) -> Result<DeviceNetworkProjection, String> {
    service
        .leave(&expected_revision, &expected_generation)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn device_network_jobs(
    service: State<'_, DeviceNetworkService>,
) -> Result<crate::device_network::DeviceNetworkJobs, String> {
    service.jobs().await.map_err(|error| error.to_string())
}

#[tauri::command]
async fn device_network_cancel(
    service: State<'_, DeviceNetworkService>,
    reference_id: String,
) -> Result<crate::device_network::DeviceDelegationRow, String> {
    service
        .cancel(&reference_id)
        .await
        .map_err(|error| error.to_string())
}

async fn pick_shared_hub_config(
    start: camino::Utf8PathBuf,
) -> Result<Option<SharedHubConfig>, String> {
    tokio::task::spawn_blocking(move || {
        let selected = DesktopController::pick_initial_setup_config_toml_dialog(&start)
            .map_err(|_| "storage_error".to_string())?;
        selected
            .map(|path| {
                let text = crate::config::loader::read_toml_utf8_bounded(&path)
                    .map_err(|_| "invalid_configuration".to_string())?;
                SharedHubConfig::import(&text).map_err(|error| error.to_string())
            })
            .transpose()
    })
    .await
    .map_err(|_| "storage_error".to_string())?
}

#[tauri::command]
async fn device_network_import(
    controller: State<'_, SharedController>,
    service: State<'_, DeviceNetworkService>,
    expected_revision: String,
    expected_generation: String,
) -> Result<DeviceNetworkProjection, String> {
    let (start, target) = {
        let mut controller = controller.lock().await;
        controller.drain_runtime_messages();
        ensure_unscoped_prompt_review_action(&mut controller, "Hub common configuration")
            .map_err(|_| "connection_changed")?;
        service
            .check_target(&expected_revision, &expected_generation)
            .map_err(|error| error.to_string())?;
        let target = DesktopConfigMutationTarget {
            workspace_path: controller.app.workspace.authority_root().to_string(),
            session_id: controller
                .state
                .app_state
                .current_session_id
                .map(|id| id.to_string()),
            config_generation: controller
                .state
                .provider_config
                .config_generation
                .to_string(),
        };
        (controller.app.workspace.root.clone(), target)
    };
    let loaded = pick_shared_hub_config(start).await;
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    ensure_config_mutation_target(&controller, &target).map_err(|_| "connection_changed")?;
    ensure_unscoped_prompt_review_action(&mut controller, "Hub common configuration")
        .map_err(|_| "connection_changed")?;
    service
        .check_target(&expected_revision, &expected_generation)
        .map_err(|error| error.to_string())?;
    let Some(shared) = loaded? else {
        return Ok(service.projection_now());
    };
    let saved = shared.clone();
    let mut persist_error = None;
    let result = service
        .configure_with_commit(shared, &expected_revision, &expected_generation, || {
            controller
                .save_device_network_config(&saved, false)
                .map_err(|error| {
                    persist_error = Some(error);
                    crate::device_network::DeviceError::Storage
                })
        })
        .await;
    if let Some(error) = persist_error {
        return Err(error);
    }
    let result = result.map_err(|error| error.to_string())?;
    service.update_runtime_config(controller.state.global_config().clone());
    drop(controller);
    service
        .request_join(&result.revision, &result.generation)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn device_network_initial_setup_import(
    controller: State<'_, SharedController>,
    service: State<'_, DeviceNetworkService>,
    expected_setup_target: DesktopInitialSetupMutationTarget,
    expected_config_target: DesktopConfigMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    let shared_controller = controller.inner().clone();
    let before = {
        let mut controller = controller.lock().await;
        controller.drain_runtime_messages();
        ensure_initial_setup_mutation_target(&controller, &expected_setup_target)
            .and_then(|()| ensure_config_mutation_target(&controller, &expected_config_target))
            .and_then(|()| ensure_config_draft_commit_admission(&controller))
            .map_err(|conflict| command_conflict_error(&mut controller, conflict))?;
        service.projection_now()
    };
    let loaded = pick_shared_hub_config(camino::Utf8PathBuf::from(
        &expected_setup_target.workspace_path,
    ))
    .await;
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    ensure_initial_setup_mutation_target(&controller, &expected_setup_target)
        .and_then(|()| ensure_config_mutation_target(&controller, &expected_config_target))
        .and_then(|()| ensure_config_draft_commit_admission(&controller))
        .map_err(|conflict| command_conflict_error(&mut controller, conflict))?;
    let Some(shared) = loaded.map_err(|error| {
        command_conflict_error(&mut controller, DesktopCommandConflict::new(error))
    })?
    else {
        return controller
            .next_web_state()
            .map_err(DesktopCommandError::internal);
    };
    let saved = shared.clone();
    let mut persist_error = None;
    let result = service
        .configure_with_commit(shared, &before.revision, &before.generation, || {
            controller
                .save_device_network_config(&saved, true)
                .map_err(|error| {
                    persist_error = Some(error);
                    crate::device_network::DeviceError::Storage
                })
        })
        .await;
    let outcome = persist_error.map_or_else(
        || result.map(|_| ()).map_err(|error| error.to_string()),
        Err,
    );
    outcome.map_err(|error| {
        command_conflict_error(&mut controller, DesktopCommandConflict::new(error))
    })?;
    service.update_runtime_config(controller.state.global_config().clone());
    service.enable_default_model_on_join();
    let target = service.projection_now();
    drop(controller);
    let _ = service
        .request_join(&target.revision, &target.generation)
        .await;
    let mut controller = shared_controller.lock().await;
    controller
        .next_web_state()
        .map_err(DesktopCommandError::internal)
}

#[tauri::command]
async fn mcp_peer_add(
    controller: State<'_, SharedController>,
    peer: super::mcp_peers::McpPeerDraft,
    expected_target: DesktopConfigMutationTarget,
) -> Result<(DesktopWebState, bool), DesktopCommandError> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    ensure_config_mutation_target(&controller, &expected_target)
        .and_then(|()| ensure_config_draft_commit_admission(&controller))
        .map_err(|error| command_conflict_error(&mut controller, error))?;
    // Invalid form values do not invalidate the UI owner or require a replacement
    // projection. Keep the editable draft and report the failure in its form.
    let config = super::mcp_peers::add(controller.state.global_config(), peer)
        .map_err(DesktopCommandError::internal)?;
    let servers = serde_json::to_string(&config.mcp.servers)
        .map_err(|error| DesktopCommandError::internal(error.to_string()))?;
    let saved = controller.save_global_config(vec![
        ("mcp.enabled".into(), "true".into()),
        ("mcp.servers_json".into(), servers),
    ]);
    controller.drain_runtime_messages();
    Ok((
        controller
            .next_web_state()
            .map_err(DesktopCommandError::internal)?,
        saved,
    ))
}

#[tauri::command]
async fn mcp_peer_remove(
    controller: State<'_, SharedController>,
    id: String,
    expected_target: DesktopConfigMutationTarget,
) -> Result<(DesktopWebState, bool), DesktopCommandError> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    ensure_config_mutation_target(&controller, &expected_target)
        .and_then(|()| ensure_config_draft_commit_admission(&controller))
        .map_err(|error| command_conflict_error(&mut controller, error))?;
    let config = super::mcp_peers::remove(controller.state.global_config(), &id)
        .map_err(DesktopCommandError::internal)?;
    let servers = serde_json::to_string(&config.mcp.servers)
        .map_err(|error| DesktopCommandError::internal(error.to_string()))?;
    let saved = controller.save_global_config(vec![("mcp.servers_json".into(), servers)]);
    controller.drain_runtime_messages();
    Ok((
        controller
            .next_web_state()
            .map_err(DesktopCommandError::internal)?,
        saved,
    ))
}

#[tauri::command]
async fn mcp_peer_check(
    controller: State<'_, SharedController>,
    id: String,
) -> Result<serde_json::Value, String> {
    let config = controller.lock().await.state.global_config().clone();
    let result = super::mcp_peers::check(&config, &id).await?;
    serde_json::to_value(result).map_err(|_| "接続結果を表示できません。".into())
}

#[tauri::command]
async fn hub_projection(
    connection: State<'_, HubConnection>,
) -> Result<HubConnectionProjection, String> {
    Ok(connection.projection().await)
}

#[tauri::command]
async fn hub_connect(
    connection: State<'_, HubConnection>,
    endpoint: String,
    token: String,
    label: String,
    expected_settings_revision: String,
    expected_connection_generation: String,
) -> Result<HubConnectionProjection, String> {
    connection
        .connect(
            endpoint,
            token,
            label,
            expected_settings_revision,
            expected_connection_generation,
        )
        .await
        .map_err(|error| error.code().to_string())
}

#[tauri::command]
async fn hub_refresh(
    connection: State<'_, HubConnection>,
    expected_connection_generation: String,
) -> Result<HubConnectionProjection, String> {
    connection
        .refresh(expected_connection_generation)
        .await
        .map_err(|error| error.code().to_string())
}

#[tauri::command]
async fn hub_save_review(
    connection: State<'_, HubConnection>,
    context: HubReviewContext,
    selection: HubSelection,
    expected_hub_id: String,
    expected_catalog_revision: CatalogRevision,
    expected_settings_revision: String,
    expected_connection_generation: String,
) -> Result<HubConnectionProjection, String> {
    connection
        .save_review(
            context,
            selection,
            expected_hub_id,
            expected_catalog_revision,
            expected_settings_revision,
            expected_connection_generation,
        )
        .await
        .map_err(|error| error.code().to_string())
}

#[tauri::command]
async fn hub_disconnect(
    connection: State<'_, HubConnection>,
    expected_connection_generation: String,
) -> Result<HubConnectionProjection, String> {
    connection
        .disconnect(expected_connection_generation)
        .await
        .map_err(|error| error.code().to_string())
}

#[tauri::command]
async fn hub_set_route_mode(
    controller: State<'_, SharedController>,
    connection: State<'_, HubConnection>,
    context: HubReviewContext,
    mode: crate::hub::HubRouteMode,
    expected_settings_revision: String,
    expected_connection_generation: String,
) -> Result<HubConnectionProjection, String> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if controller.hub_context_active(context) {
        return Err("route_busy".into());
    }
    connection
        .set_route_mode_now(
            context,
            mode,
            expected_settings_revision,
            expected_connection_generation,
        )
        .map_err(|error| error.code().to_string())
}

#[tauri::command]
async fn show_session_settings(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "session settings")?;
        if !controller.state.show_session_settings() {
            return Err(rejected_action(
                controller,
                "session settings require the current root session",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn show_provider_editor(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "provider editor")?;
        controller.state.show_provider_editor();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn show_workspace_picker(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "workspace picker")?;
        controller.show_workspace_picker();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn show_command_palette(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "command palette")?;
        controller.state.show_command_palette();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn show_shortcuts(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "keyboard shortcuts")?;
        controller.state.show_keyboard_shortcuts();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn close_overlay(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        validate_unscoped_overlay_close(controller.state.view.overlay)?;
        controller.state.hide_overlay();
        Ok(())
    })
    .await
}

#[tauri::command]
async fn switch_workspace(
    controller: State<'_, SharedController>,
    text: String,
    expected_target: DesktopDraftActionTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_draft_action_target(controller, &expected_target)?;
        ensure_unscoped_prompt_review_action(controller, "workspace navigation")?;
        if !controller.switch_workspace_to(text) {
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
    if let Err(conflict) =
        ensure_unscoped_prompt_review_action(&mut controller, "workspace navigation")
    {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    controller.state.set_workspace_input(text);
    let selected = controller.browse_workspace_dialog();
    controller.drain_runtime_messages();
    if let Err(conflict) = ensure_draft_action_target(&controller, &expected_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) =
        ensure_unscoped_prompt_review_action(&mut controller, "workspace navigation")
    {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Some(path) = selected {
        controller.state.show_workspace_picker(path.as_str());
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
async fn open_user_data_folder(
    controller: State<'_, SharedController>,
) -> Result<DesktopWebState, String> {
    mutate_controller(controller, DesktopController::open_user_data_folder).await
}

#[tauri::command]
async fn import_global_config_toml(
    controller: State<'_, SharedController>,
    draft_values: Vec<DesktopConfigValueInput>,
    expected_target: DesktopConfigMutationTarget,
) -> Result<(DesktopWebState, bool), DesktopCommandError> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if let Err(conflict) =
        ensure_unscoped_prompt_review_action(&mut controller, "configuration import")
    {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) = ensure_config_mutation_target(&controller, &expected_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) = ensure_external_config_owner_mutation_open(&controller, &draft_values) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    let selected = controller.pick_global_config_toml_dialog();
    controller.drain_runtime_messages();
    if let Err(conflict) =
        ensure_unscoped_prompt_review_action(&mut controller, "configuration import")
    {
        return Err(command_conflict_error(&mut controller, conflict));
    }
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
async fn load_initial_setup_config_toml(
    controller: State<'_, SharedController>,
    expected_config_target: DesktopConfigMutationTarget,
    expected_setup_target: DesktopInitialSetupMutationTarget,
) -> Result<Option<DesktopInitialSetupConfigDraft>, DesktopCommandError> {
    {
        let mut controller = controller.lock().await;
        controller.drain_runtime_messages();
        if let Err(conflict) =
            ensure_initial_setup_mutation_target(&controller, &expected_setup_target)
        {
            return Err(command_conflict_error(&mut controller, conflict));
        }
        if let Err(conflict) = ensure_config_mutation_target(&controller, &expected_config_target) {
            return Err(command_conflict_error(&mut controller, conflict));
        }
    }

    // Native dialogs and bounded file reads may wait on the user or filesystem.
    // Keep them outside the controller lock, then CAS the exact owners again.
    let import_start_dir = camino::Utf8PathBuf::from(expected_setup_target.workspace_path.clone());
    let loaded = DesktopController::pick_initial_setup_config_toml_dialog(&import_start_dir)
        .and_then(|selected| {
            selected
                .map(|path| {
                    DesktopController::load_initial_setup_config_toml_path(&path)
                        .map(|config| (path, config))
                })
                .transpose()
        });

    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if let Err(conflict) = ensure_initial_setup_mutation_target(&controller, &expected_setup_target)
    {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) = ensure_config_mutation_target(&controller, &expected_config_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    let Some((path, config)) = (match loaded {
        Ok(loaded) => loaded,
        Err(error) => {
            let conflict = DesktopCommandConflict::with_status(
                DesktopStatusCode::ConfigImportFailed,
                format!("initial setup import failed: {error}"),
            );
            return Err(command_conflict_error(&mut controller, conflict));
        }
    }) else {
        return Ok(None);
    };
    let (import_generation, values) = controller
        .stage_initial_setup_config_import(config)
        .map_err(|error| {
            command_conflict_error(
                &mut controller,
                DesktopCommandConflict::with_status(
                    DesktopStatusCode::ConfigImportFailed,
                    format!("initial setup import failed: {error}"),
                ),
            )
        })?;
    Ok(Some(DesktopInitialSetupConfigDraft {
        source_path: path.to_string(),
        import_generation: import_generation.to_string(),
        values: values
            .into_iter()
            .map(|value| DesktopInitialSetupConfigFieldDraft {
                key: value.key,
                text: value.text,
                sensitive: value.sensitive,
                configured: value.configured,
            })
            .collect(),
    }))
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
    expected_draft_target: DesktopDraftActionTarget,
) -> Result<DesktopCommandPaletteInsertionResult, DesktopCommandError> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if let Err(conflict) =
        ensure_draft_action_target(&controller, &expected_draft_target).and_then(|()| {
            ensure_indexed_row_mutation_target(
                &controller,
                &expected_target,
                DesktopRowCollection::Command,
                index,
            )
        })
    {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    let Some(insertion_text) = controller.state.select_command_from_palette(index) else {
        let conflict = rejected_action(
            &controller,
            "the command palette selection is no longer available",
        );
        return Err(command_conflict_error(&mut controller, conflict));
    };
    controller.drain_runtime_messages();
    let state = controller
        .next_web_state()
        .map_err(DesktopCommandError::internal)?;
    Ok(DesktopCommandPaletteInsertionResult {
        state,
        insertion_text,
    })
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopProviderActionInput {
    base_url: String,
    provider_profile: String,
    api_key_env: String,
    context_window: String,
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
            .field("provider_profile", &self.provider_profile)
            .field("api_key_env", &self.api_key_env)
            .field("context_window", &self.context_window)
            .field("selected_model_id", &self.selected_model_id)
            .finish()
    }
}

fn canonical_provider_api_key_env_input(
    input: &str,
) -> Result<Option<String>, DesktopCommandConflict> {
    if input.trim().is_empty() {
        return Ok(None);
    }
    crate::config::canonical_api_key_env_name(Some(input)).map_err(|_| {
        DesktopCommandConflict::new("the API key environment variable name is invalid")
    })
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
    let provider_profile =
        parse_provider_profile_input(&input.provider_profile).map_err(|error| {
            controller
                .state
                .set_status_message("the provider connection type is invalid");
            rejected_action(controller, &error.message)
        })?;
    let api_key_env =
        canonical_provider_api_key_env_input(&input.api_key_env).map_err(|error| {
            controller.state.set_status_message(error.message.clone());
            error
        })?;
    controller.accept_provider_action_input(
        input.base_url,
        provider_profile,
        api_key_env.unwrap_or_default(),
        input.context_window,
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
        ensure_unscoped_prompt_review_action(controller, "provider model loading")?;
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
async fn check_docling_readiness(
    controller: State<'_, SharedController>,
    expected_target: DesktopConfigMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "Docling readiness check")?;
        ensure_config_mutation_target(controller, &expected_target)?;
        if !controller.check_docling_readiness() {
            return Err(rejected_action(
                controller,
                "the Docling readiness check was not started",
            ));
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn check_initial_setup_docling_readiness(
    controller: State<'_, SharedController>,
    values: Vec<DesktopConfigValueInput>,
    expected_config_target: DesktopConfigMutationTarget,
    expected_setup_target: DesktopInitialSetupMutationTarget,
    import_generation: Option<String>,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_initial_setup_mutation_target(controller, &expected_setup_target)?;
        ensure_config_mutation_target(controller, &expected_config_target)?;
        validate_complete_config_draft(controller, &values)?;
        let import_generation =
            parse_initial_setup_import_generation(import_generation.as_deref())?;
        if !controller.check_initial_setup_docling_readiness(
            values
                .into_iter()
                .map(|value| (value.key, value.text))
                .collect(),
            import_generation,
        ) {
            return Err(rejected_action(
                controller,
                "the initial-setup Docling readiness check was not started",
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
        ensure_unscoped_prompt_review_action(controller, "provider configuration")?;
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
        ensure_unscoped_prompt_review_action(controller, "provider configuration")?;
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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct DesktopConfigValueInput {
    key: String,
    text: String,
}

impl std::fmt::Debug for DesktopConfigValueInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DesktopConfigValueInput")
            .field("key", &self.key)
            .field("text_chars", &self.text.chars().count())
            .finish()
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopInitialSetupConfigDraft {
    source_path: String,
    import_generation: String,
    values: Vec<DesktopInitialSetupConfigFieldDraft>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopInitialSetupConfigFieldDraft {
    key: String,
    text: String,
    sensitive: bool,
    configured: bool,
}

impl std::fmt::Debug for DesktopInitialSetupConfigFieldDraft {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DesktopInitialSetupConfigFieldDraft")
            .field("key", &self.key)
            .field("text_chars", &self.text.chars().count())
            .field("sensitive", &self.sensitive)
            .field("configured", &self.configured)
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopConfigMutationTarget {
    workspace_path: String,
    session_id: Option<String>,
    config_generation: String,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DesktopInitialSetupMutationTarget {
    workspace_path: String,
    global_config_path: String,
    setup_generation: String,
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

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DesktopSessionSettingsInput {
    base_url: String,
    model: String,
    provider_profile: String,
    api_key_env: String,
    access_mode: crate::config::AccessMode,
    context_window: String,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DesktopSessionSettingsMutationTarget {
    workspace_path: String,
    root_session_id: String,
    settings_revision: String,
    config_generation: String,
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
    complete_config_draft_is_dirty(controller.state.global_config(), values)
}

fn complete_config_draft_is_dirty(
    effective_config: &crate::config::ResolvedConfig,
    values: &[DesktopConfigValueInput],
) -> Result<bool, DesktopCommandConflict> {
    let expected_field_count = crate::config::ConfigField::ALL
        .into_iter()
        .filter(|field| !field.is_host_owned_generation())
        .count();
    let contains_only_current_gui_fields = values.iter().all(|value| {
        crate::config::ConfigField::ALL
            .into_iter()
            .any(|field| !field.is_host_owned_generation() && field.label() == value.key.as_str())
    });
    if values.len() != expected_field_count || !contains_only_current_gui_fields {
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
        controller.app.workspace.authority_root().as_str(),
        controller
            .state
            .app_state
            .current_session_id
            .map(|session_id| session_id.to_string()),
        controller.state.provider_config.config_generation,
    )
}

fn validate_initial_setup_mutation_target(
    expected: &DesktopInitialSetupMutationTarget,
    workspace_path: &str,
    global_config_path: &str,
    setup_generation: u64,
) -> Result<(), DesktopCommandConflict> {
    if expected.workspace_path != workspace_path
        || expected.global_config_path != global_config_path
        || expected.setup_generation != setup_generation.to_string()
    {
        return Err(DesktopCommandConflict::new(
            "initial-setup owner changed before Finish; review the current setup state and try again",
        ));
    }
    Ok(())
}

fn ensure_initial_setup_mutation_target(
    controller: &DesktopController,
    expected: &DesktopInitialSetupMutationTarget,
) -> Result<(), DesktopCommandConflict> {
    if !controller.state.startup.requires_initial_setup()
        || controller.state.view.overlay != DesktopOverlay::InitialSetup
    {
        return Err(DesktopCommandConflict::new(
            "initial setup is no longer the active configuration owner",
        ));
    }
    validate_initial_setup_mutation_target(
        expected,
        controller.app.workspace.authority_root().as_str(),
        controller
            .state
            .startup
            .global_config_path
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default()
            .as_str(),
        controller.state.startup.setup_generation,
    )
}

fn parse_initial_setup_import_generation(
    value: Option<&str>,
) -> Result<Option<u64>, DesktopCommandConflict> {
    let Some(value) = value else {
        return Ok(None);
    };
    let generation = value.parse::<u64>().map_err(|_| {
        DesktopCommandConflict::new(
            "initial setup import generation must be an unsigned decimal integer",
        )
    })?;
    if generation.to_string() != value {
        return Err(DesktopCommandConflict::new(
            "initial setup import generation is not canonical",
        ));
    }
    Ok(Some(generation))
}

fn validate_session_settings_mutation_target(
    expected: &DesktopSessionSettingsMutationTarget,
    workspace_path: &str,
    root_session_id: &str,
    settings_revision: u64,
    config_generation: u64,
    runtime_owner_token: &str,
) -> Result<(), DesktopCommandConflict> {
    validate_session_settings_settlement_target(
        expected,
        workspace_path,
        root_session_id,
        settings_revision,
        config_generation,
        runtime_owner_token,
        false,
        None,
    )
}

fn rebase_session_settings_persistence_outcome(
    controller: &mut DesktopController,
    outcome: &RootSessionSettingsPersistenceOutcome,
) -> bool {
    controller
        .state
        .apply_persisted_root_session_record(outcome.canonical_session().clone())
}

#[allow(clippy::too_many_arguments)]
fn validate_session_settings_settlement_target(
    expected: &DesktopSessionSettingsMutationTarget,
    workspace_path: &str,
    root_session_id: &str,
    settings_revision: u64,
    config_generation: u64,
    runtime_owner_token: &str,
    allow_access_owner_terminal_crossing: bool,
    already_projected_applied: Option<AlreadyProjectedSessionSettingsWrite>,
) -> Result<(), DesktopCommandConflict> {
    let runtime_owner_matches = expected.runtime_owner_token == runtime_owner_token
        || (allow_access_owner_terminal_crossing
            && access_runtime_owner_terminal_settlement_matches(
                &expected.runtime_owner_token,
                runtime_owner_token,
            ));
    let settings_revision_matches = expected.settings_revision == settings_revision.to_string()
        || already_projected_applied
            .is_some_and(|applied| applied.settings_revision == settings_revision);
    let config_generation_matches = expected.config_generation == config_generation.to_string()
        || already_projected_applied
            .is_some_and(|applied| applied.next_config_generation == Some(config_generation));
    if expected.workspace_path != workspace_path
        || expected.root_session_id != root_session_id
        || !settings_revision_matches
        || !config_generation_matches
        || !runtime_owner_matches
    {
        return Err(DesktopCommandConflict::new(
            "root-session settings owner changed before Apply; reopen the panel and try again",
        ));
    }
    Ok(())
}

fn ensure_session_settings_mutation_target(
    controller: &DesktopController,
    expected: &DesktopSessionSettingsMutationTarget,
) -> Result<u64, DesktopCommandConflict> {
    if controller.state.view.overlay != DesktopOverlay::SessionSettings {
        return Err(DesktopCommandConflict::new(
            "session settings are no longer the active panel",
        ));
    }
    let session = controller
        .state
        .open_session
        .as_ref()
        .filter(|open_session| {
            Some(open_session.session_id()) == controller.state.app_state.current_session_id
        })
        .map(|open_session| open_session.session())
        .ok_or_else(|| {
            DesktopCommandConflict::new("the current root-session settings owner is unavailable")
        })?;
    let (runtime_owner_token, _) = controller.access_mode_mutation_runtime_contract();
    validate_session_settings_mutation_target(
        expected,
        controller.app.workspace.authority_root().as_str(),
        &session.id.to_string(),
        session.session_settings_revision,
        controller.state.provider_config.config_generation,
        &runtime_owner_token,
    )?;
    Ok(session.session_settings_revision)
}

fn ensure_session_settings_settlement_target(
    controller: &DesktopController,
    expected: &DesktopSessionSettingsMutationTarget,
    policy: SessionSettingsSettlementPolicy,
    outcome: Option<&RootSessionSettingsPersistenceOutcome>,
) -> Result<u64, DesktopCommandConflict> {
    if controller.state.view.overlay != DesktopOverlay::SessionSettings {
        return Err(DesktopCommandConflict::new(
            "session settings are no longer the active panel",
        ));
    }
    let session = controller
        .state
        .open_session
        .as_ref()
        .filter(|open_session| {
            Some(open_session.session_id()) == controller.state.app_state.current_session_id
        })
        .map(|open_session| open_session.session())
        .ok_or_else(|| {
            DesktopCommandConflict::new("the current root-session settings owner is unavailable")
        })?;
    let (runtime_owner_token, _) = controller.access_mode_mutation_runtime_contract();
    let already_projected_applied = match outcome {
        Some(RootSessionSettingsPersistenceOutcome::Applied(result))
            if controller
                .state
                .persisted_root_session_settings_and_effective_config_are_projected(
                    &result.update.session,
                ) =>
        {
            let next_config_generation = (result.update.changed
                && result.config_generation_delta == 1)
                .then(|| {
                    expected
                        .config_generation
                        .parse::<u64>()
                        .ok()
                        .filter(|generation| generation.to_string() == expected.config_generation)
                        .and_then(|generation| generation.checked_add(1))
                        .filter(|generation| {
                            *generation == controller.state.provider_config.config_generation
                        })
                })
                .flatten();
            Some(AlreadyProjectedSessionSettingsWrite {
                settings_revision: result.update.session.session_settings_revision,
                next_config_generation,
            })
        }
        _ => None,
    };
    validate_session_settings_settlement_target(
        expected,
        controller.app.workspace.authority_root().as_str(),
        &session.id.to_string(),
        session.session_settings_revision,
        controller.state.provider_config.config_generation,
        &runtime_owner_token,
        policy.access_only,
        already_projected_applied,
    )?;
    Ok(session.session_settings_revision)
}

fn parse_session_settings_u32(
    field: &str,
    value: &str,
    require_positive: bool,
) -> Result<u32, DesktopCommandConflict> {
    let parsed = value
        .trim()
        .parse::<u32>()
        .map_err(|_| DesktopCommandConflict::new(format!("{field} must be an unsigned integer")))?;
    if require_positive && parsed == 0 {
        return Err(DesktopCommandConflict::new(format!(
            "{field} must be greater than zero"
        )));
    }
    Ok(parsed)
}

fn parse_optional_session_settings_u32(
    field: &str,
    value: &str,
    require_positive: bool,
) -> Result<Option<u32>, DesktopCommandConflict> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    parse_session_settings_u32(field, value, require_positive).map(Some)
}

fn session_model_parameter_patch(
    current: &crate::session::SessionModelParameters,
    context_window: Option<u32>,
) -> SessionSettingsPatch {
    let clears_existing_override = current.context_window.is_some() && context_window.is_none();
    if clears_existing_override {
        return SessionSettingsPatch {
            reset_model_parameters: true,
            context_window,
            ..SessionSettingsPatch::default()
        };
    }
    SessionSettingsPatch {
        context_window: (context_window != current.context_window)
            .then_some(context_window)
            .flatten(),
        ..SessionSettingsPatch::default()
    }
}

fn session_settings_patch_for_values(
    session: &crate::session::SessionRecord,
    base_url: String,
    model: String,
    provider_connection: crate::session::SessionProviderConnection,
    access_mode: crate::config::AccessMode,
    context_window: Option<u32>,
) -> (SessionSettingsPatch, bool) {
    let mut patch = session_model_parameter_patch(&session.model_parameters, context_window);
    patch.base_url = (base_url != session.base_url).then_some(base_url);
    patch.model = (model != session.model).then_some(model);
    patch.provider_connection = (session.provider_connection.as_ref()
        != Some(&provider_connection))
    .then_some(provider_connection);
    patch.access_mode = (access_mode != session.access_mode).then_some(access_mode);
    let changes_turn_config = patch.base_url.is_some()
        || patch.model.is_some()
        || patch.provider_connection.is_some()
        || patch.reset_model_parameters
        || patch.context_window.is_some();
    (patch, changes_turn_config)
}

fn session_settings_extra_headers_for_target(
    session: &crate::session::SessionRecord,
    effective_model: &crate::config::ModelConfig,
    requested_base_url: &str,
    requested_profile: ProviderProfile,
) -> std::collections::BTreeMap<String, String> {
    let current_profile = session
        .provider_connection
        .as_ref()
        .map(|connection| connection.profile)
        .unwrap_or(effective_model.provider_profile);
    let current_base_url = crate::config::ProviderEndpoint::parse(&session.base_url)
        .ok()
        .map(|endpoint| endpoint.as_str().to_string());
    if current_base_url.as_deref() != Some(requested_base_url)
        || current_profile != requested_profile
    {
        return Default::default();
    }
    session
        .provider_connection
        .as_ref()
        .map(|connection| connection.extra_headers.clone())
        .unwrap_or_else(|| effective_model.extra_headers.clone())
}

fn build_session_settings_patch(
    controller: &DesktopController,
    input: DesktopSessionSettingsInput,
) -> Result<(SessionSettingsPatch, bool), DesktopCommandConflict> {
    let session = controller
        .state
        .open_session
        .as_ref()
        .filter(|open_session| {
            Some(open_session.session_id()) == controller.state.app_state.current_session_id
        })
        .map(|open_session| open_session.session())
        .ok_or_else(|| {
            DesktopCommandConflict::new("the current root-session settings owner is unavailable")
        })?;
    let base_url = crate::config::ProviderEndpoint::parse(&input.base_url)
        .map_err(|error| DesktopCommandConflict::new(error.to_string()))?
        .as_str()
        .to_string();
    let model = input.model.trim().to_string();
    if model.is_empty() {
        return Err(DesktopCommandConflict::new(
            "session settings model must not be empty",
        ));
    }
    let provider_profile = parse_provider_profile_input(&input.provider_profile)?;
    let api_key_env = canonical_provider_api_key_env_input(&input.api_key_env)?;
    let effective_model = &controller.state.provider_config.effective_config.model;
    let extra_headers = session_settings_extra_headers_for_target(
        session,
        effective_model,
        &base_url,
        provider_profile,
    );
    let provider_connection = crate::session::SessionProviderConnection {
        profile: provider_profile,
        api_key_env,
        extra_headers,
    };
    let context_window =
        parse_optional_session_settings_u32("context window", &input.context_window, true)?;
    Ok(session_settings_patch_for_values(
        session,
        base_url,
        model,
        provider_connection,
        input.access_mode,
        context_window,
    ))
}

fn session_settings_storage_error(
    controller: &mut DesktopController,
    error: String,
) -> DesktopCommandError {
    controller
        .state
        .set_status_message(format!("session settings were not saved: {error}"));
    let state = controller.next_web_state().ok();
    DesktopCommandError {
        kind: "internal",
        category: DesktopCommandErrorCategory::Storage,
        code: DesktopCommandErrorCode::StorageFailure,
        message: error,
        state,
    }
}

fn ensure_config_draft_commit_admission(
    controller: &DesktopController,
) -> Result<(), DesktopCommandConflict> {
    if !controller.config_draft_mutation_admission_open() {
        return Err(DesktopCommandConflict::new(
            "configuration cannot be committed while a root run, navigation, or owner mutation is active",
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
        controller.app.workspace.authority_root().as_str(),
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
    if let Err(conflict) = ensure_unscoped_prompt_review_action(&mut controller, "configuration") {
        return Err(command_conflict_error(&mut controller, conflict));
    }
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
    if let Err(conflict) = ensure_unscoped_prompt_review_action(&mut controller, "configuration") {
        return Err(command_conflict_error(&mut controller, conflict));
    }
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
async fn finish_initial_setup(
    controller: State<'_, SharedController>,
    values: Vec<DesktopConfigValueInput>,
    expected_config_target: DesktopConfigMutationTarget,
    expected_setup_target: DesktopInitialSetupMutationTarget,
    import_generation: Option<String>,
) -> Result<(DesktopWebState, bool), DesktopCommandError> {
    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if let Err(conflict) = ensure_unscoped_prompt_review_action(&mut controller, "initial setup") {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) = ensure_initial_setup_mutation_target(&controller, &expected_setup_target)
    {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) = ensure_config_mutation_target(&controller, &expected_config_target) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) = ensure_config_draft_commit_admission(&controller) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) = validate_complete_config_draft(&controller, &values) {
        return Err(command_conflict_error(&mut controller, conflict));
    }
    let import_generation =
        match parse_initial_setup_import_generation(import_generation.as_deref()) {
            Ok(generation) => generation,
            Err(conflict) => return Err(command_conflict_error(&mut controller, conflict)),
        };
    let finished = controller.finish_initial_setup(
        values
            .into_iter()
            .map(|value| (value.key, value.text))
            .collect(),
        import_generation,
    );
    controller.drain_runtime_messages();
    Ok((
        controller
            .next_web_state()
            .map_err(DesktopCommandError::internal)?,
        finished,
    ))
}

#[tauri::command]
async fn apply_session_settings(
    controller: State<'_, SharedController>,
    input: DesktopSessionSettingsInput,
    expected_target: DesktopSessionSettingsMutationTarget,
) -> Result<(DesktopWebState, bool), DesktopCommandError> {
    let (persistence, operation_id, settlement_policy) = {
        let mut controller = controller.lock().await;
        controller.drain_runtime_messages();
        if let Err(conflict) =
            ensure_unscoped_prompt_review_action(&mut controller, "session settings")
        {
            return Err(command_conflict_error(&mut controller, conflict));
        }
        let expected_revision =
            match ensure_session_settings_mutation_target(&controller, &expected_target) {
                Ok(revision) => revision,
                Err(conflict) => return Err(command_conflict_error(&mut controller, conflict)),
            };
        let (patch, changes_turn_config) = match build_session_settings_patch(&controller, input) {
            Ok(result) => result,
            Err(conflict) => return Err(command_conflict_error(&mut controller, conflict)),
        };
        if patch.is_empty() {
            controller
                .state
                .set_status_message("session settings already match the saved root-session values");
            return Ok((
                controller
                    .next_web_state()
                    .map_err(DesktopCommandError::internal)?,
                true,
            ));
        }
        if changes_turn_config && !controller.session_settings_turn_config_mutation_admission_open()
        {
            let conflict = DesktopCommandConflict::new(
                "provider, model, context, and output settings can be saved after the active root run finishes",
            );
            return Err(command_conflict_error(&mut controller, conflict));
        }
        if !changes_turn_config
            && patch.access_mode.is_some()
            && !controller.access_mode_mutation_admission_open()
        {
            let conflict = DesktopCommandConflict::new(
                "access mode cannot change while navigation or an owner mutation is active",
            );
            return Err(command_conflict_error(&mut controller, conflict));
        }
        let persistence = match controller
            .prepare_root_session_settings_persistence(expected_revision, patch)
        {
            Ok(Some(persistence)) => persistence,
            Ok(None) => {
                let conflict = DesktopCommandConflict::new(
                    "root-session settings changed before Apply; reopen the panel and try again",
                );
                return Err(command_conflict_error(&mut controller, conflict));
            }
            Err(RootSessionSettingsApplyError::ActiveTree(message)) => {
                let conflict = DesktopCommandConflict::new(format!(
                    "root-session settings changed while the agent tree became active; keep the draft and retry after it finishes: {message}"
                ));
                return Err(command_conflict_error(&mut controller, conflict));
            }
            Err(RootSessionSettingsApplyError::Internal(error)) => {
                return Err(session_settings_storage_error(&mut controller, error));
            }
        };
        let operation_id = controller.state.begin_session_settings_persistence();
        let settlement_policy = SessionSettingsSettlementPolicy {
            access_only: persistence.access_only(),
        };
        controller
            .state
            .set_status_message("saving the exact root-session settings owner");
        (persistence, operation_id, settlement_policy)
    };

    // SQLite can wait on another process. Persist without holding the controller
    // lock; the registered mutation owner closes new in-process admissions.
    let persisted =
        match tauri::async_runtime::spawn_blocking(move || persistence.execute_blocking()).await {
            Ok(result) => result,
            Err(error) => Err(RootSessionSettingsApplyError::Internal(format!(
                "session settings persistence worker failed: {error}"
            ))),
        };

    let mut controller = controller.lock().await;
    controller.drain_runtime_messages();
    if !controller
        .state
        .finish_session_settings_persistence(operation_id)
    {
        if let Ok(outcome) = &persisted {
            let _ = rebase_session_settings_persistence_outcome(&mut controller, outcome);
        }
        let conflict = DesktopCommandConflict::new(
            "the session-settings persistence owner was superseded; keep the draft and reload",
        );
        return Err(command_conflict_error(&mut controller, conflict));
    }
    if let Err(conflict) = ensure_session_settings_settlement_target(
        &controller,
        &expected_target,
        settlement_policy,
        persisted.as_ref().ok(),
    ) {
        if let Ok(outcome) = &persisted {
            let _ = rebase_session_settings_persistence_outcome(&mut controller, outcome);
        }
        return Err(command_conflict_error(&mut controller, conflict));
    }
    let result = match persisted {
        Ok(RootSessionSettingsPersistenceOutcome::Applied(result)) => result,
        Ok(RootSessionSettingsPersistenceOutcome::Conflict { session, .. }) => {
            let _ = controller
                .state
                .apply_persisted_root_session_record(session);
            let conflict = DesktopCommandConflict::new(
                "saved root-session settings changed before Apply; keep the draft, review the refreshed saved state, and retry",
            );
            return Err(command_conflict_error(&mut controller, conflict));
        }
        Err(RootSessionSettingsApplyError::ActiveTree(message)) => {
            let conflict = DesktopCommandConflict::new(format!(
                "root-session settings changed while the agent tree became active; keep the draft and retry after it finishes: {message}"
            ));
            return Err(command_conflict_error(&mut controller, conflict));
        }
        Err(RootSessionSettingsApplyError::Internal(error)) => {
            return Err(session_settings_storage_error(&mut controller, error));
        }
    };
    if !controller.settle_root_session_settings_persistence(result) {
        let conflict = DesktopCommandConflict::new(
            "the root-session settings owner changed before settlement; reload the current chat",
        );
        return Err(command_conflict_error(&mut controller, conflict));
    }
    Ok((
        controller
            .next_web_state()
            .map_err(DesktopCommandError::internal)?,
        true,
    ))
}

#[tauri::command]
async fn toggle_access_mode(
    controller: State<'_, SharedController>,
    draft_values: Vec<DesktopConfigValueInput>,
    expected_target: DesktopAccessModeMutationTarget,
) -> Result<DesktopWebState, DesktopCommandError> {
    mutate_controller_checked(controller, |controller| {
        ensure_unscoped_prompt_review_action(controller, "access mode")?;
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
    remote_job_id: Option<String>,
    remote_profile_id: Option<String>,
) -> Result<DesktopWebState, DesktopCommandError> {
    if remote_job_id.is_some() || remote_profile_id.is_some() {
        let mut controller = controller.lock().await;
        controller.drain_runtime_messages();
        let target = remote_job_id
            .as_deref()
            .and_then(|job| job.parse::<ulid::Ulid>().ok())
            .zip(
                remote_profile_id
                    .as_deref()
                    .and_then(|profile| profile.parse::<ulid::Ulid>().ok()),
            )
            .zip(confirmation_id.parse::<ulid::Ulid>().ok());
        let resolved = target.is_some_and(|((job, profile), id)| {
            controller
                .state
                .device_network
                .as_ref()
                .is_some_and(|network| {
                    network.remote_jobs().answer_approval(
                        id,
                        job,
                        crate::mcp_publish::PublishProfileId(profile),
                        decision,
                    )
                })
        });
        return if resolved {
            controller
                .next_web_state()
                .map_err(DesktopCommandError::internal)
        } else {
            Err(command_conflict_error(
                &mut controller,
                DesktopCommandConflict::new(
                    "the receiver permission confirmation is no longer current",
                ),
            ))
        };
    }
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
    use std::collections::BTreeSet;

    use super::*;

    const TEST_ADMISSION_REVISION: u64 = 7;

    fn root_receipt(
        session_id: SessionId,
        turn_id: TurnId,
        admission_revision: u64,
    ) -> crate::runtime::RootAdmissionSnapshot {
        crate::runtime::RootAdmissionSnapshot {
            last_admitted: Some(crate::runtime::RootAdmissionReceipt {
                session_id,
                turn_id,
                revision: admission_revision,
            }),
            pending: None,
            stop_seal_id: None,
        }
    }

    #[test]
    fn desktop_command_manifest_is_unique_and_matches_the_frontend_guard() {
        let mut backend_names = BTreeSet::new();
        for name in DESKTOP_COMMAND_WIRE_NAMES {
            assert!(
                name.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
                "Desktop command wire name must be snake_case: {name}"
            );
            assert!(
                backend_names.insert((*name).to_string()),
                "duplicate Desktop command registration: {name}"
            );
        }

        let frontend_names: Vec<String> = serde_json::from_str(include_str!(
            "../../ui/desktop-web/src/desktop_commands.json"
        ))
        .expect("frontend Desktop command manifest must be valid JSON");
        let frontend_name_set = frontend_names.iter().cloned().collect::<BTreeSet<_>>();
        assert_eq!(
            frontend_name_set.len(),
            frontend_names.len(),
            "frontend Desktop command manifest contains a duplicate"
        );
        assert_eq!(
            frontend_name_set, backend_names,
            "frontend invoke guard and Tauri handler command names drifted"
        );
    }

    #[test]
    fn retired_manual_publish_commands_cannot_be_invoked_but_remote_stop_and_history_remain() {
        assert!(!DESKTOP_COMMAND_WIRE_NAMES.iter().any(|name| {
            *name == "show_mcp_publish_editor" || name.starts_with("mcp_publish_")
        }));
        for name in [
            "remote_job_cancel",
            "device_network_receiver",
            "mcp_history_list",
            "mcp_history_detail",
            "mcp_history_export",
            "mcp_history_stop",
        ] {
            assert!(DESKTOP_COMMAND_WIRE_NAMES.contains(&name), "{name}");
        }
    }

    #[test]
    fn draft_action_target_rejects_workspace_and_current_session_drift() {
        let expected = DesktopDraftActionTarget {
            workspace_path: "C:/workspace".to_string(),
            session_id: Some("session-a".to_string()),
            owner_generation: "7".to_string(),
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
    fn initial_setup_target_rejects_path_generation_and_workspace_drift() {
        let expected = DesktopInitialSetupMutationTarget {
            workspace_path: "C:/workspace".to_string(),
            global_config_path: "C:/config/config.toml".to_string(),
            setup_generation: "7".to_string(),
        };
        assert!(
            validate_initial_setup_mutation_target(
                &expected,
                "C:/workspace",
                "C:/config/config.toml",
                7,
            )
            .is_ok()
        );
        assert!(
            validate_initial_setup_mutation_target(
                &expected,
                "C:/other",
                "C:/config/config.toml",
                7,
            )
            .is_err()
        );
        assert!(
            validate_initial_setup_mutation_target(
                &expected,
                "C:/workspace",
                "C:/other/config.toml",
                7,
            )
            .is_err()
        );
        assert!(
            validate_initial_setup_mutation_target(
                &expected,
                "C:/workspace",
                "C:/config/config.toml",
                8,
            )
            .is_err()
        );
    }

    #[test]
    fn session_settings_target_rejects_every_root_owner_drift() {
        let expected = DesktopSessionSettingsMutationTarget {
            workspace_path: "C:/workspace".to_string(),
            root_session_id: "session-a".to_string(),
            settings_revision: "11".to_string(),
            config_generation: "13".to_string(),
            runtime_owner_token: "tree:17".to_string(),
        };
        assert!(
            validate_session_settings_mutation_target(
                &expected,
                "C:/workspace",
                "session-a",
                11,
                13,
                "tree:17",
            )
            .is_ok()
        );
        for (workspace, session, revision, generation, runtime) in [
            ("C:/other", "session-a", 11, 13, "tree:17"),
            ("C:/workspace", "session-b", 11, 13, "tree:17"),
            ("C:/workspace", "session-a", 12, 13, "tree:17"),
            ("C:/workspace", "session-a", 11, 14, "tree:17"),
            ("C:/workspace", "session-a", 11, 13, "idle:17"),
        ] {
            assert!(
                validate_session_settings_mutation_target(
                    &expected, workspace, session, revision, generation, runtime,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn access_only_session_settings_error_settlement_uses_the_captured_same_epoch_policy() {
        let target = DesktopSessionSettingsMutationTarget {
            workspace_path: "C:/workspace".to_string(),
            root_session_id: "session-a".to_string(),
            settings_revision: "11".to_string(),
            config_generation: "13".to_string(),
            runtime_owner_token: "tree:17".to_string(),
        };

        assert!(
            validate_session_settings_settlement_target(
                &target,
                "C:/workspace",
                "session-a",
                11,
                13,
                "tree:17",
                true,
                None,
            )
            .is_ok(),
            "an access-only operation keeps its policy even when no successful worker outcome exists"
        );
        assert!(
            validate_session_settings_settlement_target(
                &target,
                "C:/workspace",
                "session-a",
                11,
                13,
                "idle:17",
                true,
                None,
            )
            .is_ok(),
            "an access-only write may settle after its captured tree reaches idle"
        );
        assert!(
            validate_session_settings_settlement_target(
                &target,
                "C:/workspace",
                "session-a",
                11,
                13,
                "root:17",
                true,
                None,
            )
            .is_ok(),
            "an access-only write may settle when its captured child tree completes and the same root resumes"
        );
        assert!(
            validate_session_settings_settlement_target(
                &target,
                "C:/workspace",
                "session-a",
                11,
                13,
                "idle:17",
                false,
                None,
            )
            .is_err(),
            "turn-config writes retain strict runtime-token equality"
        );
        for current in ["root:18", "tree:18", "idle:18"] {
            assert!(
                validate_session_settings_settlement_target(
                    &target,
                    "C:/workspace",
                    "session-a",
                    11,
                    13,
                    current,
                    true,
                    None,
                )
                .is_err(),
                "a new or non-terminal runtime owner must be rejected: {current}"
            );
        }
        assert!(
            validate_session_settings_settlement_target(
                &target,
                "C:/workspace",
                "session-b",
                11,
                13,
                "idle:17",
                true,
                None,
            )
            .is_err(),
            "same-epoch completion never relaxes the durable session owner"
        );
        assert!(
            validate_session_settings_settlement_target(
                &target,
                "C:/other",
                "session-a",
                11,
                13,
                "idle:17",
                true,
                None,
            )
            .is_err(),
            "same-epoch completion never relaxes the workspace owner"
        );
        assert!(
            validate_session_settings_settlement_target(
                &target,
                "C:/workspace",
                "session-a",
                11,
                14,
                "idle:17",
                true,
                None,
            )
            .is_err(),
            "same-epoch completion never relaxes the config-generation owner"
        );

        let root_target = DesktopSessionSettingsMutationTarget {
            runtime_owner_token: "root:19".to_string(),
            ..target
        };
        for current in ["tree:19", "idle:19"] {
            assert!(
                validate_session_settings_settlement_target(
                    &root_target,
                    "C:/workspace",
                    "session-a",
                    11,
                    13,
                    current,
                    true,
                    None,
                )
                .is_ok()
            );
        }

        let idle_target = DesktopSessionSettingsMutationTarget {
            runtime_owner_token: "idle:20".to_string(),
            ..root_target
        };
        for current in ["root:20", "tree:20"] {
            assert!(
                validate_session_settings_settlement_target(
                    &idle_target,
                    "C:/workspace",
                    "session-a",
                    11,
                    13,
                    current,
                    true,
                    None,
                )
                .is_err(),
                "an idle owner cannot adopt a later active phase: {current}"
            );
        }
    }

    #[test]
    fn own_settings_write_settles_at_its_result_revision_only_after_exact_projection() {
        let target = DesktopSessionSettingsMutationTarget {
            workspace_path: "C:/workspace".to_string(),
            root_session_id: "session-a".to_string(),
            settings_revision: "11".to_string(),
            config_generation: "13".to_string(),
            runtime_owner_token: "idle:17".to_string(),
        };

        assert!(
            validate_session_settings_settlement_target(
                &target,
                "C:/workspace",
                "session-a",
                12,
                13,
                "idle:17",
                false,
                None,
            )
            .is_err(),
            "a result revision is not accepted until the caller proves that the exact settings projection is already current"
        );
        assert!(
            validate_session_settings_settlement_target(
                &target,
                "C:/workspace",
                "session-a",
                12,
                13,
                "idle:17",
                false,
                Some(AlreadyProjectedSessionSettingsWrite {
                    settings_revision: 12,
                    next_config_generation: None,
                }),
            )
            .is_ok(),
            "the command's own N+1 projection may settle idempotently"
        );
        assert!(
            validate_session_settings_settlement_target(
                &target,
                "C:/workspace",
                "session-a",
                12,
                14,
                "idle:17",
                false,
                Some(AlreadyProjectedSessionSettingsWrite {
                    settings_revision: 12,
                    next_config_generation: None,
                }),
            )
            .is_err(),
            "a durable or effective no-op cannot claim an unrelated config-generation increment"
        );
        assert!(
            validate_session_settings_settlement_target(
                &target,
                "C:/workspace",
                "session-a",
                12,
                14,
                "idle:17",
                false,
                Some(AlreadyProjectedSessionSettingsWrite {
                    settings_revision: 12,
                    next_config_generation: Some(14),
                }),
            )
            .is_ok(),
            "an exact settings result may settle after applying its one expected effective-config generation"
        );
        assert!(
            validate_session_settings_settlement_target(
                &target,
                "C:/workspace",
                "session-a",
                12,
                15,
                "idle:17",
                false,
                Some(AlreadyProjectedSessionSettingsWrite {
                    settings_revision: 12,
                    next_config_generation: Some(14),
                }),
            )
            .is_err(),
            "a later unrelated config generation remains a conflict"
        );
        assert!(
            validate_session_settings_settlement_target(
                &target,
                "C:/workspace",
                "session-a",
                13,
                13,
                "idle:17",
                false,
                Some(AlreadyProjectedSessionSettingsWrite {
                    settings_revision: 12,
                    next_config_generation: None,
                }),
            )
            .is_err(),
            "a later settings revision cannot be adopted as this command's result"
        );
    }

    #[test]
    fn session_settings_blank_local_context_does_not_resave_legacy_generation_values() {
        let current = crate::session::SessionModelParameters {
            temperature: Some(0.4),
            top_p: Some(0.8),
            top_k: Some(40),
            context_window: Some(65_536),
            max_output_tokens: Some(4_096),
        };

        let patch = session_model_parameter_patch(&current, None);
        let next = patch.apply_to_model_parameters(&current);

        assert!(patch.reset_model_parameters);
        assert_eq!(next.temperature, None);
        assert_eq!(next.top_p, None);
        assert_eq!(next.top_k, None);
        assert_eq!(next.context_window, None);
        assert_eq!(next.max_output_tokens, None);
        assert_eq!(
            parse_optional_session_settings_u32("context window", "  ", true)
                .expect("blank inherits"),
            None
        );
    }

    #[test]
    fn unchanged_session_local_context_does_not_resave_legacy_model_parameters() {
        let current = crate::session::SessionModelParameters {
            temperature: Some(0.4),
            top_p: Some(0.8),
            top_k: Some(40),
            context_window: None,
            max_output_tokens: Some(4_096),
        };

        let mut patch = session_model_parameter_patch(&current, None);
        patch.access_mode = Some(crate::config::AccessMode::FullAccess);

        assert!(!patch.reset_model_parameters);
        let next = patch.apply_to_model_parameters(&current);
        assert_eq!(next.context_window, current.context_window);
        assert_eq!(next.temperature, None);
        assert_eq!(next.top_p, None);
        assert_eq!(next.top_k, None);
        assert_eq!(next.max_output_tokens, None);
    }

    #[test]
    fn canonical_session_settings_no_op_is_an_explicit_success_without_a_revision_write() {
        let session = crate::session::SessionRecord {
            id: SessionId::new(),
            project_id: crate::session::ProjectId::new(),
            title: "session".to_string(),
            status: crate::session::SessionStatus::Running,
            cwd: camino::Utf8PathBuf::from("C:/workspace"),
            model: "current-model".to_string(),
            base_url: "http://127.0.0.1:1234".to_string(),
            provider_connection: Some(crate::session::SessionProviderConnection {
                profile: ProviderProfile::LmStudio,
                api_key_env: None,
                extra_headers: Default::default(),
            }),
            access_mode: crate::config::AccessMode::Default,
            model_parameters: crate::session::SessionModelParameters::default(),
            session_settings_revision: 7,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: None,
        };
        let base_url = crate::config::ProviderEndpoint::parse(" http://127.0.0.1:1234/ ")
            .expect("canonical endpoint")
            .as_str()
            .to_string();
        let (patch, changes_turn_config) = session_settings_patch_for_values(
            &session,
            base_url,
            " current-model ".trim().to_string(),
            session
                .provider_connection
                .clone()
                .expect("provider snapshot"),
            session.access_mode,
            None,
        );

        assert!(patch.is_empty());
        assert!(!changes_turn_config);
        assert_eq!(session.session_settings_revision, 7);
    }

    #[test]
    fn provider_api_key_environment_input_is_canonical_and_error_safe() {
        assert_eq!(
            canonical_provider_api_key_env_input("  OPENAI_API_KEY_1  ")
                .expect("valid environment name"),
            Some("OPENAI_API_KEY_1".to_string())
        );
        assert_eq!(
            canonical_provider_api_key_env_input("  ").expect("blank disables API key lookup"),
            None
        );
        let invalid = "must-not-appear-in-errors";
        let error = canonical_provider_api_key_env_input(invalid)
            .expect_err("invalid environment variable name");
        assert_eq!(
            error.message,
            "the API key environment variable name is invalid"
        );
        assert!(!error.message.contains(invalid));
    }

    #[test]
    fn session_settings_clear_hidden_headers_when_connection_target_changes() {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert(
            "Authorization".to_string(),
            "must-not-cross-provider-targets".to_string(),
        );
        let session = crate::session::SessionRecord {
            id: SessionId::new(),
            project_id: crate::session::ProjectId::new(),
            title: "session".to_string(),
            status: crate::session::SessionStatus::Completed,
            cwd: camino::Utf8PathBuf::from("C:/workspace"),
            model: "current-model".to_string(),
            base_url: "http://127.0.0.1:1234".to_string(),
            provider_connection: Some(crate::session::SessionProviderConnection {
                profile: ProviderProfile::LmStudio,
                api_key_env: Some("OLD_KEY".to_string()),
                extra_headers: headers.clone(),
            }),
            access_mode: crate::config::AccessMode::Default,
            model_parameters: crate::session::SessionModelParameters::default(),
            session_settings_revision: 7,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: Some(1),
        };
        let effective = ResolvedConfig::default().model;

        assert_eq!(
            session_settings_extra_headers_for_target(
                &session,
                &effective,
                "http://127.0.0.1:1234",
                ProviderProfile::LmStudio,
            ),
            headers,
            "same endpoint/profile keeps headers when only model, key, or limits change"
        );
        assert!(
            session_settings_extra_headers_for_target(
                &session,
                &effective,
                "http://127.0.0.1:8119",
                ProviderProfile::LmStudio,
            )
            .is_empty(),
            "an endpoint change cannot inherit hidden headers"
        );
        assert!(
            session_settings_extra_headers_for_target(
                &session,
                &effective,
                "http://127.0.0.1:1234",
                ProviderProfile::OpenAiCompatible,
            )
            .is_empty(),
            "a connection-profile change cannot inherit hidden headers"
        );
    }

    #[test]
    fn initial_setup_import_draft_serializes_only_public_sensitive_state() {
        let mut config = ResolvedConfig::default();
        let prompt_marker = "INITIAL_SETUP_MAIN_PROMPT_MARKER";
        config.model.system_prompt = prompt_marker.to_string();
        let secrets = [
            "model-header-import-secret",
            "model-body-import-secret",
            "docling-header-import-secret",
            "mcp-header-import-secret",
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

        let mut values = vec![DesktopInitialSetupConfigFieldDraft {
            key: "model.model".to_string(),
            text: "draft-model".to_string(),
            sensitive: false,
            configured: true,
        }];
        values.push(DesktopInitialSetupConfigFieldDraft {
            key: crate::config::ConfigField::SystemPrompt.label().to_string(),
            text: config.model.system_prompt.clone(),
            sensitive: false,
            configured: true,
        });
        values.extend(
            [
                crate::config::ConfigField::ExtraHeadersJson,
                crate::config::ConfigField::ExtraBodyJson,
                crate::config::ConfigField::DoclingHeadersJson,
                crate::config::ConfigField::McpServersJson,
            ]
            .into_iter()
            .map(|field| {
                let public = field.public_value(&config);
                DesktopInitialSetupConfigFieldDraft {
                    key: field.label().to_string(),
                    text: public.value,
                    sensitive: public.sensitive,
                    configured: public.configured,
                }
            }),
        );
        let payload = DesktopInitialSetupConfigDraft {
            source_path: "C:/config/import.toml".to_string(),
            import_generation: "9".to_string(),
            values,
        };

        let debug = format!("{payload:?}");
        let serialized = serde_json::to_value(&payload).expect("serialize import draft");
        assert_eq!(serialized["sourcePath"], "C:/config/import.toml");
        assert_eq!(serialized["importGeneration"], "9");
        assert_eq!(serialized["values"][0]["key"], "model.model");
        assert_eq!(serialized["values"][0]["text"], "draft-model");
        assert_eq!(serialized["values"][0]["sensitive"], false);
        assert_eq!(serialized["values"][0]["configured"], true);
        assert_eq!(serialized["values"][1]["text"], prompt_marker);

        let sensitive = serialized["values"]
            .as_array()
            .expect("import values")
            .iter()
            .filter(|value| value["sensitive"] == true)
            .collect::<Vec<_>>();
        assert_eq!(sensitive.len(), 4);
        for value in sensitive {
            assert_eq!(value["text"], "");
            assert_eq!(value["configured"], true);
        }

        let encoded = serde_json::to_string(&payload).expect("encode import draft");
        for secret in secrets {
            assert!(!encoded.contains(secret));
            assert!(!debug.contains(secret));
        }
        assert!(encoded.contains(prompt_marker));
        assert!(!debug.contains(prompt_marker));
        assert!(debug.contains("text_chars"));
    }

    #[test]
    fn initial_setup_import_generation_accepts_only_optional_canonical_unsigned_decimal() {
        assert_eq!(
            parse_initial_setup_import_generation(None).expect("manual setup has no import owner"),
            None
        );
        assert_eq!(
            parse_initial_setup_import_generation(Some("9")).expect("canonical import generation"),
            Some(9)
        );

        for invalid in ["09", "-1", "+9", " 9", "9 ", ""] {
            assert!(
                parse_initial_setup_import_generation(Some(invalid)).is_err(),
                "noncanonical import generation must fail closed: {invalid:?}"
            );
        }
    }

    #[test]
    fn side_chat_owner_fence_accepts_only_optional_canonical_nonnegative_decimal() {
        assert_eq!(
            parse_side_chat_append_position(None, "owner context")
                .expect("an absent owner fence is allowed"),
            None
        );
        assert_eq!(
            parse_side_chat_append_position(Some("0"), "owner context").expect("zero is canonical"),
            Some(0)
        );
        assert_eq!(
            parse_side_chat_append_position(Some("42"), "owner context")
                .expect("canonical owner fence"),
            Some(42)
        );

        for invalid in ["042", "-1", "+1", " 1", "1 ", ""] {
            assert!(
                parse_side_chat_append_position(Some(invalid), "owner context").is_err(),
                "noncanonical owner fence must fail closed: {invalid:?}"
            );
        }
    }

    #[test]
    fn side_chat_quote_parser_requires_typed_source_and_canonical_source_fence() {
        let history_item_id = crate::protocol::HistoryItemId::new();
        let quote = parse_side_chat_quote(SideChatQuoteInput {
            source_kind: "artifact".to_string(),
            source_history_item_id: history_item_id.to_string(),
            source_append_position: Some("17".to_string()),
            selected_text: "selected evidence".to_string(),
        })
        .expect("canonical typed quote");
        assert_eq!(quote.source_kind, SideChatQuoteSourceKind::Artifact);
        assert_eq!(quote.source_history_item_id, history_item_id);
        assert_eq!(quote.source_append_position, Some(17));
        assert_eq!(quote.selected_text, "selected evidence");

        for invalid_position in ["017", "-1", "+17", " 17"] {
            assert!(
                parse_side_chat_quote(SideChatQuoteInput {
                    source_kind: "transcript".to_string(),
                    source_history_item_id: history_item_id.to_string(),
                    source_append_position: Some(invalid_position.to_string()),
                    selected_text: "selected evidence".to_string(),
                })
                .is_err(),
                "noncanonical quote source fence must fail closed: {invalid_position:?}"
            );
        }

        for invalid_kind in ["Artifact", "unknown", ""] {
            assert!(
                parse_side_chat_quote(SideChatQuoteInput {
                    source_kind: invalid_kind.to_string(),
                    source_history_item_id: history_item_id.to_string(),
                    source_append_position: Some("17".to_string()),
                    selected_text: "selected evidence".to_string(),
                })
                .is_err(),
                "untyped quote source kind must fail closed: {invalid_kind:?}"
            );
        }
        assert!(
            parse_side_chat_quote(SideChatQuoteInput {
                source_kind: "transcript".to_string(),
                source_history_item_id: "not-a-history-item-id".to_string(),
                source_append_position: None,
                selected_text: "selected evidence".to_string(),
            })
            .is_err(),
            "a malformed quote source identity must fail closed"
        );
        assert!(
            serde_json::from_value::<SideChatQuoteInput>(serde_json::json!({
                "sourceKind": "transcript",
                "sourceHistoryItemId": history_item_id.to_string(),
                "sourceAppendPosition": "17",
                "selectedText": "selected evidence",
                "unexpected": true
            }))
            .is_err(),
            "the quote wire DTO must reject unknown fields"
        );
    }

    #[test]
    fn prompt_review_target_rejects_request_aba_and_every_owner_drift() {
        let expected: DesktopPromptReviewMutationTarget =
            serde_json::from_value(serde_json::json!({
                "workspacePath": "C:/workspace",
                "sessionId": "session-a",
                "ownerGeneration": "7",
                "requestId": "41",
                "expectedState": {
                    "kind": "idle",
                    "latestTurnId": null,
                    "admissionRevision": "0"
                },
            }))
            .expect("deserialize exact review target");
        assert_eq!(
            validate_prompt_review_mutation_target(
                &expected,
                "C:/workspace",
                Some("session-a".to_string()),
                7,
                Some(41),
                Some(ActiveTurnExpectation::initial_idle()),
            )
            .expect("current review target"),
            41
        );
        for (workspace, session_id, owner_generation, request_id) in [
            ("C:/other", Some("session-a".to_string()), 7, Some(41)),
            ("C:/workspace", Some("session-b".to_string()), 7, Some(41)),
            ("C:/workspace", Some("session-a".to_string()), 8, Some(41)),
            ("C:/workspace", Some("session-a".to_string()), 7, Some(42)),
            ("C:/workspace", Some("session-a".to_string()), 7, None),
        ] {
            assert!(
                validate_prompt_review_mutation_target(
                    &expected,
                    workspace,
                    session_id,
                    owner_generation,
                    request_id,
                    Some(ActiveTurnExpectation::initial_idle()),
                )
                .is_err()
            );
        }

        let mut noncanonical = expected.clone();
        noncanonical.request_id = "041".to_string();
        assert!(
            validate_prompt_review_mutation_target(
                &noncanonical,
                "C:/workspace",
                Some("session-a".to_string()),
                7,
                Some(41),
                Some(ActiveTurnExpectation::initial_idle()),
            )
            .is_err(),
            "the JS boundary accepts only canonical decimal request identities"
        );
        let mut noncanonical_generation = expected.clone();
        noncanonical_generation.owner_generation = "007".to_string();
        assert!(
            validate_prompt_review_mutation_target(
                &noncanonical_generation,
                "C:/workspace",
                Some("session-a".to_string()),
                7,
                Some(41),
                Some(ActiveTurnExpectation::initial_idle()),
            )
            .is_err(),
            "owner generation must use canonical decimal u64 spelling"
        );
        assert!(
            serde_json::from_value::<DesktopPromptReviewMutationTarget>(serde_json::json!({
                "workspacePath": "C:/workspace",
                "sessionId": "session-a",
                "ownerGeneration": 7,
                "requestId": "41",
                "expectedState": {
                    "kind": "idle",
                    "latestTurnId": null,
                    "admissionRevision": "0"
                },
            }))
            .is_err(),
            "JSON numbers must not cross the exact u64 owner boundary"
        );
        let mut malformed = expected;
        malformed.request_id = "not-a-request".to_string();
        assert!(
            validate_prompt_review_mutation_target(
                &malformed,
                "C:/workspace",
                Some("session-a".to_string()),
                7,
                Some(41),
                Some(ActiveTurnExpectation::initial_idle()),
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<DesktopPromptReviewMutationTarget>(serde_json::json!({
                "workspacePath": "C:/workspace",
                "sessionId": "session-a",
                "ownerGeneration": "7",
                "requestId": 41,
                "expectedState": {
                    "kind": "idle",
                    "latestTurnId": null,
                    "admissionRevision": "0"
                },
            }))
            .is_err(),
            "review request identity must not cross JavaScript as a number"
        );
    }

    #[test]
    fn run_and_prompt_review_targets_deserialize_exact_nested_expected_state_keys() {
        let session_id = SessionId::new().to_string();
        let idle_turn_id = TurnId::new();
        let active_turn_id = TurnId::new();
        for (expected_json, expected) in [
            (
                serde_json::json!({
                    "kind": "idle",
                    "latestTurnId": idle_turn_id.to_string(),
                    "admissionRevision": "18446744073709551614",
                }),
                ActiveTurnExpectation::Idle {
                    latest_turn_id: Some(idle_turn_id),
                    revision: u64::MAX - 1,
                },
            ),
            (
                serde_json::json!({
                    "kind": "turn",
                    "turnId": active_turn_id.to_string(),
                    "admissionRevision": "18446744073709551615",
                }),
                ActiveTurnExpectation::Turn {
                    turn_id: active_turn_id,
                    revision: u64::MAX,
                },
            ),
        ] {
            let run_json = serde_json::json!({
                "workspacePath": "C:/workspace",
                "sessionId": session_id.clone(),
                "runtimeOwnerToken": "root:9",
                "permissionConfirmationId": "41",
                "expectedState": expected_json.clone(),
            });
            let run: DesktopRunMutationTarget = serde_json::from_value(run_json.clone())
                .expect("Tauri accepts exact run target keys");
            assert_eq!(
                run.expected_state.parse().expect("parse expected state"),
                expected
            );

            let review_json = serde_json::json!({
                "workspacePath": "C:/workspace",
                "sessionId": session_id.clone(),
                "ownerGeneration": "18446744073709551615",
                "requestId": "42",
                "expectedState": expected_json,
            });
            let review: DesktopPromptReviewMutationTarget =
                serde_json::from_value(review_json.clone())
                    .expect("Tauri accepts exact Prompt Review target keys");
            assert_eq!(
                review
                    .expected_state
                    .parse()
                    .expect("parse review expected state"),
                expected
            );

            let mut malformed_run = run_json;
            malformed_run
                .as_object_mut()
                .expect("run target object")
                .insert("legacyOwner".to_string(), serde_json::json!(true));
            assert!(
                serde_json::from_value::<DesktopRunMutationTarget>(malformed_run).is_err(),
                "unknown run target keys must fail closed"
            );
            let mut malformed_review = review_json;
            malformed_review
                .as_object_mut()
                .expect("review target object")
                .insert("legacyOwner".to_string(), serde_json::json!(true));
            assert!(
                serde_json::from_value::<DesktopPromptReviewMutationTarget>(malformed_review)
                    .is_err(),
                "unknown Prompt Review target keys must fail closed"
            );
            let missing_revision = serde_json::json!({
                "workspacePath": "C:/workspace",
                "sessionId": session_id.clone(),
                "runtimeOwnerToken": "root:9",
                "permissionConfirmationId": "41",
                "expectedState": { "kind": "idle", "latestTurnId": null },
            });
            assert!(
                serde_json::from_value::<DesktopRunMutationTarget>(missing_revision).is_err(),
                "admissionRevision is required on every expectedState variant"
            );
        }
    }

    #[test]
    fn generic_overlay_close_cannot_bypass_prompt_review_request_identity() {
        assert!(validate_unscoped_overlay_close(DesktopOverlay::ConfigEditor).is_ok());
        assert!(validate_unscoped_overlay_close(DesktopOverlay::PromptReview).is_err());
    }

    #[test]
    fn run_mutation_target_rejects_owner_drift_with_operation_neutral_guidance() {
        let expected = DesktopRunMutationTarget {
            workspace_path: "C:/workspace".to_string(),
            session_id: Some("session-a".to_string()),
            runtime_owner_token: "root:11".to_string(),
            permission_confirmation_id: Some("41".to_string()),
            expected_state: DesktopRunExpectedState::Idle {
                latest_turn_id: None,
                admission_revision: "0".to_string(),
            },
        };
        assert!(
            validate_run_mutation_target(
                &expected,
                "C:/workspace",
                Some("session-a".to_string()),
                "root:11".to_string(),
                Some("41".to_string()),
                ActiveTurnExpectation::initial_idle(),
            )
            .is_ok()
        );
        for (workspace, session_id, runtime_owner_token, permission_confirmation_id) in [
            (
                "C:/other",
                Some("session-a".to_string()),
                "root:11".to_string(),
                Some("41".to_string()),
            ),
            (
                "C:/workspace",
                Some("session-b".to_string()),
                "root:11".to_string(),
                Some("41".to_string()),
            ),
            (
                "C:/workspace",
                Some("session-a".to_string()),
                "root:12".to_string(),
                Some("41".to_string()),
            ),
            (
                "C:/workspace",
                Some("session-a".to_string()),
                "root:11".to_string(),
                Some("42".to_string()),
            ),
        ] {
            let conflict = validate_run_mutation_target(
                &expected,
                workspace,
                session_id,
                runtime_owner_token,
                permission_confirmation_id,
                ActiveTurnExpectation::initial_idle(),
            )
            .expect_err("a stale run mutation owner must be rejected");
            assert_eq!(
                conflict.message,
                "the active run owner changed before the action was applied; review the current task and try again"
            );
        }
    }

    #[test]
    fn stop_target_rejects_permission_confirmation_aba() {
        let stale_permission_a = DesktopRunMutationTarget {
            workspace_path: "C:/workspace".to_string(),
            session_id: Some("session-a".to_string()),
            runtime_owner_token: "root:11".to_string(),
            permission_confirmation_id: Some("41".to_string()),
            expected_state: DesktopRunExpectedState::Idle {
                latest_turn_id: None,
                admission_revision: "0".to_string(),
            },
        };

        assert!(
            validate_run_mutation_target(
                &stale_permission_a,
                "C:/workspace",
                Some("session-a".to_string()),
                "root:11".to_string(),
                Some("42".to_string()),
                ActiveTurnExpectation::initial_idle(),
            )
            .is_err(),
            "a stale Stop rendered for permission A must not abort replacement permission B"
        );
    }

    #[test]
    fn stop_target_allows_only_same_turn_terminal_crossing_within_one_runtime_epoch() {
        let session_a = SessionId::new();
        let turn_a = TurnId::new();
        let turn_b = TurnId::new();
        let expected = DesktopStopMutationTarget::Turn {
            workspace_path: "C:/workspace".to_string(),
            session_id: session_a.to_string(),
            turn_id: turn_a.to_string(),
            admission_revision: TEST_ADMISSION_REVISION.to_string(),
            root_epoch: "11".to_string(),
        };

        assert_eq!(
            validate_stop_mutation_target(
                &expected,
                "C:/workspace",
                Some(session_a.to_string()),
                Some(11),
                11,
                None,
                ActiveTurnExpectation::Turn {
                    turn_id: turn_b,
                    revision: TEST_ADMISSION_REVISION + 1,
                },
                Some(root_receipt(session_a, turn_b, TEST_ADMISSION_REVISION + 1,)),
            )
            .expect("a live same-epoch root preserves Stop across continuation A to B"),
            DesktopStopAdmission::Root { generation: 11 },
        );
        assert_eq!(
            validate_stop_mutation_target(
                &expected,
                "C:/workspace",
                Some(session_a.to_string()),
                None,
                11,
                None,
                ActiveTurnExpectation::Idle {
                    latest_turn_id: Some(turn_a),
                    revision: TEST_ADMISSION_REVISION,
                },
                None,
            )
            .expect("detached descendants retain the exact terminal-latest A Stop"),
            DesktopStopAdmission::Turn {
                turn_id: turn_a,
                admission_revision: TEST_ADMISSION_REVISION,
            },
        );

        for (root_generation, last_epoch, active_turn_expectation) in [
            (
                None,
                11,
                ActiveTurnExpectation::Idle {
                    latest_turn_id: Some(turn_b),
                    revision: TEST_ADMISSION_REVISION + 1,
                },
            ),
            (
                None,
                12,
                ActiveTurnExpectation::Idle {
                    latest_turn_id: Some(turn_a),
                    revision: TEST_ADMISSION_REVISION,
                },
            ),
        ] {
            assert!(
                validate_stop_mutation_target(
                    &expected,
                    "C:/workspace",
                    Some(session_a.to_string()),
                    root_generation,
                    last_epoch,
                    None,
                    active_turn_expectation,
                    None,
                )
                .is_err(),
                "B or a new runtime epoch must reject the captured A Stop"
            );
        }
        assert!(
            validate_stop_mutation_target(
                &expected,
                "C:/other",
                Some(session_a.to_string()),
                None,
                11,
                None,
                ActiveTurnExpectation::Idle {
                    latest_turn_id: Some(turn_a),
                    revision: TEST_ADMISSION_REVISION,
                },
                None,
            )
            .is_err()
        );
        assert!(
            validate_stop_mutation_target(
                &expected,
                "C:/workspace",
                Some(SessionId::new().to_string()),
                None,
                11,
                None,
                ActiveTurnExpectation::Idle {
                    latest_turn_id: Some(turn_a),
                    revision: TEST_ADMISSION_REVISION,
                },
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn root_stop_promotes_only_the_same_generation_admission_receipt() {
        let session_a = SessionId::new();
        let turn_a = TurnId::new();
        let turn_b = TurnId::new();
        let expected = DesktopStopMutationTarget::Root {
            workspace_path: "C:/workspace".to_string(),
            session_id: Some(session_a.to_string()),
            root_generation: "11".to_string(),
            latest_turn_id: Some(turn_a.to_string()),
            admission_revision: TEST_ADMISSION_REVISION.to_string(),
            permission_confirmation_id: Some("41".to_string()),
        };
        assert_eq!(
            validate_stop_mutation_target(
                &expected,
                "C:/workspace",
                Some(session_a.to_string()),
                Some(11),
                11,
                Some("41".to_string()),
                ActiveTurnExpectation::Idle {
                    latest_turn_id: Some(turn_a),
                    revision: TEST_ADMISSION_REVISION,
                },
                None,
            )
            .expect("root before admission"),
            DesktopStopAdmission::Root { generation: 11 },
        );
        assert!(
            validate_stop_mutation_target(
                &expected,
                "C:/other",
                Some(session_a.to_string()),
                Some(11),
                11,
                Some("41".to_string()),
                ActiveTurnExpectation::Idle {
                    latest_turn_id: Some(turn_a),
                    revision: TEST_ADMISSION_REVISION,
                },
                None,
            )
            .is_err(),
            "a pre-admission Root Stop cannot cross its captured workspace"
        );
        assert_eq!(
            validate_stop_mutation_target(
                &expected,
                "C:/workspace",
                Some(session_a.to_string()),
                Some(11),
                11,
                Some("41".to_string()),
                ActiveTurnExpectation::Idle {
                    latest_turn_id: Some(turn_a),
                    revision: TEST_ADMISSION_REVISION,
                },
                Some(root_receipt(session_a, turn_a, TEST_ADMISSION_REVISION)),
            )
            .expect("a canonical receipt must promote terminal-latest A to a durable Turn Stop"),
            DesktopStopAdmission::Root { generation: 11 },
        );
        assert!(
            validate_stop_mutation_target(
                &expected,
                "C:/other",
                Some(session_a.to_string()),
                Some(11),
                11,
                Some("41".to_string()),
                ActiveTurnExpectation::Idle {
                    latest_turn_id: Some(turn_a),
                    revision: TEST_ADMISSION_REVISION,
                },
                Some(root_receipt(session_a, turn_a, TEST_ADMISSION_REVISION)),
            )
            .is_err(),
            "a receipt-backed Root Stop cannot cross its captured workspace"
        );
        assert_eq!(
            validate_stop_mutation_target(
                &expected,
                "C:/workspace",
                Some(session_a.to_string()),
                Some(11),
                11,
                Some("41".to_string()),
                ActiveTurnExpectation::Idle {
                    latest_turn_id: Some(turn_a),
                    revision: TEST_ADMISSION_REVISION,
                },
                Some(root_receipt(session_a, turn_b, TEST_ADMISSION_REVISION + 1,)),
            )
            .expect("the same root continuation receipt outranks a lagging Idle(A) projection"),
            DesktopStopAdmission::Root { generation: 11 },
        );
        assert_eq!(
            validate_stop_mutation_target(
                &expected,
                "C:/workspace",
                Some(session_a.to_string()),
                Some(11),
                11,
                Some("41".to_string()),
                ActiveTurnExpectation::Turn {
                    turn_id: turn_b,
                    revision: TEST_ADMISSION_REVISION + 1,
                },
                Some(root_receipt(session_a, turn_b, TEST_ADMISSION_REVISION + 1,)),
            )
            .expect("same root UserTurnStored may overtake the queued Stop"),
            DesktopStopAdmission::Root { generation: 11 },
        );
        for (root_generation, permission, receipt) in [
            (
                Some(12),
                Some("41".to_string()),
                Some(root_receipt(session_a, turn_b, TEST_ADMISSION_REVISION + 1)),
            ),
            (
                Some(11),
                Some("42".to_string()),
                Some(root_receipt(session_a, turn_b, TEST_ADMISSION_REVISION + 1)),
            ),
            (Some(11), Some("41".to_string()), None),
        ] {
            assert!(
                validate_stop_mutation_target(
                    &expected,
                    "C:/workspace",
                    Some(session_a.to_string()),
                    root_generation,
                    root_generation.unwrap_or(11),
                    permission,
                    ActiveTurnExpectation::Turn {
                        turn_id: turn_b,
                        revision: TEST_ADMISSION_REVISION + 1,
                    },
                    receipt,
                )
                .is_err()
            );
        }

        let new_session_target = DesktopStopMutationTarget::Root {
            workspace_path: "C:/workspace".to_string(),
            session_id: None,
            root_generation: "11".to_string(),
            latest_turn_id: None,
            admission_revision: "0".to_string(),
            permission_confirmation_id: None,
        };
        assert!(
            validate_stop_mutation_target(
                &new_session_target,
                "C:/other",
                None,
                Some(11),
                11,
                None,
                ActiveTurnExpectation::Idle {
                    latest_turn_id: None,
                    revision: 0,
                },
                Some(root_receipt(session_a, turn_b, 1)),
            )
            .is_err(),
            "a new-session Root Stop cannot adopt a receipt from another workspace"
        );
        assert_eq!(
            validate_stop_mutation_target(
                &new_session_target,
                "C:/workspace",
                Some(session_a.to_string()),
                Some(11),
                11,
                None,
                ActiveTurnExpectation::Turn {
                    turn_id: turn_b,
                    revision: 1,
                },
                Some(root_receipt(session_a, turn_b, 1)),
            )
            .expect("the exact root receipt owns new-session adoption"),
            DesktopStopAdmission::Root { generation: 11 },
        );
        assert_eq!(
            validate_stop_mutation_target(
                &new_session_target,
                "C:/workspace",
                None,
                Some(11),
                11,
                None,
                ActiveTurnExpectation::Idle {
                    latest_turn_id: None,
                    revision: 0,
                },
                Some(root_receipt(session_a, turn_b, 1)),
            )
            .expect("same-control receipt owns admission before new-session projection arrives"),
            DesktopStopAdmission::Root { generation: 11 },
        );
        assert!(
            validate_stop_mutation_target(
                &new_session_target,
                "C:/workspace",
                Some(SessionId::new().to_string()),
                Some(11),
                11,
                None,
                ActiveTurnExpectation::Idle {
                    latest_turn_id: None,
                    revision: 0,
                },
                Some(root_receipt(session_a, turn_b, 1)),
            )
            .is_err(),
            "a same-generation receipt cannot adopt a different projected session"
        );
        assert!(
            validate_stop_mutation_target(
                &expected,
                "C:/workspace",
                None,
                Some(11),
                11,
                Some("41".to_string()),
                ActiveTurnExpectation::Idle {
                    latest_turn_id: Some(turn_a),
                    revision: TEST_ADMISSION_REVISION,
                },
                Some(root_receipt(session_a, turn_b, TEST_ADMISSION_REVISION + 1,)),
            )
            .is_err(),
            "an existing-session Root target cannot be adopted through a missing projection"
        );
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
    fn agent_execution_target_uses_camel_case_input_and_snake_case_output() {
        let root_session_id = SessionId::new();
        let child_session_id = SessionId::new();
        let target: DesktopAgentExecutionTarget = serde_json::from_value(serde_json::json!({
            "workspacePath": "C:/workspace",
            "rootSessionId": root_session_id.to_string(),
            "agentPath": "review/security",
            "childSessionId": child_session_id.to_string(),
        }))
        .expect("deserialize agent execution target");
        assert_eq!(target.agent_path, "review/security");

        let projection = DesktopAgentExecutionProjection {
            workspace_path: "C:/workspace".to_string(),
            root_session_id: root_session_id.to_string(),
            agent_path: target.agent_path,
            session_id: child_session_id.to_string(),
            task_name: "security_review".to_string(),
            transcript_rows: Vec::new(),
            turn_page_offset: 0,
            turn_page_end: 0,
            turn_page_total: 0,
            turn_page_has_previous: false,
        };
        let value = serde_json::to_value(projection).expect("serialize agent execution projection");
        let object = value.as_object().expect("projection object");
        assert!(object.contains_key("workspace_path"));
        assert!(object.contains_key("root_session_id"));
        assert!(object.contains_key("transcript_rows"));
        assert!(object.contains_key("turn_page_offset"));
        assert!(object.contains_key("turn_page_end"));
        assert!(object.contains_key("turn_page_total"));
        assert!(object.contains_key("turn_page_has_previous"));
        assert!(!object.contains_key("workspacePath"));
    }

    #[test]
    fn agent_interrupt_target_requires_exact_camel_case_turn_identity() {
        let root_session_id = SessionId::new();
        let child_session_id = SessionId::new();
        let turn_id = TurnId::new();
        let target: DesktopAgentInterruptTarget = serde_json::from_value(serde_json::json!({
            "workspacePath": "C:/workspace",
            "rootSessionId": root_session_id.to_string(),
            "agentPath": "/root/review",
            "childSessionId": child_session_id.to_string(),
            "expectedTurnId": turn_id.to_string(),
            "admissionRevision": "7",
        }))
        .expect("deserialize exact interrupt target");
        let serialized = serde_json::to_value(&target).expect("serialize exact interrupt target");
        assert_eq!(
            serialized
                .as_object()
                .expect("interrupt target object")
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            [
                "admissionRevision",
                "agentPath",
                "childSessionId",
                "expectedTurnId",
                "rootSessionId",
                "workspacePath",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>(),
        );
        assert_eq!(target.expected_turn_id().expect("turn id"), turn_id);
        assert_eq!(target.admission_revision().expect("revision"), 7);
        assert_eq!(
            target
                .execution_target()
                .expect("canonical execution target"),
            DesktopAgentExecutionTarget {
                workspace_path: "C:/workspace".to_string(),
                root_session_id: root_session_id.to_string(),
                agent_path: "/root/review".to_string(),
                child_session_id: child_session_id.to_string(),
            }
        );
        assert!(
            serde_json::from_value::<DesktopAgentInterruptTarget>(serde_json::json!({
                "workspacePath": "C:/workspace",
                "rootSessionId": root_session_id.to_string(),
                "agentPath": "/root/review",
                "childSessionId": child_session_id.to_string(),
                "expectedTurnId": turn_id.to_string(),
            }))
            .is_err(),
            "a child interrupt without an exact admission revision must fail closed"
        );
        for invalid in [
            serde_json::json!({
                "workspacePath": "C:/workspace",
                "rootSessionId": root_session_id.to_string().to_lowercase(),
                "agentPath": "/root/review",
                "childSessionId": child_session_id.to_string(),
                "expectedTurnId": turn_id.to_string(),
                "admissionRevision": "7",
            }),
            serde_json::json!({
                "workspacePath": "C:/workspace",
                "rootSessionId": root_session_id.to_string(),
                "agentPath": "/root/review",
                "childSessionId": child_session_id.to_string(),
                "expectedTurnId": turn_id.to_string().to_lowercase(),
                "admissionRevision": "7",
            }),
            serde_json::json!({
                "workspacePath": "C:/workspace",
                "rootSessionId": root_session_id.to_string(),
                "agentPath": "/root/review",
                "childSessionId": child_session_id.to_string(),
                "expectedTurnId": turn_id.to_string(),
                "admissionRevision": "007",
            }),
        ] {
            let invalid: DesktopAgentInterruptTarget =
                serde_json::from_value(invalid).expect("deserialize wire shape");
            assert!(
                invalid.execution_target().is_err()
                    || invalid.expected_turn_id().is_err()
                    || invalid.admission_revision().is_err(),
                "noncanonical identities and decimal owners must fail closed"
            );
        }
    }

    #[test]
    fn background_session_interrupt_requires_the_exact_turn_stop_target() {
        let session_id = SessionId::new();
        let turn_a = TurnId::new();
        let turn_b = TurnId::new();
        let target = DesktopStopMutationTarget::Turn {
            workspace_path: "C:/workspace".to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_a.to_string(),
            admission_revision: TEST_ADMISSION_REVISION.to_string(),
            root_epoch: "11".to_string(),
        };
        assert_eq!(
            validate_background_session_interrupt_target(
                &target,
                "C:/workspace",
                session_id,
                Some((turn_a, TEST_ADMISSION_REVISION)),
            )
            .expect("current A target"),
            (turn_a, TEST_ADMISSION_REVISION)
        );
        assert!(
            validate_background_session_interrupt_target(
                &target,
                "C:/workspace",
                session_id,
                Some((turn_b, TEST_ADMISSION_REVISION + 1)),
            )
            .is_err(),
            "an interrupt captured for A must not retarget replacement B"
        );
        assert_eq!(
            validate_background_session_interrupt_target(
                &target,
                "C:/workspace",
                session_id,
                None,
            )
                .expect("terminal crossing defers latest-turn CAS to storage"),
            (turn_a, TEST_ADMISSION_REVISION)
        );
        for stale_target in [
            DesktopStopMutationTarget::Turn {
                workspace_path: "C:/other".to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_a.to_string(),
                admission_revision: TEST_ADMISSION_REVISION.to_string(),
                root_epoch: "11".to_string(),
            },
            DesktopStopMutationTarget::Turn {
                workspace_path: "C:/workspace".to_string(),
                session_id: SessionId::new().to_string(),
                turn_id: turn_a.to_string(),
                admission_revision: TEST_ADMISSION_REVISION.to_string(),
                root_epoch: "11".to_string(),
            },
            DesktopStopMutationTarget::Turn {
                workspace_path: "C:/workspace".to_string(),
                session_id: session_id.to_string(),
                turn_id: "not-a-turn".to_string(),
                admission_revision: TEST_ADMISSION_REVISION.to_string(),
                root_epoch: "11".to_string(),
            },
            DesktopStopMutationTarget::Turn {
                workspace_path: "C:/workspace".to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_a.to_string(),
                admission_revision: TEST_ADMISSION_REVISION.to_string(),
                root_epoch: "011".to_string(),
            },
        ] {
            assert!(
                validate_background_session_interrupt_target(
                    &stale_target,
                    "C:/workspace",
                    session_id,
                    Some((turn_a, TEST_ADMISSION_REVISION)),
                )
                .is_err()
            );
        }
        assert!(
            validate_background_session_interrupt_target(
                &DesktopStopMutationTarget::Root {
                    workspace_path: "C:/workspace".to_string(),
                    session_id: Some(session_id.to_string()),
                    root_generation: "11".to_string(),
                    latest_turn_id: Some(turn_a.to_string()),
                    admission_revision: TEST_ADMISSION_REVISION.to_string(),
                    permission_confirmation_id: None,
                },
                "C:/workspace",
                session_id,
                Some((turn_a, TEST_ADMISSION_REVISION)),
            )
            .is_err()
        );
        for invalid in [
            serde_json::json!({
                "kind": "turn",
                "workspacePath": "C:/workspace",
                "sessionId": null,
                "turnId": turn_a.to_string(),
                "admissionRevision": TEST_ADMISSION_REVISION.to_string(),
                "rootEpoch": "11",
            }),
            serde_json::json!({
                "kind": "turn",
                "workspacePath": "C:/workspace",
                "turnId": turn_a.to_string(),
                "admissionRevision": TEST_ADMISSION_REVISION.to_string(),
                "rootEpoch": "11",
            }),
        ] {
            assert!(
                serde_json::from_value::<DesktopStopMutationTarget>(invalid).is_err(),
                "an admitted Turn Stop must require one concrete session owner"
            );
        }
    }

    #[test]
    fn stop_target_wire_roundtrips_exact_camel_case_for_both_variants() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let targets = [
            DesktopStopMutationTarget::Root {
                workspace_path: "C:/workspace".to_string(),
                session_id: Some(session_id.to_string()),
                root_generation: "11".to_string(),
                latest_turn_id: Some(turn_id.to_string()),
                admission_revision: "7".to_string(),
                permission_confirmation_id: Some("41".to_string()),
            },
            DesktopStopMutationTarget::Turn {
                workspace_path: "C:/workspace".to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.to_string(),
                admission_revision: "7".to_string(),
                root_epoch: "11".to_string(),
            },
        ];
        for target in &targets {
            let value = serde_json::to_value(target).expect("serialize exact Stop target");
            let object = value.as_object().expect("Stop target object");
            assert!(object.contains_key("workspacePath"));
            assert!(object.contains_key("sessionId"));
            assert!(object.contains_key("admissionRevision"));
            assert!(!object.contains_key("workspace_path"));
            assert!(!object.contains_key("session_id"));
            assert!(!object.contains_key("admission_revision"));
            let parsed = serde_json::from_value::<DesktopStopMutationTarget>(value)
                .expect("deserialize exact Stop target");
            assert_eq!(&parsed, target);
        }

        let legacy_snake_case = serde_json::json!({
            "kind": "turn",
            "workspace_path": "C:/workspace",
            "session_id": session_id.to_string(),
            "turn_id": turn_id.to_string(),
            "admission_revision": "7",
            "root_epoch": "11",
        });
        assert!(serde_json::from_value::<DesktopStopMutationTarget>(legacy_snake_case).is_err());
        let mut unknown = serde_json::to_value(&targets[1]).expect("serialize Turn target");
        unknown.as_object_mut().expect("Turn target object").insert(
            "compatibilityFlag".to_string(),
            serde_json::Value::Bool(false),
        );
        assert!(serde_json::from_value::<DesktopStopMutationTarget>(unknown).is_err());
    }

    #[test]
    fn agent_execution_target_rejects_stale_workspace_or_root_owner() {
        let root_session_id = SessionId::new();
        let child_session_id = SessionId::new();
        let target = DesktopAgentExecutionTarget {
            workspace_path: "C:/workspace".to_string(),
            root_session_id: root_session_id.to_string(),
            agent_path: "review/security".to_string(),
            child_session_id: child_session_id.to_string(),
        };
        assert!(
            validate_agent_execution_owner(&target, "C:/workspace", Some(root_session_id),).is_ok()
        );
        assert!(
            validate_agent_execution_owner(&target, "C:/other", Some(root_session_id)).is_err()
        );
        assert!(
            validate_agent_execution_owner(&target, "C:/workspace", Some(SessionId::new()),)
                .is_err()
        );
        assert!(validate_agent_execution_owner(&target, "C:/workspace", None).is_err());
    }

    #[test]
    fn agent_execution_edge_rejects_forged_root_child_and_path() {
        let root_session_id = SessionId::new();
        let child_session_id = SessionId::new();
        let target = DesktopAgentExecutionTarget {
            workspace_path: "C:/workspace".to_string(),
            root_session_id: root_session_id.to_string(),
            agent_path: "review/security".to_string(),
            child_session_id: child_session_id.to_string(),
        };
        let edge = SessionSpawnEdge {
            root_session_id,
            parent_session_id: SessionId::new(),
            child_session_id,
            agent_path: target.agent_path.clone(),
            task_name: "security_review".to_string(),
            spawn_order: 1,
            created_at_ms: 1,
        };
        assert!(
            validate_agent_execution_edge(&target, root_session_id, child_session_id, Some(&edge),)
                .is_ok()
        );
        assert!(
            validate_agent_execution_edge(
                &target,
                SessionId::new(),
                child_session_id,
                Some(&edge),
            )
            .is_err()
        );
        assert!(
            validate_agent_execution_edge(&target, root_session_id, SessionId::new(), Some(&edge),)
                .is_err()
        );
        let mut forged_path = target.clone();
        forged_path.agent_path = "review/other".to_string();
        assert!(
            validate_agent_execution_edge(
                &forged_path,
                root_session_id,
                child_session_id,
                Some(&edge),
            )
            .is_err()
        );
        assert!(
            validate_agent_execution_edge(&target, root_session_id, child_session_id, None)
                .is_err()
        );
    }

    #[test]
    fn previous_agent_execution_page_is_bounded_and_requires_exact_adjacency() {
        assert_eq!(
            previous_agent_execution_page_request(160, 240),
            Ok(AgentExecutionReadRequest::Previous {
                expected_end: 240,
                offset: 80,
            })
        );
        assert_eq!(
            previous_agent_execution_page_request(37, 117),
            Ok(AgentExecutionReadRequest::Previous {
                expected_end: 117,
                offset: 0,
            })
        );
        assert!(previous_agent_execution_page_request(0, 80).is_err());
        assert!(previous_agent_execution_page_request(80, 80).is_err());
        assert!(validate_previous_agent_execution_page(0, 160, 0, 160, 200).is_ok());
        assert!(validate_previous_agent_execution_page(0, 160, 0, 159, 200).is_err());
        assert!(validate_previous_agent_execution_page(0, 160, 1, 159, 200).is_err());
        assert!(validate_previous_agent_execution_page(0, 160, 0, 160, 159).is_err());
        assert!(validate_agent_execution_snapshot_end(0, 160, 160).is_ok());
        assert!(validate_agent_execution_snapshot_end(0, 160, 161).is_err());
        assert!(validate_agent_execution_snapshot_end(160, 160, 160).is_err());
        assert!(agent_execution_has_previous(80, 160));
        assert!(!agent_execution_has_previous(0, 160));
        assert!(agent_execution_has_previous(80, 720));
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
            .filter(|field| !field.key.is_host_owned_generation())
            .map(|field| DesktopConfigValueInput {
                key: field.key.label().to_string(),
                text: field.value.clone(),
            })
            .collect::<Vec<_>>();

        assert!(!complete_config_draft_is_dirty(&config, &values).expect("clean full draft"));
        let prompt_input = values
            .iter_mut()
            .find(|value| value.key == crate::config::ConfigField::SystemPrompt.label())
            .expect("main system prompt field");
        prompt_input.text = "MAIN_DRAFT_PRIVATE_MARKER".to_string();
        let prompt_debug = format!("{prompt_input:?}");
        assert!(prompt_debug.contains("text_chars"));
        assert!(!prompt_debug.contains("MAIN_DRAFT_PRIVATE_MARKER"));
        prompt_input.text.clear();
        values
            .iter_mut()
            .find(|value| value.key == "model.model")
            .expect("model field")
            .text = "locally-edited-model".to_string();
        assert!(complete_config_draft_is_dirty(&config, &values).expect("dirty full draft"));
        let last = values.last_mut().expect("visible config field");
        let current_last = last.clone();
        *last = DesktopConfigValueInput {
            key: crate::config::ConfigField::Temperature.label().to_string(),
            text: "0.7".to_string(),
        };
        assert!(
            complete_config_draft_is_dirty(&config, &values).is_err(),
            "a compatibility-only generation field cannot replace a current GUI draft field"
        );
        *values.last_mut().expect("visible config field") = current_last;
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
        assert!(validate_turn_page_load_admission(false).is_ok());
        assert!(validate_turn_page_load_admission(true).is_err());
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
    fn side_chat_target_requires_exact_ulids_and_decimal_generation() {
        let owner = SessionId::new();
        let side_chat = crate::storage::SideChatId::new();
        assert_eq!(
            parse_side_chat_target(
                &owner.to_string(),
                &side_chat.to_string(),
                "18446744073709551615",
            ),
            Ok((owner, side_chat, u64::MAX))
        );
        assert!(parse_side_chat_target("owner", &side_chat.to_string(), "0").is_err());
        assert!(parse_side_chat_target(&owner.to_string(), "chat", "0").is_err());
        assert!(parse_side_chat_target(&owner.to_string(), &side_chat.to_string(), "1.0").is_err());
    }

    #[test]
    fn side_chat_creation_requires_the_exact_owner_and_catalog_requires_global_generation() {
        let owner = SessionId::new();
        let other_owner = SessionId::new();
        assert!(validate_side_chat_config_owner(owner, "7", Some(owner), 7).is_ok());
        assert!(validate_side_chat_config_owner(owner, "7", Some(other_owner), 7).is_err());
        assert!(validate_side_chat_config_owner(owner, "7", None, 7).is_err());
        let generation_conflict = validate_side_chat_config_owner(owner, "8", Some(owner), 7)
            .expect_err("stale config generation");
        assert_eq!(
            generation_conflict.message,
            "configuration changed before the side chat operation; retry from current settings"
        );
        assert!(validate_side_chat_config_owner(owner, "07", Some(owner), 7).is_err());
        assert!(validate_side_chat_global_config_target("7", 7).is_ok());
        assert!(validate_side_chat_global_config_target("8", 7).is_err());
        assert!(validate_side_chat_global_config_target("07", 7).is_err());

        let target = SideChatCatalogRequestTarget {
            base_url: "http://127.0.0.1:1234".to_string(),
            provider_profile: ProviderProfile::OpenAiCompatible,
            config_generation: 7,
        };
        assert!(validate_side_chat_catalog_target(&target, 7).is_ok());
        assert!(validate_side_chat_catalog_target(&target, 8).is_err());
    }

    #[test]
    fn side_chat_catalog_probe_is_canonical_and_credential_free() {
        let mut config = ResolvedConfig::default();
        let profile = ProviderProfile::OpenAiCompatible;
        config.side_chat.connect_timeout_ms = 3_210;
        config.side_chat.request_timeout_ms = 54_321;
        config.side_chat.max_retries = 4;
        config.side_chat.context_window = 65_536;
        config.model.api_key_env = Some("MAIN_PROVIDER_SECRET".to_string());
        config
            .model
            .extra_headers
            .insert("X-Main-Secret".to_string(), "secret".to_string());
        config.model.chat_completions_reasoning_parameters =
            Some(crate::config::ChatCompletionsReasoningParameters::EffortAndSummary);
        config.model.reasoning_effort = Some(crate::config::ReasoningEffort::High);
        config.model.reasoning_summary = ReasoningSummary::Detailed;
        config.model.temperature = Some(0.5);
        config.model.top_p = Some(0.8);
        config.model.top_k = Some(20);
        config.model.presence_penalty = Some(0.1);
        config.model.frequency_penalty = Some(0.2);
        config.model.seed = Some(42);
        config.model.stop_sequences = vec!["stop".to_string()];
        config.model.extra_body_json = Some(serde_json::json!({
            "reasoning": { "effort": "high" },
            "api_key": "must-not-leak"
        }));

        let (probe, canonical) =
            side_chat_catalog_probe_config(config, " http://127.0.0.1:1234/v1/ ", profile)
                .expect("valid side chat provider endpoint");
        assert_eq!(canonical, "http://127.0.0.1:1234");
        assert_eq!(probe.model.base_url, canonical);
        assert_eq!(probe.model.provider_profile, profile);
        assert_eq!(probe.model.connect_timeout_ms, 3_210);
        assert_eq!(probe.model.request_timeout_ms, 54_321);
        assert_eq!(probe.model.max_retries, 4);
        assert_eq!(probe.model.context_window, 65_536);
        assert!(probe.model.system_prompt.is_empty());
        assert_eq!(probe.model.api_key_env, None);
        assert!(probe.model.extra_headers.is_empty());
        assert_eq!(probe.model.chat_completions_reasoning_parameters, None);
        assert_eq!(probe.model.reasoning_effort, None);
        assert_eq!(probe.model.reasoning_summary, ReasoningSummary::None);
        assert_eq!(probe.model.temperature, None);
        assert_eq!(probe.model.top_p, None);
        assert_eq!(probe.model.top_k, None);
        assert_eq!(probe.model.presence_penalty, None);
        assert_eq!(probe.model.frequency_penalty, None);
        assert_eq!(probe.model.seed, None);
        assert!(probe.model.stop_sequences.is_empty());
        assert_eq!(probe.model.extra_body_json, None);
        assert!(
            side_chat_catalog_probe_config(
                ResolvedConfig::default(),
                "http://user:password@127.0.0.1:1234",
                ProviderProfile::OpenAiCompatible,
            )
            .is_err()
        );
    }

    #[test]
    fn side_chat_catalog_result_uses_main_model_labels_and_typed_load_state() {
        let model = side_chat_catalog_model_projection(ProviderModelInfo {
            id: "google/gemma-4-12b-qat".to_string(),
            display_name: Some("Gemma 4 12B QAT".to_string()),
            context_window: Some(131_072),
            max_output_tokens: Some(8_192),
            supports_images: Some(false),
            supports_tools: Some(false),
            supports_reasoning: Some(false),
            max_parallel_predictions: Some(4),
            load_state: ProviderModelLoadState::Loaded,
            source: "lm_studio_native".to_string(),
        });
        assert_eq!(model.id, "google/gemma-4-12b-qat");
        assert!(
            model
                .label
                .starts_with("google/gemma-4-12b-qat  [ctx=131072")
        );
        assert_eq!(model.load_state, ProviderModelLoadState::Loaded);

        let projection = SideChatCatalogProjection {
            base_url: "http://127.0.0.1:1234".to_string(),
            provider_profile: ProviderProfile::OpenAiCompatible.as_str().to_string(),
            config_generation: "7".to_string(),
            models: vec![model],
        };
        let json = serde_json::to_value(projection).expect("serialize side chat catalog");
        assert!(json.get("ownerSessionId").is_none());
        assert_eq!(json["baseUrl"], "http://127.0.0.1:1234");
        assert_eq!(json["providerProfile"], "openai_compatible");
        assert_eq!(json["configGeneration"], "7");
        assert_eq!(json["models"][0]["loadState"], "loaded");
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
