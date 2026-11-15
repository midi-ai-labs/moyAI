use super::*;

impl DesktopController {
    pub(crate) fn access_mode_mutation_runtime_contract(&self) -> (String, bool) {
        let root_run_generation = self.root_run_generation();
        let agent_tree_active = self.current_agent_tree_active();
        (
            access_runtime_owner_token(
                root_run_generation,
                agent_tree_active,
                self.last_root_run_epoch(),
            ),
            !self.state.navigation_loading() && !self.state.background_mutation_pending(),
        )
    }

    pub(crate) fn access_mode_mutation_admission_open(&self) -> bool {
        self.access_mode_mutation_runtime_contract().1
    }

    pub(crate) fn session_settings_turn_config_mutation_admission_open(&self) -> bool {
        self.config_draft_mutation_admission_open() && !self.current_agent_tree_active()
    }

    pub(super) fn access_mode_persistence_target_relation(
        &self,
        target: &AccessModePersistenceTarget,
    ) -> AccessModePersistenceTargetRelation {
        let (runtime_owner_token, _) = self.access_mode_mutation_runtime_contract();
        let relation = access_mode_persistence_target_relation(
            target,
            &self.app.workspace.root,
            self.state.app_state.current_session_id,
            self.state.provider_config.config_generation,
            &runtime_owner_token,
        );
        if relation != AccessModePersistenceTargetRelation::Stale {
            return relation;
        }
        let after_root = self.access_mode_persistence_relation_after_root_finish(target);
        if after_root != AccessModePersistenceTargetRelation::Stale {
            return after_root;
        }
        self.access_mode_persistence_relation_after_tree_finish(target)
    }

    fn access_mode_persistence_relation_after_root_finish(
        &self,
        target: &AccessModePersistenceTarget,
    ) -> AccessModePersistenceTargetRelation {
        let Some(root_run_generation) = target.root_run_generation else {
            return AccessModePersistenceTargetRelation::Stale;
        };
        if target.workspace_root != self.app.workspace.root
            || target.config_generation != self.state.provider_config.config_generation
            || self.root_run_generation().is_some()
            || self.last_root_run_epoch() != root_run_generation
            || target.runtime_owner_token != format!("root:{root_run_generation}")
        {
            return AccessModePersistenceTargetRelation::Stale;
        }
        match (target.session_id, self.state.app_state.current_session_id) {
            (target_session_id, current_session_id) if target_session_id == current_session_id => {
                AccessModePersistenceTargetRelation::Exact
            }
            (None, Some(session_id)) => {
                AccessModePersistenceTargetRelation::AdoptedSession(session_id)
            }
            _ => AccessModePersistenceTargetRelation::Stale,
        }
    }

    fn access_mode_persistence_relation_after_tree_finish(
        &self,
        target: &AccessModePersistenceTarget,
    ) -> AccessModePersistenceTargetRelation {
        if target.root_run_generation.is_some()
            || target.workspace_root != self.app.workspace.root
            || target.config_generation != self.state.provider_config.config_generation
            || self.root_run_generation().is_some()
            || self.current_agent_tree_active()
            || target.runtime_owner_token != format!("tree:{}", self.last_root_run_epoch())
        {
            return AccessModePersistenceTargetRelation::Stale;
        }
        match (target.session_id, self.state.app_state.current_session_id) {
            (target_session_id, current_session_id) if target_session_id == current_session_id => {
                AccessModePersistenceTargetRelation::Exact
            }
            _ => AccessModePersistenceTargetRelation::Stale,
        }
    }

    pub(crate) fn toggle_access_mode_remembered(&mut self) -> bool {
        let session_service = self.app.session_service.clone();
        let expected_access_mode = self
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode;
        self.start_access_mode_persistence(
            ConfigEditorState::compare_and_set_global_access_mode,
            move |session_id, access_mode| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.block_on(async move {
                    session_service
                        .compare_and_set_root_session_access_mode(
                            session_id,
                            expected_access_mode,
                            access_mode,
                        )
                        .await
                        .and_then(|updated| {
                            updated.map(|update| Some(update.session)).ok_or_else(|| {
                                crate::error::SessionError::Message(format!(
                                    "session {session_id} access mode changed before this update"
                                ))
                            })
                        })
                        .map_err(|error| error.to_string())
                })
            },
        )
    }

    pub(super) fn start_access_mode_persistence<CompareAndSetGlobal, PersistSession>(
        &mut self,
        compare_and_set_global: CompareAndSetGlobal,
        persist_session: PersistSession,
    ) -> bool
    where
        CompareAndSetGlobal: FnMut(
                crate::config::AccessMode,
                crate::config::AccessMode,
            ) -> Result<Option<Utf8PathBuf>, String>
            + Send
            + 'static,
        PersistSession: FnOnce(SessionId, crate::config::AccessMode) -> Result<Option<SessionRecord>, String>
            + Send
            + 'static,
    {
        if !self.access_mode_mutation_admission_open() {
            self.state.set_status_message(
                "access mode cannot change while navigation or an owner mutation is active",
            );
            return false;
        }
        let old_effective_access_mode = self
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode;
        let access_mode = old_effective_access_mode.next();
        let (runtime_owner_token, _) = self.access_mode_mutation_runtime_contract();
        let target = AccessModePersistenceTarget {
            operation_id: self.state.begin_access_mode_persistence(),
            workspace_root: self.app.workspace.root.clone(),
            session_id: self.state.app_state.current_session_id,
            config_generation: self.state.provider_config.config_generation,
            root_run_generation: self.root_run_generation(),
            runtime_owner_token,
            old_global_access_mode: self.app.config.permissions.access_mode,
            old_effective_access_mode,
            access_mode,
        };
        let request_id = self.access_mode_persistence_requests.begin(target.clone());
        let runtime_tx = self.runtime_tx.clone();
        let worker_target = target.clone();
        let worker = Arc::new(AccessModePersistenceWorker::new(
            compare_and_set_global,
            persist_session,
        ));
        let initial_worker = worker.clone();
        std::thread::spawn(move || {
            let result = initial_worker.persist_initial_owners(&worker_target);
            let _ = runtime_tx.send(RuntimeMessage::AccessModePersisted {
                request_id,
                target: worker_target,
                phase: AccessModePersistencePhase::InitialOwners,
                worker: initial_worker,
                result,
            });
        });
        self.state
            .set_status_message(if target.session_id.is_some() {
                "saving access mode to global config and the current root session"
            } else {
                "saving access mode to global config"
            });
        true
    }

    pub(super) fn spawn_adopted_session_access_persistence(
        &self,
        request_id: LatestRequestId,
        target: AccessModePersistenceTarget,
        session_id: SessionId,
        remembered_path: Utf8PathBuf,
        worker: Arc<AccessModePersistenceWorker>,
    ) {
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let result = worker.persist_adopted_session(&target, session_id, remembered_path);
            let _ = runtime_tx.send(RuntimeMessage::AccessModePersisted {
                request_id,
                target,
                phase: AccessModePersistencePhase::AdoptedSession { session_id },
                worker,
                result,
            });
        });
    }

    pub(super) fn resume_pending_access_mode_adoption(&mut self, session_id: SessionId) -> bool {
        let Some(pending) = self.pending_access_mode_adoption.as_ref() else {
            return false;
        };
        let request_id = pending.request_id;
        let target = pending.target.clone();
        let request_is_current = self
            .access_mode_persistence_requests
            .is_current(request_id, &target);
        let operation_is_current = self
            .state
            .access_mode_persistence_is_current(target.operation_id);
        let relation = self.access_mode_persistence_target_relation(&target);
        if !request_is_current
            || !operation_is_current
            || relation != AccessModePersistenceTargetRelation::AdoptedSession(session_id)
        {
            let _ = self.pending_access_mode_adoption.take();
            let _ = self
                .access_mode_persistence_requests
                .finish_if_current(request_id, &target);
            let _ = self
                .state
                .finish_access_mode_persistence(target.operation_id);
            let _ = self.reload_config();
            self.state.set_status_message(
                "access mode owner changed before session admission; current configuration was reloaded",
            );
            return false;
        }
        let pending = self
            .pending_access_mode_adoption
            .take()
            .expect("pending access mode adoption checked above");
        self.spawn_adopted_session_access_persistence(
            pending.request_id,
            pending.target,
            session_id,
            pending.remembered_path,
            pending.worker,
        );
        self.state.set_status_message(
            "global access mode saved; saving the admitted current root session",
        );
        true
    }

    pub(super) fn settle_pending_access_mode_without_session(&mut self) {
        let Some(pending) = self.pending_access_mode_adoption.take() else {
            return;
        };
        let relation = self.access_mode_persistence_target_relation(&pending.target);
        let request_is_current = self
            .access_mode_persistence_requests
            .finish_if_current(pending.request_id, &pending.target);
        let operation_is_current = self
            .state
            .finish_access_mode_persistence(pending.target.operation_id);
        let target_is_current = request_is_current
            && operation_is_current
            && relation == AccessModePersistenceTargetRelation::Exact;
        if !target_is_current {
            let _ = self.reload_config();
            return;
        }
        let mut global_config = self.state.global_config().clone();
        global_config.permissions.access_mode = pending.target.access_mode;
        self.app.config = global_config.clone();
        self.state.replace_global_config(global_config);
        self.state.set_status_message(format!(
            "共通設定の承認方法を「{}」に保存しました。次の承認判断から適用されます。表示中の承認依頼は変更しません。",
            access_mode_display_label(pending.target.access_mode)
        ));
    }

    #[cfg(test)]
    pub(super) fn toggle_access_mode_with_persistence<CompareAndSetGlobal, PersistSession>(
        &mut self,
        compare_and_set_global: CompareAndSetGlobal,
        persist_session: PersistSession,
    ) -> bool
    where
        CompareAndSetGlobal: FnMut(
            crate::config::AccessMode,
            crate::config::AccessMode,
        ) -> Result<Option<Utf8PathBuf>, String>,
        PersistSession: FnOnce(SessionId, crate::config::AccessMode) -> Result<(), String>,
    {
        if !self.access_mode_mutation_admission_open() {
            self.state.set_status_message(
                "access mode cannot change while navigation or an owner mutation is active",
            );
            return false;
        }
        let access_mode = self
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode
            .next();
        let old_global_access_mode = self.app.config.permissions.access_mode;
        let current_root_session_id = self.state.app_state.current_session_id;
        let _remembered_path = match persist_desktop_access_mode_owners(
            old_global_access_mode,
            access_mode,
            current_root_session_id,
            compare_and_set_global,
            |session_id, access_mode| persist_session(session_id, access_mode),
        ) {
            Ok(path) => path,
            Err(error) => {
                let _ = self.reload_config();
                self.state.set_status_message(format!(
                    "access mode was not changed; configuration was reloaded: {error}"
                ));
                return false;
            }
        };
        if self.state.app_state.current_session_id != current_root_session_id {
            self.state.set_status_message(
                "access mode owner changed before commit; reload the current chat".to_string(),
            );
            return false;
        }
        self.app.config.permissions.access_mode = access_mode;
        self.state.provider_config.update_access_mode(access_mode);
        if let Some(session_id) = current_root_session_id {
            for session in &mut self.state.app_state.sessions {
                if session.id == session_id {
                    session.access_mode = access_mode;
                }
            }
            for summary in &mut self.state.app_state.loaded_sessions {
                if summary.session.id == session_id {
                    summary.session.access_mode = access_mode;
                }
            }
        }
        let scope = if current_root_session_id.is_some() {
            "共通設定とこのチャット"
        } else {
            "共通設定"
        };
        let config_message = format!(
            "{scope}の承認方法を「{}」に保存しました。次の承認判断から適用されます。表示中の承認依頼は変更しません。",
            access_mode_display_label(access_mode)
        );
        self.state.set_status_message(config_message);
        true
    }
}
