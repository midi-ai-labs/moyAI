use super::*;

impl DesktopController {
    pub(crate) fn start_run_at(
        &mut self,
        prompt: String,
        expected_active_turn: ActiveTurnExpectation,
    ) -> bool {
        if self.state.navigation_loading() {
            self.state
                .set_status_message("wait for navigation to finish before sending");
            return false;
        }
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            self.state.set_status_message("prompt is empty");
            return false;
        }
        let prompt_dispatch = crate::session::PromptDispatchPart::raw(&prompt);
        self.launch_run_with_options(prompt, prompt_dispatch, None, None, expected_active_turn)
    }

    pub(super) fn launch_run_with_options(
        &mut self,
        prompt: String,
        prompt_dispatch: crate::session::PromptDispatchPart,
        review_request: Option<ReviewRequest>,
        prompt_review_to_cancel: Option<PromptReviewTarget>,
        expected_active_turn: ActiveTurnExpectation,
    ) -> bool {
        if self.exit_drain.is_some() {
            self.state
                .set_status_message("moyAIを終了しています。新しい依頼は送信できません。");
            return false;
        }
        if self.state.app_state.prompt_review.is_some() && prompt_review_to_cancel.is_none() {
            self.state.set_status_message(
                "the active Prompt Review must be sent or cancelled through its exact target",
            );
            return false;
        }
        if self.state.background_mutation_pending() {
            self.state
                .set_status_message("wait for the current owner mutation to finish before sending");
            return false;
        }
        if self.state.navigation_loading() {
            self.state
                .set_status_message("wait for navigation to finish before starting a run");
            return false;
        }
        if self.state.post_run_refresh_pending() {
            self.state.set_status_message(
                "wait for the completed task to finish refreshing before sending",
            );
            return false;
        }
        if matches!(expected_active_turn, ActiveTurnExpectation::Turn { .. }) {
            if review_request.is_none()
                && prompt_review_to_cancel.is_none()
                && !prompt.trim().is_empty()
            {
                return self.launch_active_turn_steer(prompt, expected_active_turn);
            }
            self.state
                .set_status_message("the captured active turn cannot admit this request");
            return false;
        }
        if self.run_lifecycle.root_is_active()
            || matches!(
                self.state.app_state.run_status,
                crate::tui::state::RunStatus::Running
            )
        {
            self.state.set_status_message(
                "the idle run owner changed before submission; refresh and try again",
            );
            return false;
        }
        if prompt.trim().is_empty() && review_request.is_none() {
            return false;
        }
        if !self.state.composer.retained_images.is_empty()
            && self.state.app_state.current_session_id.is_none()
        {
            self.state.set_status_message(
                "編集する会話が変わりました。元の発言から編集をやり直してください。",
            );
            return false;
        }
        self.invalidate_session_search_requests();
        let run_generation = self.next_root_run_generation;
        let Some(next_generation) = run_generation.checked_add(1) else {
            self.state
                .set_status_message("desktop run generation is exhausted; restart moyAI");
            return false;
        };
        self.next_root_run_generation = next_generation;
        let image_paths = self.state.composer.image_attachment_paths.clone();
        let run_control = RunControl::new();
        let hub_route = match self
            .state
            .hub_connection
            .as_ref()
            .map(|hub| hub.begin_turn(crate::hub::HubReviewContext::Main, run_control.token()))
            .transpose()
        {
            Ok(route) => route.flatten(),
            Err(error) => {
                self.state.set_status_message(error.to_string());
                return false;
            }
        };
        self.state.begin_agent_run();
        let request = RunRequest {
            prompt: prompt.clone(),
            session_id: self.state.app_state.current_session_id,
            continue_last: false,
            title: self
                .state
                .app_state
                .current_session_id
                .is_none()
                .then(|| NEW_SESSION_PLACEHOLDER_TITLE.to_string()),
            cwd: self.app.workspace.cwd.clone(),
            config: RunConfigInput::Resolved(self.state.provider_config.effective_config.clone()),
            output_mode: OutputMode::Human,
            show_reasoning_summary: true,
            prompt_dispatch: Some(prompt_dispatch.clone()),
            editor_context: Some(self.current_editor_context()),
            review_request,
            image_paths,
            run_control: run_control.clone(),
            session_access_mode_adoption: None,
            agent_confirmation: None,
            agent_context: None,
            admission_kind: crate::app::RunAdmissionKind::NewUserRun,
            expected_active_turn,
        };
        self.run_lifecycle.begin(run_generation, run_control);
        self.pending_root_submission = Some(PendingRootSubmission {
            run_generation,
            owner_workspace_path: self.root_submission_owner_workspace_path(),
            owner_session_id: request.session_id,
            prompt_dispatch,
            image_paths: self.state.composer.image_attachment_paths.clone(),
            prompt_review_to_cancel,
        });
        let run_service = hub_route.as_ref().map_or_else(
            || self.app.run_service.clone(),
            |route| Arc::new(self.app.run_service.with_hub_turn(route.clone())),
        );
        let run_service =
            if self.state.composer.retained_images.is_empty() {
                run_service
            } else {
                let session_id = request
                    .session_id
                    .expect("retained image session was checked before admission");
                Arc::new(run_service.with_retained_edit_images(
                    session_id,
                    self.state.composer.retained_images.clone(),
                ))
            };
        let runtime_tx = self.runtime_tx.clone();
        let control_tx = self.control_tx.clone();
        let next_permission_request_id = self.next_permission_request_id.clone();
        let notification_title = request
            .title
            .clone()
            .unwrap_or_else(|| self.state.current_session_label());
        let worker = self
            .root_task_runtime
            .spawn(run_generation, move || async move {
                let mut request = request;
                let worker_run_control = request.run_control.clone();
                let root_run_control = request.run_control.clone();
                let mut renderer = DesktopRenderer {
                    runtime_tx,
                    bootstrap_control: control_tx.clone(),
                    run_generation,
                    notification_title: notification_title.clone(),
                    notified_terminal: false,
                    control_result: None,
                };
                let mut prompt = SharedConfirmationPrompt::new_with_root_control(
                    DesktopConfirmationPrompt {
                        control: control_tx.clone(),
                        next_permission_request_id,
                    },
                    root_run_control,
                );
                request.agent_confirmation = Some(prompt.clone());
                let result = run_service
                    .execute(AppCommand::Run(request), &mut renderer, &mut prompt)
                    .await
                    .map_err(|error| error.to_string());
                if let Some(route) = &hub_route {
                    route.finish().await;
                }
                match &result {
                    Ok(AppCommandOutcome::Turn(summary)) if !renderer.notified_terminal => {
                        let notification_body =
                            run_completion_notification_body(&renderer.notification_title, summary);
                        send_windows_desktop_notification("moyAI", &notification_body);
                    }
                    Err(error)
                        if !renderer.notified_terminal
                            && desktop_run_failure_notification_allowed(
                                worker_run_control.cause().as_ref(),
                            ) =>
                    {
                        let notification_body = run_error_notification_body(
                            &renderer.notification_title,
                            &crate::tui::state::RunStatus::Failed,
                            error,
                        );
                        send_windows_desktop_notification("moyAI", &notification_body);
                    }
                    _ => {}
                }
                match result {
                    Ok(AppCommandOutcome::Turn(summary)) => {
                        publish_desktop_run_finished(&control_tx, run_generation, Ok(summary));
                    }
                    Ok(AppCommandOutcome::ControlCompleted) => {
                        if let Some(result) = renderer.control_result.take() {
                            // This reports the real control result on the same worker-completion FIFO.
                            let _ = control_tx.send(RuntimeMessage::ControlFinished {
                                run_generation,
                                result,
                            });
                        } else {
                            publish_desktop_run_finished(
                                &control_tx,
                                run_generation,
                                Err("the completed control command did not return a displayable result".to_string()),
                            );
                        }
                    }
                    Err(error) => {
                        publish_desktop_run_finished(&control_tx, run_generation, Err(error));
                    }
                }
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                self.discard_pending_root_submission(run_generation);
                self.run_lifecycle.finish_root();
                self.state.finish_agent_run();
                self.state.app_state.run_status = crate::tui::state::RunStatus::Failed;
                self.state
                    .set_status_message(format!("failed to start desktop run worker: {error}"));
                return false;
            }
        };
        if let Err(worker) = self.run_lifecycle.attach_worker(run_generation, worker) {
            worker.abort();
            self.discard_pending_root_submission(run_generation);
            self.run_lifecycle.finish_root();
            self.state.finish_agent_run();
            self.state.app_state.run_status = crate::tui::state::RunStatus::Failed;
            self.state
                .set_status_message("desktop run owner changed before worker attachment");
            return false;
        }
        true
    }

    pub(crate) fn cancel_root_run_at_generation(&mut self, generation: u64) -> bool {
        let (root_stop_attempt, root_scope_control) =
            match self.run_lifecycle.begin_stop_attempt(generation) {
                DesktopRootStopAttemptAdmission::Acquired {
                    attempt,
                    run_control,
                } => (attempt, run_control),
                DesktopRootStopAttemptAdmission::AlreadyPending => {
                    self.state
                        .set_status_message("the exact root task Stop is already being validated");
                    return true;
                }
                DesktopRootStopAttemptAdmission::NotOwned => return false,
                DesktopRootStopAttemptAdmission::Exhausted => {
                    self.state.set_status_message(
                        "root Stop attempt identity is exhausted; restart moyAI",
                    );
                    return false;
                }
            };
        let stop_plan = match self
            .app
            .run_service
            .seal_root_execution_for_stop(&root_scope_control)
        {
            Ok(crate::app::run_service::RootExecutionStopSealOutcome::Acquired(plan)) => plan,
            Ok(crate::app::run_service::RootExecutionStopSealOutcome::AlreadySealed) => {
                self.run_lifecycle.finish_stop_attempt(root_stop_attempt);
                self.state
                    .set_status_message("the exact root task Stop is already being validated");
                return true;
            }
            Ok(crate::app::run_service::RootExecutionStopSealOutcome::Rejected) => {
                self.run_lifecycle.finish_stop_attempt(root_stop_attempt);
                return false;
            }
            Err(error) => {
                self.run_lifecycle.finish_stop_attempt(root_stop_attempt);
                self.state.set_status_message(format!(
                    "failed to seal the exact root Stop target: {error}"
                ));
                return false;
            }
        };
        if !stop_plan.has_durable_owner() {
            let claim = self
                .app
                .run_service
                .claim_unadmitted_root_execution_stop(&root_scope_control, stop_plan);
            self.run_lifecycle.finish_stop_attempt(root_stop_attempt);
            return match claim {
                Ok(claim)
                    if claim
                        .local_stop
                        .is_some_and(crate::runtime::RootExecutionLocalStop::stop_accepted) =>
                {
                    self.durable_agent_activity_refresh_requests.clear();
                    self.state.mark_run_stop_requested(
                        "run cancellation requested",
                        "停止を要求しました。現在の処理を中断しています。",
                    );
                    true
                }
                Ok(_) => false,
                Err(error) => {
                    self.state.set_status_message(format!(
                        "failed to stop the pre-admission root task: {error}"
                    ));
                    false
                }
            };
        }
        let Some(session_id) = stop_plan.session_id_hint() else {
            self.run_lifecycle.finish_stop_attempt(root_stop_attempt);
            self.state
                .set_status_message("the sealed root Stop plan lost its durable session owner");
            return false;
        };
        self.state.mark_post_run_refresh_pending();
        self.spawn_root_session_cancel_persist(
            generation,
            session_id,
            root_scope_control,
            stop_plan,
            root_stop_attempt,
        );
        self.state
            .set_status_message("validating the exact root task Stop target...");
        true
    }

    pub(super) fn settle_control_finished(
        &mut self,
        run_generation: u64,
        result: DesktopControlResult,
    ) {
        if !self.run_lifecycle.owns(run_generation) {
            return;
        }
        let pending = self
            .pending_root_submission
            .take_if(|pending| pending.run_generation == run_generation);
        self.run_lifecycle.finish_root();
        self.settle_pending_permission_after_root_finish();
        self.state.finish_agent_run();
        let Some(pending) = pending else {
            return;
        };
        if pending.owner_workspace_path != self.root_submission_owner_workspace_path()
            || pending.owner_session_id != self.state.app_state.current_session_id
        {
            return;
        }
        if let Some(target) = &pending.prompt_review_to_cancel {
            self.cancel_prompt_review_if_current(target);
        }
        // Goal controls do not admit a Turn or consume image attachments. Retain canonical
        // history and the previous terminal state; acknowledge only the submitted text.
        self.advance_composer_commit_generation();
        self.state
            .set_typed_status_message(DesktopStatusCode::GoalControl, result.description());
    }

    pub(super) fn settle_root_finished(
        &mut self,
        run_generation: u64,
        result: Result<RunSummary, String>,
    ) {
        if !self.run_lifecycle.owns(run_generation) {
            return;
        }
        if self.state.app_state.current_session_id.is_none() {
            self.settle_pending_access_mode_without_session();
        }
        match result {
            Ok(summary) => {
                self.commit_pending_root_submission(run_generation, None);
                self.run_lifecycle.finish_root();
                self.settle_pending_permission_after_root_finish();
                self.state.finish_agent_run();
                self.state.mark_post_run_refresh_pending();
                self.state.apply_run_summary(summary);
                self.stop_session_runtime_listener();
                self.refresh_current_session_after_terminal_run();
            }
            Err(error) => {
                self.discard_pending_root_submission(run_generation);
                self.run_lifecycle.finish_root();
                self.settle_pending_permission_after_root_finish();
                self.state.finish_agent_run();
                if !matches!(
                    self.state.app_state.run_status,
                    crate::tui::state::RunStatus::Cancelled
                ) {
                    self.state.app_state.run_status = crate::tui::state::RunStatus::Failed;
                }
                if !self.state.status_code.is_terminal_interruption() {
                    self.state.set_status_message(error);
                }
                self.stop_session_runtime_listener();
                if self.state.app_state.current_session_id.is_some() {
                    self.state.mark_post_run_refresh_pending();
                    self.refresh_current_session_after_terminal_run();
                } else {
                    self.state.clear_post_run_refresh_pending();
                }
            }
        }
    }

    pub(super) fn apply_canonical_session_event(
        &mut self,
        listener_generation: u64,
        target: SessionRuntimeListenerTarget,
        event: RuntimeEvent,
    ) {
        let listener_is_current = self
            .session_runtime_listener
            .as_ref()
            .is_some_and(|listener| {
                listener.generation == listener_generation && listener.target == target
            });
        if !listener_is_current
            || target.workspace_root != self.app.workspace.root
            || self.state.app_state.current_session_id != Some(target.session_id)
            || event.session_id != target.session_id
            || event.turn_id != target.turn_id
        {
            return;
        }
        if let RuntimeEventMsg::TurnTerminal { terminal } = event.msg {
            self.run_lifecycle.observe_terminal_event();
            self.state.apply_run_summary(RunSummary::from_terminal(
                target.session_id,
                target.turn_id,
                *terminal,
            ));
            self.state.finish_agent_run();
            self.state.mark_post_run_refresh_pending();
            self.stop_session_runtime_listener();
            self.refresh_current_session_after_terminal_run();
        } else if !self.session_projection_refresh_requests.is_pending() {
            self.spawn_latest_live_session_refresh(target.session_id);
        }
    }

    pub(super) fn apply_root_run_event(&mut self, run_generation: u64, event: RunEvent) {
        if !self.run_lifecycle.owns(run_generation) {
            return;
        }
        let admitted_listener_target = match &event {
            RunEvent::UserTurnStored { session_id, turn } => Some(SessionRuntimeListenerTarget {
                workspace_root: self.app.workspace.root.clone(),
                session_id: *session_id,
                turn_id: turn.turn_id,
            }),
            _ => None,
        };
        if let RunEvent::UserTurnStored { turn, .. } = &event {
            self.commit_pending_root_submission(run_generation, Some(turn));
        }
        if self.run_lifecycle.cancellation_requested()
            && !run_event_is_terminal(&event)
            && !matches!(&event, RunEvent::SessionStarted { .. })
        {
            return;
        }
        let refresh_session_id = match &event {
            RunEvent::SessionStarted { session_id, .. }
            | RunEvent::SessionTitleUpdated { session_id, .. } => Some(*session_id),
            _ => None,
        };
        if matches!(&event, RunEvent::SessionStarted { .. }) {
            self.durable_agent_activity_refresh_failures = 0;
        }
        let live_refresh_session_id = event
            .session_id()
            .or(self.state.app_state.current_session_id);
        if run_event_is_terminal(&event) {
            self.run_lifecycle.observe_terminal_event();
        }
        self.state.apply_run_event(&event);
        if let Some(target) = admitted_listener_target
            && self.state.app_state.current_session_id == Some(target.session_id)
        {
            // UserTurnStored is the durable admission receipt available on the
            // bootstrap FIFO today. Attach its exact cursor immediately instead of
            // waiting for a possibly reordered session snapshot refresh.
            self.reconcile_session_runtime_listener(Some(target));
        }
        if let Some(message) = desktop_terminal_status_message(&event) {
            self.state.set_status_message_preserving_code(message);
        }
        if let RunEvent::SessionStarted { session_id, .. } = &event {
            self.resume_pending_access_mode_adoption(*session_id);
            self.persist_preferences();
        }
        if live_event_requires_canonical_refresh(&event)
            && live_refresh_session_id == self.state.app_state.current_session_id
        {
            if let Some(session_id) = live_refresh_session_id {
                // The main transcript is a continuously merged suffix. Always refresh
                // its latest bounded chunk so expanding older history never leaves live
                // output pinned to an obsolete earlier offset.
                self.spawn_latest_live_session_refresh(session_id);
            }
        }
        if run_event_is_terminal(&event) {
            self.state.mark_post_run_refresh_pending();
        }
        if let Some(session_id) = refresh_session_id {
            self.spawn_snapshot_refresh_for_session(session_id);
        }
    }
}
