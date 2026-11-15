use super::*;

impl DesktopController {
    pub(crate) fn delete_session(&mut self, session_id: SessionId) -> bool {
        if !self.ensure_navigation_admission("chat deletion") {
            return false;
        }
        if !self
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == session_id)
        {
            self.state
                .set_status_message("削除するチャットが見つかりません。一覧を確認してください。");
            return false;
        }
        self.invalidate_session_target_requests();
        self.state.set_status_message("チャットを削除しています…");
        let operation_id = self.state.begin_session_delete_mutation();
        self.spawn_session_delete(session_id, operation_id);
        true
    }

    pub(crate) fn archive_session(&mut self, session_id: SessionId, archived: bool) -> bool {
        if !self.ensure_navigation_admission("chat archive state") {
            return false;
        }
        if !self
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == session_id)
        {
            self.state
                .set_status_message("変更するチャットが見つかりません。一覧を確認してください。");
            return false;
        }
        self.invalidate_session_target_requests();
        self.state.set_status_message(if archived {
            "チャットをアーカイブしています…"
        } else {
            "チャットを復元しています…"
        });
        let operation_id = self.state.begin_session_archive_mutation();
        let target = SessionMutationRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            project_id: self.app.workspace.project_id,
            session_id,
            operation_id,
        };
        self.spawn_session_archive(
            target,
            archived,
            self.state.view.session_search_text.clone(),
            self.state.view.session_search_include_archived,
        );
        true
    }

    pub(crate) fn rollback_session(&mut self, session_id: SessionId) -> bool {
        if !self.ensure_navigation_admission("chat rollback") {
            return false;
        }
        let Some(row) = self
            .state
            .snapshot
            .session_rows
            .iter()
            .find(|row| row.session_id == session_id)
        else {
            self.state
                .set_status_message("巻き戻すチャットが見つかりません。一覧を更新してください。");
            return false;
        };
        if row.loaded_status == LoadedSessionStatus::Active {
            self.state.set_status_message(
                "実行中のチャットは巻き戻せません。停止してから操作してください。",
            );
            return false;
        }
        self.invalidate_session_target_requests();
        self.state
            .set_status_message("最後の依頼と回答を履歴から削除しています…");
        let operation_id = self.state.begin_session_rollback_mutation();
        let target = SessionMutationRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            project_id: self.app.workspace.project_id,
            session_id,
            operation_id,
        };
        self.spawn_session_rollback(
            target,
            self.state.view.session_search_text.clone(),
            self.state.view.session_search_include_archived,
        );
        true
    }

    pub(crate) fn fork_session(&mut self, session_id: SessionId) -> bool {
        if !self.ensure_navigation_admission("chat fork") {
            return false;
        }
        let Some(row) = self
            .state
            .snapshot
            .session_rows
            .iter()
            .find(|row| row.session_id == session_id)
        else {
            self.state
                .set_status_message("分岐元のチャットが見つかりません。一覧を更新してください。");
            return false;
        };
        let title = format!("{} fork", row.title);
        self.invalidate_session_target_requests();
        self.state
            .set_status_message("この会話を元に、新しいチャットを作成しています…");
        let operation_id = self.state.begin_session_maintenance_mutation();
        let target = SessionMutationRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            project_id: self.app.workspace.project_id,
            session_id,
            operation_id,
        };
        self.spawn_session_fork(target, Some(title));
        true
    }

    pub(crate) fn interrupt_session(
        &mut self,
        session_id: SessionId,
        expected_turn_id: TurnId,
        expected_admission_revision: u64,
    ) -> bool {
        let expected_active_turn = ActiveTurnExpectation::Turn {
            turn_id: expected_turn_id,
            revision: expected_admission_revision,
        };
        if !self.ensure_navigation_admission("running chat interrupt") {
            return false;
        }
        if !self
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == session_id)
        {
            self.state
                .set_status_message("停止するチャットが見つかりません。一覧を更新してください。");
            return false;
        }
        self.invalidate_session_target_requests();
        if self.state.app_state.current_session_id == Some(session_id) && self.state.is_busy() {
            self.cancel_exact_turn_at(expected_turn_id, expected_admission_revision);
            return true;
        }
        self.state
            .set_status_message("選択したチャットの停止を要求しています…");
        let operation_id = self.state.begin_session_maintenance_mutation();
        let target = SessionMutationRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            project_id: self.app.workspace.project_id,
            session_id,
            operation_id,
        };
        self.spawn_session_interrupt(target, expected_active_turn);
        true
    }

    pub(crate) fn delete_project(&mut self, project_id: ProjectId) -> bool {
        if !self.ensure_navigation_admission("project deletion") {
            return false;
        }
        let Some(project_root) = self
            .state
            .snapshot
            .project_rows
            .iter()
            .find(|row| row.project_id == project_id)
            .map(|row| Utf8PathBuf::from(&row.path))
        else {
            self.state.set_status_message(
                "削除するプロジェクトが見つかりません。一覧を確認してください。",
            );
            return false;
        };
        self.invalidate_session_target_requests();
        self.state
            .set_status_message("プロジェクトを削除しています…");
        let mut hidden_roots = self.preferences.deleted_project_roots.clone();
        hidden_roots.extend(internal_desktop_project_roots(
            self.app.session_service.store.paths().data_dir.as_path(),
        ));
        if !hidden_roots.iter().any(|root| root == &project_root) {
            hidden_roots.push(project_root.clone());
        }
        let operation_id = self.state.begin_project_delete_mutation();
        let target = ProjectDeleteRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            owner_project_id: self.app.workspace.project_id,
            project_id,
            project_root,
            operation_id,
        };
        self.spawn_project_delete(target, hidden_roots);
        true
    }

    pub(super) fn spawn_session_cancel_persist(
        &mut self,
        session_id: SessionId,
        expected_active_turn: ActiveTurnExpectation,
    ) {
        let app = self.app.clone();
        let root_admission_fence = self.next_root_run_generation;
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self
            .session_projection_refresh_requests
            .begin(target.clone());
        let control_tx = self.control_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop cancel-persist runtime");
            let (durable_outcome, in_memory_stop_accepted, result) = runtime.block_on(async move {
                let (durable_outcome, in_memory_stop_accepted) =
                    match claim_exact_root_execution_stop(&app, session_id, expected_active_turn)
                        .await
                    {
                        Ok(outcome) => outcome,
                        Err(error) => return (None, false, Err(error.to_string())),
                    };
                let detail = match load_session_detail(&app, session_id).await {
                    Ok(detail) => detail,
                    Err(error) => {
                        return (
                            Some(durable_outcome),
                            in_memory_stop_accepted,
                            Err(error.to_string()),
                        );
                    }
                };
                (
                    Some(durable_outcome),
                    in_memory_stop_accepted,
                    loaded_session_from_detail_with_activity(&app, detail).await,
                )
            });
            let _ = control_tx.send(RuntimeMessage::CurrentSessionRefreshed {
                request_id,
                target,
                purpose: CurrentSessionRefreshPurpose::StopRequestRefresh {
                    root_admission_fence,
                    root_stop_attempt: None,
                    expected_active_turn: Some(expected_active_turn),
                    in_memory_stop_accepted,
                    durable_outcome,
                },
                result,
            });
        });
    }

    pub(super) fn spawn_root_session_cancel_persist(
        &mut self,
        generation: u64,
        session_id: SessionId,
        root_scope_control: RunControl,
        stop_plan: crate::app::run_service::RootExecutionStopPlan,
        root_stop_attempt: DesktopRootStopAttempt,
    ) {
        let app = self.app.clone();
        let root_admission_fence = self.next_root_run_generation;
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self
            .session_projection_refresh_requests
            .begin(target.clone());
        let control_tx = self.control_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop root cancel-persist runtime");
            let (durable_outcome, in_memory_stop_accepted, expected_active_turn, result) = runtime
                .block_on(async move {
                    let claim = match app
                        .run_service
                        .claim_root_execution_stop(&root_scope_control, stop_plan)
                        .await
                    {
                        Ok(claim) => claim,
                        Err(error) => return (None, false, None, Err(error.to_string())),
                    };
                    let local_stop_accepted = claim
                        .local_stop
                        .is_some_and(crate::runtime::RootExecutionLocalStop::stop_accepted);
                    if claim
                        .session_id
                        .is_some_and(|claimed| claimed != session_id)
                    {
                        return (
                            claim.durable_outcome,
                            local_stop_accepted,
                            claim.expected_active_turn,
                            Err("the canonical root Stop plan changed sessions".to_string()),
                        );
                    }
                    let detail = match load_session_detail(&app, session_id).await {
                        Ok(detail) => detail,
                        Err(error) => {
                            return (
                                claim.durable_outcome,
                                local_stop_accepted,
                                claim.expected_active_turn,
                                Err(error.to_string()),
                            );
                        }
                    };
                    (
                        claim.durable_outcome,
                        local_stop_accepted,
                        claim.expected_active_turn,
                        loaded_session_from_detail_with_activity(&app, detail).await,
                    )
                });
            let _ = control_tx.send(RuntimeMessage::CurrentSessionRefreshed {
                request_id,
                target,
                purpose: CurrentSessionRefreshPurpose::StopRequestRefresh {
                    root_admission_fence,
                    root_stop_attempt: Some(root_stop_attempt),
                    expected_active_turn,
                    in_memory_stop_accepted,
                    durable_outcome,
                },
                result,
            });
        });
        debug_assert_eq!(self.run_lifecycle.root_generation(), Some(generation));
    }

    pub(super) fn spawn_session_delete(
        &self,
        session_id: SessionId,
        operation_id: DesktopAsyncOperationId,
    ) {
        let app = self.app.clone();
        let target = SessionDeleteRequestTarget {
            workspace_root: app.workspace.root.clone(),
            project_id: app.workspace.project_id,
            session_id,
            operation_id,
        };
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
                run_storage_maintenance_after_delete(&app)?;
                load_snapshot_for_selection(&app, None)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionDeleted { target, result });
        });
    }

    fn spawn_session_archive(
        &self,
        target: SessionMutationRequestTarget,
        archived: bool,
        query: String,
        include_archived: bool,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let session_id = target.session_id;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-archive runtime");
            let result = runtime.block_on(async move {
                app.session_service
                    .set_session_archived(session_id, archived)
                    .await
                    .map_err(|error| error.to_string())?;
                load_snapshot_for_session_search(&app, &query, include_archived, None)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionArchived {
                target,
                archived,
                result,
            });
        });
    }

    fn spawn_session_rollback(
        &self,
        target: SessionMutationRequestTarget,
        query: String,
        include_archived: bool,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let session_id = target.session_id;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-rollback runtime");
            let result = runtime.block_on(async move {
                let rollback = app
                    .session_service
                    .rollback_session(session_id, 1)
                    .await
                    .map_err(|error| error.to_string())?;
                let snapshot = load_snapshot_for_session_search(
                    &app,
                    &query,
                    include_archived,
                    Some(session_id),
                )
                .await
                .map_err(|error| error.to_string())?;
                let detail = load_session_detail(&app, session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let loaded = loaded_session_from_detail_with_activity(&app, detail).await?;
                Ok(DesktopRollbackLoaded {
                    snapshot,
                    loaded,
                    dropped_turn_count: rollback.dropped_turn_ids.len(),
                })
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionRolledBack { target, result });
        });
    }

    fn spawn_session_fork(&self, target: SessionMutationRequestTarget, title: Option<String>) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let source_session_id = target.session_id;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-fork runtime");
            let result = runtime.block_on(async move {
                let fork = app
                    .session_service
                    .fork_session(source_session_id, title)
                    .await
                    .map_err(|error| error.to_string())?;
                let session_id = fork.forked_session.id;
                load_session_operation_projection(
                    &app,
                    session_id,
                    format!(
                        "forked chat {} to {} ({} history item(s), {} turn item(s))",
                        source_session_id,
                        session_id,
                        fork.copied_history_items,
                        fork.copied_turn_items
                    ),
                )
                .await
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionOperationApplied { target, result });
        });
    }

    fn spawn_session_interrupt(
        &self,
        target: SessionMutationRequestTarget,
        expected_active_turn: ActiveTurnExpectation,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let session_id = target.session_id;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-interrupt runtime");
            let result = runtime.block_on(async move {
                let (outcome, _) =
                    claim_exact_root_execution_stop(&app, session_id, expected_active_turn).await?;
                if matches!(outcome, ExactRootExecutionStopOutcome::TargetChanged) {
                    return Err("the running chat changed before interrupt was applied".to_string());
                }
                load_stopped_session_operation_projection(
                    &app,
                    session_id,
                    expected_active_turn,
                    std::time::Duration::from_secs(30),
                )
                .await
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionOperationApplied { target, result });
        });
    }

    fn spawn_project_delete(
        &self,
        target: ProjectDeleteRequestTarget,
        hidden_roots: Vec<Utf8PathBuf>,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        let project_id = target.project_id;
        let project_root_for_thread = target.project_root.clone();
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
                run_storage_maintenance_after_delete(&app)?;
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
                        quick_chat_workspace_directory().unwrap_or_else(|| {
                            fallback_workspace_after_project_delete(
                                &project_root_for_thread,
                                &hidden_roots,
                                app.session_service.store.paths().data_dir.as_path(),
                            )
                        })
                    });
                    if let Some(parent) = next_root.parent() {
                        std::fs::create_dir_all(parent.as_std_path())
                            .map_err(|error| error.to_string())?;
                    }
                    std::fs::create_dir_all(next_root.as_std_path())
                        .map_err(|error| error.to_string())?;
                    let process_runtime = app.process_runtime.clone();
                    app = AppBootstrap::rebuild_for_directory_with_process_runtime(
                        &next_root,
                        process_runtime,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                }
                let snapshot = load_snapshot_for_selection(&app, None)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(WorkspaceLoadResult { app, snapshot })
            });
            let _ = runtime_tx.send(RuntimeMessage::ProjectDeleted { target, result });
        });
    }
}
