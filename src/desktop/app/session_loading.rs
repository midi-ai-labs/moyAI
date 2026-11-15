use super::*;

impl DesktopController {
    pub(crate) fn load_previous_turn_page(&mut self) {
        let detail = self.state.selected_detail();
        if detail.turn_page_offset == 0 || detail.turn_page_limit == 0 {
            self.state
                .set_status_message("earlier turn page is not available");
            return;
        }
        let previous = detail
            .turn_page_offset
            .saturating_sub(detail.turn_page_limit);
        self.load_selected_turn_page(previous);
    }

    pub(crate) fn load_next_turn_page(&mut self) {
        let Some(next_offset) = self.state.next_turn_page_offset() else {
            self.state
                .set_status_message("later turn page is not available");
            return;
        };
        self.load_selected_turn_page(next_offset);
    }

    fn load_selected_turn_page(&mut self, offset: usize) {
        if self.state.turn_page_load_pending() {
            self.state
                .set_status_message("turn page load is already active");
            return;
        }
        if !self.state.can_begin_turn_page_load() {
            self.state
                .set_status_message("turn page cannot load while the current session is changing");
            return;
        }
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a session before changing turn page");
            return;
        };
        self.state
            .set_status_message(format!("loading turn page for session {session_id}..."));
        self.spawn_turn_page_load(session_id, offset, DESKTOP_TURN_PAGE_LIMIT);
    }

    pub(super) fn spawn_session_load(
        &self,
        session_id: SessionId,
        reason: SessionLoadReason,
        request_id: NavigationRequestId,
    ) {
        let app = self.app.clone();
        let target = SessionLoadRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            workspace_cwd: self.app.workspace.cwd.clone(),
            project_id: self.app.workspace.project_id,
            session_id,
        };
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session runtime");
            let result = runtime.block_on(load_session_navigation_result(app, session_id, reason));
            let _ = runtime_tx.send(RuntimeMessage::SessionLoaded {
                request_id,
                target,
                reason,
                result,
            });
        });
    }

    fn spawn_current_session_refresh(&mut self, session_id: SessionId) {
        self.spawn_current_session_refresh_page(
            session_id,
            CurrentSessionRefreshPurpose::Refresh,
            None,
        );
    }

    fn spawn_current_session_refresh_page(
        &mut self,
        session_id: SessionId,
        purpose: CurrentSessionRefreshPurpose,
        offset: Option<usize>,
    ) {
        let app = self.app.clone();
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self
            .session_projection_refresh_requests
            .begin(target.clone());
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop current-session-refresh runtime");
            let result = runtime.block_on(async move {
                let detail = if let Some(offset) = offset {
                    let read = app
                        .session_service
                        .canonical_session_read(
                            session_id,
                            0,
                            DESKTOP_HISTORY_PROJECTION_LIMIT,
                            offset,
                            DESKTOP_TURN_PAGE_LIMIT,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    LoadedSessionDetail { read }
                } else {
                    load_latest_session_detail(&app, session_id)
                        .await
                        .map_err(|error| error.to_string())?
                };
                loaded_session_from_detail_with_activity(&app, detail).await
            });
            let _ = runtime_tx.send(RuntimeMessage::CurrentSessionRefreshed {
                request_id,
                target,
                purpose,
                result,
            });
        });
    }

    fn spawn_turn_page_load(&mut self, session_id: SessionId, offset: usize, limit: usize) {
        let app = self.app.clone();
        let target = SessionPageRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
            offset,
            limit,
        };
        let request_id = self.turn_page_requests.begin(target.clone());
        self.state.begin_turn_page_load();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop turn-page runtime");
            let result = runtime.block_on(async move {
                let read = app
                    .session_service
                    .canonical_session_read(
                        session_id,
                        0,
                        DESKTOP_HISTORY_PROJECTION_LIMIT,
                        offset,
                        limit,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(LoadedSession {
                    read,
                    agent_activity_records: None,
                })
            });
            let _ = runtime_tx.send(RuntimeMessage::TurnPageLoaded {
                request_id,
                target,
                result,
            });
        });
    }

    pub(super) fn spawn_latest_live_session_refresh(&mut self, session_id: SessionId) {
        let app = self.app.clone();
        let contiguous_offset = self
            .state
            .open_session
            .as_ref()
            .filter(|open_session| open_session.session_id() == session_id)
            .map(OpenSessionView::loaded_turn_end);
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self
            .session_projection_refresh_requests
            .begin(target.clone());
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop latest-session-refresh runtime");
            let result = runtime.block_on(async move {
                let detail = if let Some(offset) = contiguous_offset {
                    let read = app
                        .session_service
                        .canonical_session_read(
                            session_id,
                            0,
                            DESKTOP_HISTORY_PROJECTION_LIMIT,
                            offset,
                            DESKTOP_TURN_PAGE_LIMIT,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    LoadedSessionDetail { read }
                } else {
                    load_latest_session_detail(&app, session_id)
                        .await
                        .map_err(|error| error.to_string())?
                };
                Ok(loaded_session_from_detail(detail, None))
            });
            let _ = runtime_tx.send(RuntimeMessage::LiveSessionRefreshed {
                request_id,
                target,
                result,
            });
        });
    }

    pub(super) fn spawn_durable_agent_activity_refresh(&mut self, session_id: SessionId) {
        let app = self.app.clone();
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self
            .durable_agent_activity_refresh_requests
            .begin(target.clone());
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop durable-agent-activity runtime");
            let result = runtime.block_on(async move {
                app.run_service
                    .durable_agent_activity_records(session_id)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::DurableAgentActivityRefreshed {
                request_id,
                target,
                result,
            });
        });
    }

    pub(super) fn spawn_session_rejoin(
        &self,
        session_id: SessionId,
        request_id: NavigationRequestId,
    ) {
        let app = self.app.clone();
        let target = SessionLoadRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            workspace_cwd: self.app.workspace.cwd.clone(),
            project_id: self.app.workspace.project_id,
            session_id,
        };
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session rejoin runtime");
            let result = runtime.block_on(load_session_navigation_result(
                app,
                session_id,
                SessionLoadReason::RunningRejoin,
            ));
            let _ = runtime_tx.send(RuntimeMessage::SessionLoaded {
                request_id,
                target,
                reason: SessionLoadReason::RunningRejoin,
                result,
            });
        });
    }

    pub(super) fn apply_session_loaded_message(
        &mut self,
        request_id: NavigationRequestId,
        target: SessionLoadRequestTarget,
        reason: SessionLoadReason,
        result: Result<SessionNavigationLoadResult, String>,
    ) {
        if target.workspace_root != self.app.workspace.root
            || target.workspace_cwd != self.app.workspace.cwd
            || target.project_id != self.app.workspace.project_id
            || !self
                .state
                .is_current_session_navigation(request_id, target.session_id)
        {
            return;
        }
        let session_id = target.session_id;
        match result {
            Ok(result) => {
                if self.session_load_is_blocked_by_active_run() {
                    self.state.finish_navigation(request_id);
                    return;
                }
                self.state.finish_navigation(request_id);
                if let Some(workspace) = result.workspace
                    && !self.replace_workspace_from_load(workspace)
                {
                    return;
                }
                if let Some(snapshot) = result.snapshot
                    && !self
                        .state
                        .replace_snapshot_selecting_session(snapshot, session_id)
                {
                    self.state.set_status_message(format!(
                        "edited session {} is missing from the project list",
                        session_id
                    ));
                    return;
                }
                let loaded = result.loaded;
                let loaded_status = loaded.read.session.status;
                self.state.load_open_session(&loaded.read);
                self.state.composer.retained_images = self
                    .edit_image_drafts
                    .get(&session_id)
                    .map(|(_, images)| images.clone())
                    .unwrap_or_default();
                self.state.view.hub_project_open = false;
                self.state.hide_overlay();
                if let Some(records) = loaded.agent_activity_records {
                    self.loaded_agent_activity_records = Some((loaded.read.session.id, records));
                    self.durable_agent_activity_refresh_failures = 0;
                }
                if !self.state.status_code.is_terminal_interruption()
                    && !(reason == SessionLoadReason::RunningRejoin
                        && loaded_status != SessionStatus::Running)
                {
                    self.state.set_status_message(match reason {
                        SessionLoadReason::RunningRejoin => {
                            "実行中の会話を開きました。".to_string()
                        }
                        SessionLoadReason::UserSelection | SessionLoadReason::CreatedEditFork => {
                            "会話を開きました。".to_string()
                        }
                    });
                }
                self.reconcile_runtime_listener_with_open_session();
                self.persist_preferences();
            }
            Err(error) => {
                finish_navigation_failure(&mut self.state, request_id, error);
            }
        }
    }

    pub(super) fn apply_current_session_refreshed_message(
        &mut self,
        session_id: SessionId,
        purpose: CurrentSessionRefreshPurpose,
        result: Result<LoadedSession, String>,
    ) {
        if (matches!(purpose, CurrentSessionRefreshPurpose::Refresh)
            && self.session_load_is_blocked_by_active_run())
            || self.state.app_state.current_session_id != Some(session_id)
        {
            self.state.clear_post_run_refresh_pending();
            return;
        }
        match result {
            Ok(loaded) => {
                let loaded_status = loaded.read.session.status;
                let loaded_active_turn_expectation = if loaded_status == SessionStatus::Running {
                    loaded
                        .read
                        .active_turn_id
                        .map(|turn_id| ActiveTurnExpectation::Turn {
                            turn_id,
                            revision: loaded.read.admission_revision,
                        })
                        .unwrap_or(ActiveTurnExpectation::Idle {
                            latest_turn_id: loaded.read.latest_turn_id,
                            revision: loaded.read.admission_revision,
                        })
                } else {
                    ActiveTurnExpectation::Idle {
                        latest_turn_id: loaded.read.latest_turn_id,
                        revision: loaded.read.admission_revision,
                    }
                };
                let page_has_more = loaded.read.turns.has_more;
                let history_is_contiguous = self
                    .state
                    .load_open_session_preserving_history(&loaded.read);
                self.reconcile_runtime_listener_with_open_session();
                if let Some(records) = loaded.agent_activity_records {
                    self.loaded_agent_activity_records = Some((loaded.read.session.id, records));
                    self.durable_agent_activity_refresh_failures = 0;
                }
                if !history_is_contiguous || page_has_more {
                    let next_offset = self
                        .state
                        .open_session
                        .as_ref()
                        .filter(|open_session| open_session.session_id() == session_id)
                        .map(OpenSessionView::loaded_turn_end);
                    if let Some(next_offset) = next_offset {
                        self.spawn_current_session_refresh_page(
                            session_id,
                            purpose,
                            Some(next_offset),
                        );
                        return;
                    }
                }
                self.state.clear_post_run_refresh_pending();
                if let CurrentSessionRefreshPurpose::StopRequestRefresh {
                    expected_active_turn,
                    in_memory_stop_accepted,
                    durable_outcome,
                    ..
                } = purpose
                {
                    let durable_stop_applied = matches!(
                        durable_outcome,
                        Some(ExactRootExecutionStopOutcome::Applied { .. })
                    );
                    let durable_target_changed = matches!(
                        durable_outcome,
                        Some(ExactRootExecutionStopOutcome::TargetChanged)
                    );
                    if durable_target_changed {
                        self.state.set_status_message(
                            "the task changed before Stop was applied; the replacement was not stopped",
                        );
                    } else if loaded_status == SessionStatus::Running
                        && matches!(
                            self.state.app_state.run_status,
                            crate::tui::state::RunStatus::Running
                        )
                        && Some(loaded_active_turn_expectation) == expected_active_turn
                        && (in_memory_stop_accepted || durable_stop_applied)
                    {
                        self.state.mark_run_stop_requested(
                            "run cancellation requested",
                            "Stop request accepted; waiting for the matching terminal event",
                        );
                    } else if matches!(
                        loaded_status,
                        SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
                    ) {
                        self.run_lifecycle.observe_terminal_event();
                    }
                }
            }
            Err(error) => {
                self.state.clear_post_run_refresh_pending();
                if matches!(
                    purpose,
                    CurrentSessionRefreshPurpose::StopRequestRefresh { .. }
                ) {
                    self.state.set_status_message(format!(
                        "failed to confirm durable Stop request: {error}"
                    ));
                } else {
                    self.state.set_status_message(error);
                }
            }
        }
    }

    pub(super) fn session_load_is_blocked_by_active_run(&self) -> bool {
        self.run_lifecycle.root_is_active()
            || matches!(
                self.state.app_state.run_status,
                crate::tui::state::RunStatus::Running
            )
    }

    pub(super) fn refresh_current_session_after_terminal_run(&mut self) {
        self.refresh_snapshot();
        if let Some(session_id) = self.state.app_state.current_session_id {
            self.spawn_current_session_refresh(session_id);
        }
    }
}
