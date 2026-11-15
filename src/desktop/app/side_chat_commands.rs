use super::*;

impl DesktopController {
    pub(super) fn side_chat_projection(&self) -> DesktopSideChatProjection {
        let Some(owner_session_id) = self.state.app_state.current_session_id else {
            return DesktopSideChatProjection::default();
        };
        let current_owner_append_position = self
            .app
            .store
            .protocol_event_store()
            .canonical_snapshot_for_session(
                owner_session_id,
                ProtocolPageRequest::Latest { limit: 1 },
                ProtocolPageRequest::Latest { limit: 1 },
            )
            .ok()
            .and_then(|snapshot| snapshot.fence.append_position);
        let context_as_of_append_position =
            current_owner_append_position.map(|position| position.to_string());
        let context_truncated = false;
        let side_chat_defaults = &self.state.provider_config.effective_config.side_chat;
        let default_api_key_env = side_chat_defaults.api_key_env.clone().unwrap_or_default();
        let default_base_url = side_chat_defaults.base_url.clone();
        let default_model = side_chat_defaults.model.clone();
        let default_system_prompt = side_chat_defaults.system_prompt.clone();
        let default_profile = side_chat_defaults.provider_profile.as_str().to_string();
        let durable = match self
            .app
            .store
            .side_chat_repo()
            .conversation_projection(owner_session_id)
        {
            Ok(projection) => projection,
            Err(error) => {
                return DesktopSideChatProjection {
                    owner_session_id: Some(owner_session_id.to_string()),
                    model: default_model,
                    system_prompt: default_system_prompt,
                    base_url: default_base_url,
                    provider_profile: default_profile,
                    api_key_env: default_api_key_env,
                    status: "failed".to_string(),
                    phase: "storage read failed".to_string(),
                    last_error: error.to_string(),
                    context_as_of_append_position,
                    context_truncated,
                    ..DesktopSideChatProjection::default()
                };
            }
        };
        let Some(durable) = durable else {
            return DesktopSideChatProjection {
                owner_session_id: Some(owner_session_id.to_string()),
                model: default_model,
                system_prompt: default_system_prompt,
                base_url: default_base_url,
                provider_profile: default_profile,
                api_key_env: default_api_key_env,
                status: "idle".to_string(),
                last_error: self
                    .side_chat_errors
                    .get(&owner_session_id)
                    .cloned()
                    .unwrap_or_default(),
                generation: "0".to_string(),
                context_as_of_append_position,
                context_truncated,
                ..DesktopSideChatProjection::default()
            };
        };
        let binding = durable.binding;
        let (draft_text, draft_quote, draft_error) =
            match decode_persisted_side_chat_draft(&binding.persisted_draft) {
                Ok(draft) => (
                    draft.text,
                    draft
                        .quote
                        .map(|quote| DesktopSideChatDraftQuoteProjection {
                            source_kind: quote.source_kind.as_str().to_string(),
                            source_history_item_id: quote.source_history_item_id.to_string(),
                            source_append_position: quote
                                .source_append_position
                                .map(|position| position.to_string()),
                            selected_text: quote.selected_text,
                        }),
                    None,
                ),
                Err(error) => (String::new(), None, Some(error)),
            };
        let draft_is_valid = draft_error.is_none();
        let mut messages = durable
            .messages
            .into_iter()
            .map(|message| DesktopSideChatMessageProjection {
                id: message.id.to_string(),
                sequence_no: usize::try_from(message.sequence_no).unwrap_or(usize::MAX),
                role: match message.role {
                    crate::storage::side_chat::SideChatConversationRole::User => "user",
                    crate::storage::side_chat::SideChatConversationRole::Assistant => "assistant",
                    crate::storage::side_chat::SideChatConversationRole::Error => "error",
                }
                .to_string(),
                content: message.content,
            })
            .collect::<Vec<_>>();
        let active = self
            .side_chat_runs
            .get(&owner_session_id)
            .filter(|run| run.side_chat_id == binding.id.to_string());
        let active_context = active.and_then(|_| self.side_chat_contexts.get(&owner_session_id));
        let context_as_of_append_position = active_context
            .and_then(|context| context.as_of_append_position)
            .or(current_owner_append_position)
            .map(|position| position.to_string());
        let context_truncated = active_context.is_some_and(|context| context.truncated);
        if let Some(run) = active
            && !run.streamed_text.is_empty()
        {
            let next_sequence = messages
                .last()
                .map_or(0, |message| message.sequence_no.saturating_add(1));
            messages.push(DesktopSideChatMessageProjection {
                id: format!("stream:{}:{}", binding.id, run.run_generation),
                sequence_no: next_sequence,
                role: "assistant".to_string(),
                content: run.streamed_text.clone(),
            });
        }
        let deleting = binding.delete_requested_at_ms.is_some();
        let direct_mode = self.state.hub_connection.as_ref().is_none_or(|hub| {
            hub.projection_now().side_chat_mode == crate::hub::HubRouteMode::Direct
        });
        let needs_direct_capture = binding.route_kind == SideChatRouteKind::Hub && direct_mode;
        let direct_provider_capture = needs_direct_capture.then(|| {
            let defaults = &self.state.provider_config.effective_config.side_chat;
            let valid = SideChatProviderTarget::try_from(defaults).is_ok();
            crate::desktop::web_model::DesktopSideChatDirectCaptureProjection {
                base_url: defaults.base_url.clone(), model: defaults.model.clone(),
                provider_profile: defaults.provider_profile.as_str().to_string(),
                enabled: valid && !deleting && active.is_none() && durable.status != SessionStatus::Running,
                reason: if valid { "この会話はHubで作成されました。履歴を保持したまま、下記のDirect設定をこの会話へ一度だけ登録します。" }
                    else { "サイドチャットの直接接続先とモデルを設定してください。会話履歴はそのまま残ります。" }.to_string(),
            }
        });
        DesktopSideChatProjection {
            configured: true,
            deleting,
            chat_id: Some(binding.id.to_string()),
            owner_session_id: Some(owner_session_id.to_string()),
            model: binding.model,
            system_prompt: binding.system_prompt,
            base_url: binding.base_url,
            provider_profile: binding.provider_profile.as_str().to_string(),
            api_key_env: binding.api_key_env.unwrap_or_default(),
            status: active
                .map(|_| "running".to_string())
                .unwrap_or_else(|| durable.status.key().to_string()),
            phase: active.map_or_else(
                || {
                    if deleting {
                        "deletion pending".to_string()
                    } else {
                        String::new()
                    }
                },
                |run| run.phase.clone(),
            ),
            last_error: draft_error
                .or_else(|| self.side_chat_errors.get(&owner_session_id).cloned())
                .or(durable.last_error)
                .unwrap_or_default(),
            generation: binding.request_generation.to_string(),
            draft_text,
            draft_quote,
            draft_revision: binding.draft_revision.to_string(),
            context_scope: "owner_session".to_string(),
            context_as_of_append_position,
            context_truncated,
            messages,
            can_send: draft_is_valid
                && !needs_direct_capture
                && !deleting
                && active.is_none()
                && durable.status != SessionStatus::Running,
            can_cancel: !deleting && active.is_some(),
            direct_provider_capture,
        }
    }

    #[cfg(test)]
    pub(crate) fn configure_side_chat(
        &mut self,
        owner_session_id: SessionId,
        base_url: String,
        model: String,
        system_prompt: String,
        provider_profile: ProviderProfile,
    ) -> Result<(), String> {
        self.ensure_current_side_chat_owner(owner_session_id)?;
        if self.side_chat_runs.contains_key(&owner_session_id) {
            return Err("the side chat must be stopped before its provider is changed".to_string());
        }
        let mut side_chat_config = self
            .state
            .provider_config
            .effective_config
            .side_chat
            .clone();
        side_chat_config.base_url = base_url;
        side_chat_config.model = model;
        side_chat_config.system_prompt = system_prompt;
        side_chat_config.provider_profile = provider_profile;
        let target = SideChatProviderTarget::try_from(&side_chat_config)
            .map_err(|error| error.to_string())?;
        self.app
            .store
            .side_chat_repo()
            .configure(owner_session_id, target)
            .map_err(|error| error.to_string())?;
        self.side_chat_contexts.remove(&owner_session_id);
        self.side_chat_errors.remove(&owner_session_id);
        Ok(())
    }

    /// Materializes the current Global Side Chat defaults as a stable snapshot
    /// for this owner session. Existing conversations keep their captured
    /// provider identity. A Hub-origin conversation can explicitly capture its
    /// first Direct provider while idle without replacing its history.
    pub(crate) fn ensure_side_chat(&mut self, owner_session_id: SessionId) -> Result<(), String> {
        self.ensure_current_side_chat_owner(owner_session_id)?;
        if let Some(existing) = self
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .map_err(|error| error.to_string())?
        {
            if existing.delete_requested_at_ms.is_some() {
                return Err("サイドチャットの削除が終わるまでお待ちください。".into());
            }
            self.side_chat_errors.remove(&owner_session_id);
            return Ok(());
        }
        let mut defaults = self
            .state
            .provider_config
            .effective_config
            .side_chat
            .clone();
        let hub = self
            .state
            .hub_connection
            .as_ref()
            .map(|hub| hub.projection_now());
        let hub_origin = hub
            .as_ref()
            .is_some_and(|hub| hub.side_chat_mode == crate::hub::HubRouteMode::Hub);
        if hub_origin {
            let hub = hub.expect("Hub mode");
            if !hub.can_enable_side_chat_hub {
                return Err(
                    "Hubに接続し、サイドチャットで使うモデルを選んで保存してください。".into(),
                );
            }
            let review = hub
                .side_chat_review
                .ok_or_else(|| "サイドチャットで使うHubのモデルを選んでください。".to_string())?;
            defaults.base_url = hub.endpoint;
            defaults.model = review.selection.preferred_model_id;
            defaults.provider_profile = ProviderProfile::OpenAiCompatible;
            defaults.api_key_env = None;
        }
        let mut target =
            SideChatProviderTarget::try_from(&defaults).map_err(|error| error.to_string())?;
        if hub_origin {
            target.route_kind = SideChatRouteKind::Hub;
        }
        self.app
            .store
            .side_chat_repo()
            .ensure(owner_session_id, target)
            .map_err(|error| error.to_string())?;
        self.side_chat_contexts.remove(&owner_session_id);
        self.side_chat_errors.remove(&owner_session_id);
        Ok(())
    }

    pub(crate) fn capture_side_chat_direct_provider(
        &mut self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
        expected_draft_revision: u64,
    ) -> Result<(), String> {
        self.ensure_current_side_chat_owner(owner_session_id)?;
        if self.side_chat_runs.contains_key(&owner_session_id)
            || self.state.hub_connection.as_ref().is_some_and(|hub| {
                hub.projection_now().side_chat_mode != crate::hub::HubRouteMode::Direct
            })
        {
            return Err(
                "サイドチャットを直接接続に切り替え、実行が終わってから設定を適用してください。"
                    .into(),
            );
        }
        self.app
            .store
            .side_chat_repo()
            .capture_direct_provider(
                owner_session_id,
                side_chat_id,
                expected_generation,
                expected_draft_revision,
                &self.state.provider_config.effective_config.side_chat,
            )
            .map_err(|error| error.to_string())?;
        self.side_chat_errors.remove(&owner_session_id);
        Ok(())
    }

    pub(crate) fn save_side_chat_draft(
        &mut self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_draft_revision: u64,
        text: String,
        quote: Option<SideChatQuoteRequest>,
    ) -> Result<(), String> {
        self.ensure_current_side_chat_owner(owner_session_id)?;
        let persisted_draft = encode_persisted_side_chat_draft(text, quote.as_ref())?;
        self.app
            .store
            .side_chat_repo()
            .update_draft(
                owner_session_id,
                side_chat_id,
                expected_draft_revision,
                persisted_draft,
            )
            .map_err(|error| error.to_string())?;
        self.side_chat_errors.remove(&owner_session_id);
        Ok(())
    }

    pub(crate) fn start_side_chat(
        &mut self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
        expected_draft_revision: u64,
        expected_owner_append_position: Option<i64>,
        quote: Option<SideChatQuoteRequest>,
        text: String,
    ) -> Result<(), String> {
        self.start_side_chat_with_ack_timeout(
            owner_session_id,
            side_chat_id,
            expected_generation,
            expected_draft_revision,
            expected_owner_append_position,
            quote,
            text,
            SIDE_CHAT_START_ACK_TIMEOUT,
        )
    }

    pub(super) fn start_side_chat_with_ack_timeout(
        &mut self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
        expected_draft_revision: u64,
        expected_owner_append_position: Option<i64>,
        quote: Option<SideChatQuoteRequest>,
        text: String,
        admission_ack_timeout: std::time::Duration,
    ) -> Result<(), String> {
        if self.exit_drain.is_some() {
            return Err("moyAIを終了しています。新しい依頼は送信できません。".to_string());
        }
        self.ensure_current_side_chat_owner(owner_session_id)?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("side chat text must not be empty".to_string());
        }
        if self.side_chat_runs.contains_key(&owner_session_id) {
            return Err("the side chat already has an active request".to_string());
        }
        let binding = self
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "the selected main session has no side chat".to_string())?;
        if binding.id != side_chat_id || binding.request_generation != expected_generation {
            return Err("the side chat target changed before the action".to_string());
        }
        if binding.draft_revision != expected_draft_revision {
            return Err(format!(
                "the side chat draft changed before the action: expected {expected_draft_revision}, current {}",
                binding.draft_revision
            ));
        }
        decode_persisted_side_chat_draft(&binding.persisted_draft)?;
        let provisional_generation = expected_generation
            .checked_add(1)
            .ok_or_else(|| "side chat request generation is exhausted".to_string())?;
        let conversation_session_id = binding.conversation_session_id;
        let preflight_context_window = binding.context_window;
        let preflight_system_prompt = side_chat_system_prompt(&binding.system_prompt);
        let preflight_provider_target = binding.provider_target();
        let side_chat_id_text = binding.id.to_string();
        let turn_id = TurnId::new();
        let question = text.clone();
        let user_turn = UserTurn {
            turn_id,
            items: vec![UserInputItem::Text { text }],
            prompt_dispatch: None,
            editor_context: None,
        };
        let run_control = RunControl::new();
        let cancel = run_control.token();
        let hub_route = self
            .state
            .hub_connection
            .as_ref()
            .map(|hub| hub.begin_turn(crate::hub::HubReviewContext::SideChat, cancel.clone()))
            .transpose()
            .map_err(|error| error.to_string())?
            .flatten();
        if hub_route.is_none() && binding.route_kind == SideChatRouteKind::Hub {
            return Err("この会話にDirect設定を適用してから送信してください。Hubの管理URLには送信しません。".into());
        }
        let worker_cancel = cancel.clone();
        let worker_run_control = run_control.clone();
        let store = self.app.store.clone();
        let stream_owner_session_id = owner_session_id;
        let stream_side_chat_id = side_chat_id_text.clone();
        let runtime_tx = self.runtime_tx.clone();
        let control_tx = self.control_tx.clone();
        let (worker_start_tx, worker_start_rx) = tokio::sync::oneshot::channel::<()>();
        let (admission_ack_tx, admission_ack_rx) =
            mpsc::sync_channel::<Result<(u64, SideChatContextMetadata), String>>(1);
        let worker =
            self.side_chat_task_runtime
                .spawn(provisional_generation, move || async move {
                    if worker_start_rx.await.is_err() {
                        return;
                    }
                    let publish_start_failure = |error: String| {
                        let _ = admission_ack_tx.send(Err(error.clone()));
                        let _ = control_tx.send(RuntimeMessage::SideChatFinished {
                            owner_session_id: stream_owner_session_id,
                            side_chat_id: stream_side_chat_id.clone(),
                            run_generation: provisional_generation,
                            result: Err(error),
                        });
                    };
                    let process_run_lease =
                        match store.try_acquire_run_process_lease(conversation_session_id) {
                            Ok(lease) => lease,
                            Err(error) => {
                                publish_start_failure(error.to_string());
                                return;
                            }
                        };
                    let active_run_lease = match store
                        .active_runs()
                        .try_start(conversation_session_id, worker_run_control)
                    {
                        Ok(lease) => lease,
                        Err(error) => {
                            drop(process_run_lease);
                            publish_start_failure(error.to_string());
                            return;
                        }
                    };
                    if let Err(error) = active_run_lease.set_turn_id(turn_id) {
                        drop(active_run_lease);
                        drop(process_run_lease);
                        publish_start_failure(error.to_string());
                        return;
                    }
                    let prepared = match prepare_side_chat_input(
                        &store,
                        stream_owner_session_id,
                        conversation_session_id,
                        expected_owner_append_position,
                        quote,
                        &question,
                        preflight_system_prompt,
                        preflight_context_window.saturating_sub(hub_route.as_ref().map_or(
                            0,
                            |route| {
                                route
                                    .model_system_prompt_reservation()
                                    // Hub reserves bytes/4; Side budgets UTF-8 bytes.
                                    .saturating_mul(4)
                                    .min(u32::MAX as usize) as u32
                            },
                        )),
                    )
                    .await
                    {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            drop(active_run_lease);
                            drop(process_run_lease);
                            publish_start_failure(error);
                            return;
                        }
                    };
                    let admitted = match store
                        .side_chat_repo()
                        .claim_and_admit_request(
                            stream_owner_session_id,
                            side_chat_id,
                            expected_generation,
                            expected_draft_revision,
                            preflight_provider_target,
                            turn_id,
                            &user_turn,
                        )
                        .await
                    {
                        Ok(admitted) => admitted,
                        Err(error) => {
                            drop(active_run_lease);
                            drop(process_run_lease);
                            publish_start_failure(error.to_string());
                            return;
                        }
                    };
                    let run_generation = admitted.generation;
                    let mut profile = side_chat_request_profile(&admitted.binding);
                    if let Some(route) = &hub_route {
                        profile.base_url = route.hub_endpoint().into();
                        profile.model = route.logical_model().into();
                        profile.provider_profile = crate::config::ProviderProfile::OpenAiCompatible;
                        profile.api_key_env = None;
                        profile.extra_headers.clear();
                        profile.hub_route = Some(route.clone());
                    }
                    profile.system_prompt = prepared.system_prompt.clone();
                    let admission_id = admitted.admission.admission_id;
                    if admission_ack_tx
                        .send(Ok((run_generation, prepared.context.clone())))
                        .is_err()
                    {
                        // The command owner disappeared after admission. Preserve canonical truth by
                        // settling the admitted turn instead of leaving an unowned Running session.
                        worker_cancel.cancel();
                    }
                    let result = execute_admitted_canonical_side_chat(
                        store,
                        conversation_session_id,
                        admission_id,
                        turn_id,
                        profile,
                        prepared.messages,
                        worker_cancel,
                        |event| {
                            let message = match event {
                                SideChatStreamEvent::TextDelta(delta) => {
                                    RuntimeMessage::SideChatDelta {
                                        owner_session_id: stream_owner_session_id,
                                        side_chat_id: stream_side_chat_id.clone(),
                                        run_generation,
                                        delta,
                                    }
                                }
                                SideChatStreamEvent::ProviderPhase(event) => {
                                    RuntimeMessage::SideChatPhase {
                                        owner_session_id: stream_owner_session_id,
                                        side_chat_id: stream_side_chat_id.clone(),
                                        run_generation,
                                        phase: event.phase.as_str().to_string(),
                                    }
                                }
                            };
                            let _ = runtime_tx.try_send(message);
                        },
                    )
                    .await;
                    if let Some(route) = &hub_route {
                        route.finish().await;
                    }
                    drop(active_run_lease);
                    drop(process_run_lease);
                    let _ = control_tx.send(RuntimeMessage::SideChatFinished {
                        owner_session_id: stream_owner_session_id,
                        side_chat_id: stream_side_chat_id,
                        run_generation,
                        result,
                    });
                })?;
        if worker_start_tx.send(()).is_err() {
            drop(worker);
            return Err("side chat worker stopped before admission".to_string());
        }
        let (run_generation, phase, start_result) = match admission_ack_rx
            .recv_timeout(admission_ack_timeout)
        {
            Ok(Ok((run_generation, context))) if run_generation == provisional_generation => {
                self.side_chat_contexts.insert(owner_session_id, context);
                (run_generation, "request admitted".to_string(), Ok(()))
            }
            Ok(Ok((run_generation, _))) => {
                cancel.cancel();
                (
                    run_generation,
                    "stopping after an unexpected request generation".to_string(),
                    Err(format!(
                        "side chat admission returned generation {run_generation}, expected {provisional_generation}"
                    )),
                )
            }
            Ok(Err(error)) => {
                drop(worker);
                return Err(error);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                cancel.cancel();
                (
                    provisional_generation,
                    "start acknowledgement timed out; stop requested".to_string(),
                    Err(format!(
                        "side chat admission did not acknowledge within {}ms",
                        admission_ack_timeout.as_millis()
                    )),
                )
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                drop(worker);
                return Err("side chat worker stopped before acknowledging admission".to_string());
            }
        };
        self.side_chat_runs.insert(
            owner_session_id,
            DesktopSideChatRun {
                side_chat_id: side_chat_id_text,
                run_generation,
                cancel,
                worker,
                phase,
                streamed_text: String::new(),
                delete_after_finish: false,
            },
        );
        self.side_chat_errors.remove(&owner_session_id);
        start_result
    }

    pub(crate) fn cancel_side_chat(
        &mut self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
    ) -> Result<(), String> {
        self.ensure_current_side_chat_owner(owner_session_id)?;
        self.validate_side_chat_target(owner_session_id, side_chat_id, expected_generation)?;
        let run = self
            .side_chat_runs
            .get_mut(&owner_session_id)
            .ok_or_else(|| "the side chat no longer has an active request".to_string())?;
        if run.side_chat_id != side_chat_id.to_string() {
            return Err("the side chat request owner changed".to_string());
        }
        run.phase = "stop requested".to_string();
        run.cancel.cancel();
        self.side_chat_errors.remove(&owner_session_id);
        Ok(())
    }

    pub(crate) fn delete_side_chat(
        &mut self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
    ) -> Result<(), String> {
        self.ensure_current_side_chat_owner(owner_session_id)?;
        self.validate_side_chat_target(owner_session_id, side_chat_id, expected_generation)?;
        let repository = self.app.store.side_chat_repo();
        repository
            .request_delete(owner_session_id, side_chat_id, expected_generation)
            .map_err(|error| error.to_string())?;
        if let Some(run) = self.side_chat_runs.get_mut(&owner_session_id) {
            if run.side_chat_id != side_chat_id.to_string() {
                return Err("the side chat request owner changed".to_string());
            }
            run.delete_after_finish = true;
            run.phase = "stopping before deletion".to_string();
            run.cancel.cancel();
            self.side_chat_errors.remove(&owner_session_id);
            return Ok(());
        }
        repository
            .finalize_pending_deletions()
            .map_err(|error| error.to_string())?;
        self.side_chat_contexts.remove(&owner_session_id);
        self.side_chat_errors.remove(&owner_session_id);
        Ok(())
    }

    pub(crate) fn set_side_chat_command_error(
        &mut self,
        owner_session_id: SessionId,
        message: impl Into<String>,
    ) {
        self.side_chat_errors
            .insert(owner_session_id, message.into());
    }

    fn ensure_current_side_chat_owner(&self, owner_session_id: SessionId) -> Result<(), String> {
        if self.state.app_state.current_session_id != Some(owner_session_id) {
            return Err(
                "the selected main session changed before the side chat action".to_string(),
            );
        }
        Ok(())
    }

    fn validate_side_chat_target(
        &self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
    ) -> Result<(), String> {
        let binding = self
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "the selected main session has no side chat".to_string())?;
        if binding.id != side_chat_id || binding.request_generation != expected_generation {
            return Err("the side chat target changed before the action".to_string());
        }
        Ok(())
    }
}
