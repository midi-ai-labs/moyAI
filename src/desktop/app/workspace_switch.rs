use super::*;

impl DesktopController {
    pub(crate) fn switch_workspace_to(&mut self, text: String) -> bool {
        if !self.ensure_navigation_admission("workspace") {
            return false;
        }
        self.state.set_workspace_input(text);
        self.begin_workspace_switch_from_input()
    }

    fn begin_workspace_switch_from_input(&mut self) -> bool {
        let Some(requested) = self.resolve_workspace_input() else {
            return false;
        };
        self.invalidate_session_target_requests();
        let request_id = self.state.begin_workspace_load(requested.clone(), None);
        self.spawn_workspace_load(requested, request_id);
        true
    }

    pub(crate) fn show_workspace_picker(&mut self) {
        if !self.ensure_navigation_admission("workspace") {
            return;
        }
        let path = self.app.workspace.cwd.to_string();
        self.state.show_workspace_picker(&path);
    }

    pub(super) fn spawn_workspace_load(
        &self,
        requested: Utf8PathBuf,
        request_id: NavigationRequestId,
    ) {
        self.spawn_workspace_load_for_selection(
            requested,
            None,
            request_id,
            WorkspaceRootMode::Discover,
        );
    }

    pub(super) fn spawn_fixed_workspace_load(
        &self,
        requested: Utf8PathBuf,
        request_id: NavigationRequestId,
    ) {
        self.spawn_workspace_load_for_selection(
            requested,
            None,
            request_id,
            WorkspaceRootMode::Fixed,
        );
    }

    pub(super) fn spawn_workspace_load_for_new_project_session(
        &self,
        requested: Utf8PathBuf,
        request_id: NavigationRequestId,
    ) {
        let process_runtime = self.app.process_runtime.clone();
        let runtime_tx = self.runtime_tx.clone();
        let completion_target = requested.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop workspace runtime");
            let result = runtime.block_on(async move {
                let app = AppBootstrap::rebuild_for_directory_with_process_runtime(
                    &requested,
                    process_runtime,
                )
                .await
                .map_err(|error| error.to_string())?;
                let snapshot = load_snapshot_for_selection(&app, None)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(WorkspaceLoadResult { app, snapshot })
            });
            let _ = runtime_tx.send(RuntimeMessage::WorkspaceSwitchedForNewProjectSession {
                request_id,
                requested: completion_target,
                result,
            });
        });
    }

    pub(super) fn spawn_workspace_load_for_selection(
        &self,
        requested: Utf8PathBuf,
        selected_session_id: Option<SessionId>,
        request_id: NavigationRequestId,
        root_mode: WorkspaceRootMode,
    ) {
        let process_runtime = self.app.process_runtime.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop workspace runtime");
            let result = runtime.block_on(async move {
                let app = match root_mode {
                    WorkspaceRootMode::Discover => {
                        AppBootstrap::rebuild_for_directory_with_process_runtime(
                            &requested,
                            process_runtime,
                        )
                        .await
                    }
                    WorkspaceRootMode::Fixed => {
                        AppBootstrap::rebuild_for_directory_as_workspace_root_with_process_runtime(
                            &requested,
                            process_runtime,
                        )
                        .await
                    }
                }
                .map_err(|error| error.to_string())?;
                let snapshot = load_snapshot_for_selection(&app, selected_session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(WorkspaceLoadResult { app, snapshot })
            });
            let _ = runtime_tx.send(RuntimeMessage::WorkspaceSwitched { request_id, result });
        });
    }

    pub(crate) fn browse_workspace_dialog(&mut self) -> Option<Utf8PathBuf> {
        let start_dir = if self.state.workspace_input.trim().is_empty() {
            Some(self.app.workspace.cwd.clone())
        } else {
            self.resolve_workspace_input()
                .or_else(|| Some(self.app.workspace.cwd.clone()))
        };
        match pick_workspace_directory(start_dir.as_ref()) {
            Ok(Some(path)) => {
                self.state
                    .set_status_message(format!("selected workspace {}", path));
                Some(path)
            }
            Ok(None) => None,
            Err(error) => {
                self.state
                    .set_status_message(format!("workspace browse failed: {error}"));
                None
            }
        }
    }

    pub(super) fn resolve_workspace_input(&mut self) -> Option<camino::Utf8PathBuf> {
        let requested = self.state.workspace_input.trim().to_string();
        if requested.is_empty() {
            self.state
                .set_status_message("作業フォルダーを入力してください。");
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
                    "作業フォルダーを開けません: {} ({error})",
                    requested
                ));
                return None;
            }
        };
        if !metadata.is_dir() {
            self.state
                .set_status_message(format!("フォルダーを指定してください: {}", requested));
            return None;
        }
        Some(requested)
    }

    pub(super) fn apply_workspace_switched_message(
        &mut self,
        request_id: NavigationRequestId,
        result: Result<WorkspaceLoadResult, String>,
    ) {
        match result {
            Ok(loaded) => {
                if !self.state.is_current_navigation(request_id) {
                    return;
                }
                if !self.replace_workspace_from_load(loaded) {
                    return;
                }
                if let Some(session_id) = self.state.selected_session_id() {
                    self.state.set_status_message("会話を開いています…");
                    let request_id = self.state.begin_session_load(session_id);
                    self.spawn_session_load(
                        session_id,
                        SessionLoadReason::UserSelection,
                        request_id,
                    );
                } else {
                    self.state
                        .set_status_message(format!("作業フォルダー: {}", self.app.workspace.cwd));
                }
            }
            Err(error) => {
                finish_navigation_failure(&mut self.state, request_id, error);
            }
        }
    }

    pub(super) fn apply_new_project_workspace_switched_message(
        &mut self,
        request_id: NavigationRequestId,
        requested: Utf8PathBuf,
        result: Result<WorkspaceLoadResult, String>,
    ) {
        let target_is_current = self.state.navigation.active().is_some_and(|request| {
            request.id == request_id
                && matches!(
                    &request.target,
                    crate::desktop::navigation::NavigationTarget::Workspace {
                        path,
                        selected_session_id: None,
                        starts_new_project_session: true,
                    } if path == &requested
                )
        });
        if !target_is_current {
            return;
        }
        match result {
            Ok(loaded) => {
                if !self.replace_workspace_from_load(loaded) {
                    return;
                }
                self.start_new_chat_with_global_access();
                self.state
                    .set_status_message("新しいチャットを開始できます。");
            }
            Err(error) => {
                finish_navigation_failure(&mut self.state, request_id, error);
            }
        }
    }

    pub(super) fn replace_workspace_from_load(&mut self, loaded: WorkspaceLoadResult) -> bool {
        if !self.ensure_initial_setup_owner_replacement_admission("workspace completion") {
            self.state.clear_navigation();
            return false;
        }
        let next_config_generation =
            next_config_generation(self.state.provider_config.config_generation);
        self.invalidate_session_target_requests();
        self.provider_catalog_requests.clear();
        self.docling_readiness_requests.clear();
        self.app = loaded.app.clone();
        if !self.is_quick_chat_workspace() {
            self.preferences
                .unmark_project_deleted(&self.app.workspace.root);
        }
        self.session_search_requests.clear();
        let hub_connection = self.state.hub_connection.clone();
        let mcp_publish = self.state.mcp_publish.clone();
        let device_network = self.state.device_network.clone();
        self.state = DesktopState::new(loaded.snapshot, self.app.config.clone());
        self.state.hub_connection = hub_connection;
        self.state.mcp_publish = mcp_publish;
        self.state.device_network = device_network;
        self.state.set_file_change_display_roots(
            &self.app.workspace.root,
            self.app.workspace.authority_root(),
        );
        self.state.provider_config.config_generation = next_config_generation;
        self.loaded_agent_activity_records = None;
        self.durable_agent_activity_refresh_failures = 0;
        self.state.workspace_input = self.app.workspace.cwd.to_string();
        if let Some(opacity) = self.preferences.window_opacity_percent {
            self.state.set_window_opacity_percent(opacity);
        }
        self.persist_preferences();
        true
    }
}
