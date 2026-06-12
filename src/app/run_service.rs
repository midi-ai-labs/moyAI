use std::collections::BTreeMap;
use std::fs;

use base64::Engine as _;
use camino::{Utf8Path, Utf8PathBuf};

use crate::agent::{AgentLoop, AgentRunRequest, RuntimeInputView};
use crate::app::session_title::{generate_session_title, is_placeholder_session_title};
use crate::app::{
    AppCommand, ReviewRequest, RunRequest, SessionArchiveRequest, SessionCompactRequest,
    SessionEventsRequest, SessionForkRequest, SessionHistoryRequest, SessionIdleAdmissionRequest,
    SessionInterruptRequest, SessionListRequest, SessionLoadedRequest, SessionMemoryRequest,
    SessionReadRequest, SessionRejoinRequest, SessionRollbackRequest, SessionSearchRequest,
    SessionSettingsUpdateRequest, SessionShowRequest, SessionSteerRequest,
    SessionTitleUpdateRequest, SessionTurnsRequest,
};
use crate::cli::{ConfirmationPrompt, EventRenderer};
use crate::config::model::PartialResolvedConfig;
use crate::config::{ModelConfig, ResolvedConfig, merge::apply_patch as apply_config_patch};
use crate::error::{AppRunError, RuntimeError};
use crate::harness::{HarnessRecordingSink, NativeHarnessRecorder};
use crate::llm::{
    ConfigModelCatalog, ModelCatalog, apply_model_availability_report_to_config,
    check_model_availability,
};
use crate::protocol::{
    ActiveWorkContractProjection, AdditionalContextEntry, AdditionalContextKind,
    ModelCapabilities as ProtocolModelCapabilities, OutputContract, ProjectionId,
    ProtocolEventStore, ProtocolRecordingSink, SandboxProfile, SteerTurn, ThreadOp, ToolChoice,
    TurnContext, UserInputItem, UserTurn,
};
use crate::runtime::{RunEventSink, SessionRuntimeEventHub};
use crate::session::{
    DispatchTransformKind, ImagePart, PromptDispatchPart, RunSummary, SessionModelParameters,
    SessionRecord, SessionRepository, SessionSelector, SessionSettingsPatch, SessionStartRequest,
    SessionStateSnapshot, SessionStatus, TaskRoute,
};
use crate::storage::StoreBundle;
use crate::workspace::{branch_review_scope, uncommitted_review_scope};

const MAX_IMAGE_ATTACHMENTS_PER_TURN: usize = 8;
const MAX_IMAGE_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;

#[derive(Clone)]
pub struct RunService {
    store: StoreBundle,
    config: crate::config::ResolvedConfig,
    workspace: crate::workspace::Workspace,
    session_service: crate::session::SessionService,
    agent_loop: AgentLoop,
    session_event_hub: SessionRuntimeEventHub,
}

impl RunService {
    pub fn new(
        store: StoreBundle,
        config: crate::config::ResolvedConfig,
        workspace: crate::workspace::Workspace,
        session_service: crate::session::SessionService,
        agent_loop: AgentLoop,
        session_event_hub: SessionRuntimeEventHub,
    ) -> Self {
        Self {
            store,
            config,
            workspace,
            session_service,
            agent_loop,
            session_event_hub,
        }
    }

    pub async fn execute(
        &self,
        command: AppCommand,
        renderer: &mut dyn EventRenderer,
        prompt: &mut dyn ConfirmationPrompt,
    ) -> Result<RunSummary, AppRunError> {
        match command {
            AppCommand::Run(request) => self.execute_run(request, renderer, prompt).await,
            AppCommand::SessionArchive(request) => {
                self.execute_session_archive(request, renderer).await
            }
            AppCommand::SessionList(request) => self.execute_session_list(request, renderer).await,
            AppCommand::SessionLoaded(request) => {
                self.execute_session_loaded(request, renderer).await
            }
            AppCommand::SessionSearch(request) => {
                self.execute_session_search(request, renderer).await
            }
            AppCommand::SessionSettingsUpdate(request) => {
                self.execute_session_settings_update(request, renderer)
                    .await
            }
            AppCommand::SessionTitleUpdate(request) => {
                self.execute_session_title_update(request, renderer).await
            }
            AppCommand::SessionInterrupt(request) => {
                self.execute_session_interrupt(request, renderer).await
            }
            AppCommand::SessionCompact(request) => {
                self.execute_session_compact(request, renderer).await
            }
            AppCommand::SessionMemory(request) => {
                self.execute_session_memory(request, renderer).await
            }
            AppCommand::SessionIdleAdmission(request) => {
                self.execute_session_idle_admission(request, renderer).await
            }
            AppCommand::SessionShow(request) => self.execute_session_show(request, renderer).await,
            AppCommand::SessionHistory(request) => {
                self.execute_session_history(request, renderer).await
            }
            AppCommand::SessionRead(request) => self.execute_session_read(request, renderer).await,
            AppCommand::SessionRejoin(request) => {
                self.execute_session_rejoin(request, renderer).await
            }
            AppCommand::SessionRollback(request) => {
                self.execute_session_rollback(request, renderer).await
            }
            AppCommand::SessionFork(request) => self.execute_session_fork(request, renderer).await,
            AppCommand::SessionTurns(request) => {
                self.execute_session_turns(request, renderer).await
            }
            AppCommand::SessionEvents(request) => {
                self.execute_session_events(request, renderer).await
            }
            AppCommand::SessionSteer(request) => {
                self.execute_session_steer(request, renderer).await
            }
        }
    }

    async fn runtime_input_view(
        &self,
        session_id: crate::session::SessionId,
    ) -> Result<RuntimeInputView, AppRunError> {
        let history_items = self
            .store
            .protocol_event_store()
            .list_history_items_for_session(session_id)?;
        let runtime_input = RuntimeInputView::from_history_items(history_items);
        if !runtime_input.has_user_turn() {
            return Err(AppRunError::Message(
                "cannot build runtime input without a canonical protocol user turn".to_string(),
            ));
        }
        Ok(runtime_input)
    }

    async fn execute_run(
        &self,
        request: RunRequest,
        renderer: &mut dyn EventRenderer,
        prompt: &mut dyn ConfirmationPrompt,
    ) -> Result<RunSummary, AppRunError> {
        let selector = match (request.session_id, request.continue_last) {
            (Some(id), false) => SessionSelector::ById(id),
            (None, true) => SessionSelector::Latest,
            (None, false) => SessionSelector::New,
            (Some(_), true) => {
                return Err(AppRunError::Message(
                    "`--session` and `--continue-last` cannot be combined".to_string(),
                ));
            }
        };
        let session_settings = self.session_settings_for_selector(&selector).await?;
        let mut effective_config = compose_run_effective_config(
            self.config.clone(),
            session_settings.as_ref(),
            request.config_override.clone(),
            &request.model,
            &request.base_url,
        );
        let should_generate_session_title = matches!(&selector, SessionSelector::New)
            && request
                .title
                .as_deref()
                .map(is_placeholder_session_title)
                .unwrap_or(false)
            && !request.prompt.trim().is_empty();
        let image_parts = load_image_attachments(&request.cwd, &request.image_paths)?;
        hydrate_configured_model_from_provider(&mut effective_config, !image_parts.is_empty())
            .await?;
        let model = ConfigModelCatalog::new(effective_config.clone()).resolve(None)?;
        if !image_parts.is_empty() && !effective_config.model.supports_images {
            return Err(AppRunError::Message(format!(
                "configured model `{}` does not advertise image support; choose a vision-capable model before sending images",
                effective_config.model.model
            )));
        }
        let prepared = prepare_run_turn(&self.workspace, &request)?;
        if let Some(session_id) = request.session_id
            && request.review_request.is_none()
            && !prepared.prompt.trim().is_empty()
        {
            let existing = self.store.session_repo().get_session(session_id).await?;
            if existing.status == SessionStatus::Running {
                return self
                    .store_active_turn_steer_from_parts(
                        SessionSteerRequest {
                            session_id,
                            prompt: prepared.prompt.clone(),
                            cwd: request.cwd.clone(),
                            image_paths: request.image_paths.clone(),
                            client_user_message_id: None,
                        },
                        image_parts,
                        Some("run --session against active session".to_string()),
                        renderer,
                    )
                    .await;
            }
        }

        let session_context = self
            .session_service
            .start_or_resume(
                SessionStartRequest {
                    selector,
                    title: request.title.clone(),
                    cwd: request.cwd.clone(),
                    model: effective_config.model.model.clone(),
                    base_url: effective_config.model.base_url.clone(),
                    access_mode: effective_config.permissions.access_mode,
                },
                self.workspace.clone(),
            )
            .await?;
        let mut renderer_sink = RendererSink {
            renderer,
            show_reasoning: request.show_reasoning,
        };
        let recorder = NativeHarnessRecorder::start_harness_only(
            &self.store,
            Some(session_context.session.id),
            self.workspace.root.clone(),
        )?;
        let protocol_turn_id = recorder.protocol_turn_id();
        let mut harness_sink = HarnessRecordingSink::new(recorder, &mut renderer_sink);
        let mut sink = ProtocolRecordingSink::new(
            self.store.protocol_event_store(),
            Some(session_context.session.id),
            protocol_turn_id,
            &mut harness_sink,
        )
        .with_runtime_event_publisher(self.session_event_hub.publisher());
        sink.emit(crate::session::RunEvent::SessionStarted {
            session_id: session_context.session.id,
            title: session_context.session.title.clone(),
        })?;

        let user_message_id = if prepared.prompt.trim().is_empty() {
            let runtime_input = self.runtime_input_view(session_context.session.id).await?;
            latest_user_message_id_from_history_items(&runtime_input.history_items).ok_or_else(
                || {
                    AppRunError::Message(
                        "cannot resume a session without a prompt or prior user message"
                            .to_string(),
                    )
                },
            )?
        } else {
            let thread_op = build_user_thread_op(
                protocol_turn_id,
                &session_context,
                &effective_config,
                &prepared,
                &image_parts,
                request.editor_context.clone(),
            );
            let ThreadOp::UserTurn(user_turn) = &thread_op else {
                return Err(AppRunError::Message(
                    "run submission did not produce a user turn".to_string(),
                ));
            };
            if !user_turn.is_dispatchable() {
                return Err(AppRunError::Message(format!(
                    "configured model `{}` cannot dispatch this user turn",
                    effective_config.model.model
                )));
            }
            let user_message = self
                .session_service
                .store_user_thread_op_with_protocol_bundle(
                    &session_context,
                    user_turn,
                    Some(effective_config.model.model.clone()),
                    prepared.initial_state.clone(),
                    protocol_turn_id,
                    sink.reserve_sequence_no(),
                )
                .await?;
            sink.emit_pre_recorded(crate::session::RunEvent::UserTurnStored {
                session_id: session_context.session.id,
                message_id: user_message.id,
                turn: Box::new(user_turn.clone()),
            })?;
            sink.emit(crate::session::RunEvent::UserMessageStored {
                message_id: user_message.id,
            })?;
            if should_generate_session_title {
                if let Ok(title) = generate_session_title(
                    &effective_config,
                    &prepared.prompt,
                    request.cancel.clone(),
                )
                .await
                {
                    if !is_placeholder_session_title(&title) {
                        let title_event = crate::session::RunEvent::SessionTitleUpdated {
                            session_id: session_context.session.id,
                            title: title.clone(),
                        };
                        if self
                            .store
                            .session_repo()
                            .update_session_title_with_protocol_event(
                                session_context.session.id,
                                &title,
                                &title_event,
                                protocol_turn_id,
                                Some(sink.reserve_sequence_no()),
                            )
                            .await
                            .is_ok()
                        {
                            sink.emit_pre_recorded(title_event)?;
                        }
                    }
                }
            }
            user_message.id
        };

        let runtime_input = self.runtime_input_view(session_context.session.id).await?;
        let state = self
            .session_service
            .load_state(session_context.session.id)
            .await?;
        let session_id = session_context.session.id;
        let summary = match self
            .agent_loop
            .run(
                AgentRunRequest {
                    session: session_context,
                    user_message_id,
                    protocol_turn_id,
                    runtime_input,
                    state,
                    config: effective_config,
                    model,
                    cancel: request.cancel.clone(),
                },
                prompt,
                &mut sink,
            )
            .await
        {
            Ok(summary) => summary,
            Err(error) => {
                let current = self.store.session_repo().get_session(session_id).await?;
                if current.status == SessionStatus::Running {
                    if request.cancel.is_cancelled() {
                        let event = crate::session::RunEvent::SessionInterrupted {
                            session_id,
                            reason: "run cancelled by user".to_string(),
                        };
                        self.store
                            .session_repo()
                            .set_status_with_protocol_event(
                                session_id,
                                SessionStatus::Cancelled,
                                &event,
                                protocol_turn_id,
                                Some(sink.reserve_sequence_no()),
                            )
                            .await?;
                        sink.emit_pre_recorded(event)?;
                    } else {
                        let event = crate::session::RunEvent::SessionFailed {
                            session_id,
                            message: error.to_string(),
                        };
                        self.store
                            .session_repo()
                            .set_status_with_protocol_event(
                                session_id,
                                SessionStatus::Failed,
                                &event,
                                protocol_turn_id,
                                Some(sink.reserve_sequence_no()),
                            )
                            .await?;
                        sink.emit_pre_recorded(event)?;
                    }
                }
                return Err(error.into());
            }
        };
        drop(sink);
        renderer.finish(&summary)?;
        Ok(summary)
    }

    async fn session_settings_for_selector(
        &self,
        selector: &SessionSelector,
    ) -> Result<Option<SessionRecord>, AppRunError> {
        match selector {
            SessionSelector::New => Ok(None),
            SessionSelector::ById(session_id) => Ok(Some(
                self.store.session_repo().get_session(*session_id).await?,
            )),
            SessionSelector::Latest => Ok(self
                .store
                .session_repo()
                .latest_session(self.workspace.project_id)
                .await?),
        }
    }

    async fn execute_session_list(
        &self,
        request: SessionListRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let sessions = self
            .store
            .session_repo()
            .list_sessions(request.project_id, request.limit)
            .await?;
        renderer.render_session_list(&sessions)?;
        Ok(RunSummary {
            session_id: crate::session::SessionId::new(),
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_loaded(
        &self,
        request: SessionLoadedRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let loaded = self
            .session_service
            .loaded_sessions(request.project_id, request.limit, request.include_archived)
            .await?;
        renderer.render_loaded_sessions(&loaded)?;
        Ok(RunSummary {
            session_id: crate::session::SessionId::new(),
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_search(
        &self,
        request: SessionSearchRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let sessions = self
            .session_service
            .search_sessions(
                request.project_id,
                &request.query,
                request.limit,
                request.include_archived,
            )
            .await?;
        renderer.render_session_list(&sessions)?;
        Ok(RunSummary {
            session_id: crate::session::SessionId::new(),
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_archive(
        &self,
        request: SessionArchiveRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let session = self
            .session_service
            .set_session_archived(request.session_id, request.archived)
            .await?;
        renderer.render_session_list(std::slice::from_ref(&session))?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_interrupt(
        &self,
        request: SessionInterruptRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let session = self
            .session_service
            .interrupt_running_session(request.session_id, request.reason)
            .await?;
        renderer.render_session_list(std::slice::from_ref(&session))?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_compact(
        &self,
        request: SessionCompactRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let result = self
            .session_service
            .compact_session(request.session_id, request.keep_recent)
            .await?;
        renderer.render_session_compact_result(&result)?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_memory(
        &self,
        request: SessionMemoryRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let update = self
            .session_service
            .update_session_memory_mode(request.session_id, request.mode)
            .await?;
        renderer.render_session_memory_mode_update(&update)?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_idle_admission(
        &self,
        request: SessionIdleAdmissionRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let admission = self
            .session_service
            .evaluate_idle_turn_admission(
                request.session_id,
                request.pending_trigger_turn,
                request.plan_mode,
            )
            .await?;
        renderer.render_session_idle_turn_admission(&admission)?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_settings_update(
        &self,
        request: SessionSettingsUpdateRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let update = self
            .session_service
            .update_session_settings(
                request.session_id,
                SessionSettingsPatch {
                    cwd: request.cwd,
                    model: request.model,
                    base_url: request.base_url,
                    access_mode: request.access_mode,
                    reset_model_parameters: request.reset_model_parameters,
                    temperature: request.temperature,
                    top_p: request.top_p,
                    top_k: request.top_k,
                    max_output_tokens: request.max_output_tokens,
                },
            )
            .await?;
        renderer.render_session_list(std::slice::from_ref(&update.session))?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_title_update(
        &self,
        request: SessionTitleUpdateRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let update = self
            .session_service
            .update_session_title(request.session_id, request.title)
            .await?;
        renderer.render_session_list(std::slice::from_ref(&update.session))?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_show(
        &self,
        request: SessionShowRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let session = self
            .store
            .session_repo()
            .get_session(request.session_id)
            .await?;
        let history_items = self
            .store
            .protocol_event_store()
            .list_history_items_for_session(request.session_id)?;
        if history_items.is_empty() {
            return Err(AppRunError::Message(
                "cannot show session because canonical protocol history is empty".to_string(),
            ));
        }
        renderer.render_session_history_items(&session, &history_items, request.show_reasoning)?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_history(
        &self,
        request: SessionHistoryRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let page = self
            .session_service
            .canonical_history_page(request.session_id, request.offset, request.limit)
            .await?;
        renderer.render_session_history_page(&page)?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_turns(
        &self,
        request: SessionTurnsRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let page = self
            .session_service
            .canonical_turn_page(request.session_id, request.offset, request.limit)
            .await?;
        renderer.render_session_turn_page(&page)?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_events(
        &self,
        request: SessionEventsRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let page = self
            .session_service
            .canonical_runtime_event_page(request.session_id, request.offset, request.limit)
            .await?;
        renderer.render_session_runtime_event_page(&page)?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_read(
        &self,
        request: SessionReadRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let read = self
            .session_service
            .canonical_session_read(
                request.session_id,
                request.history_offset,
                request.history_limit,
                request.turn_offset,
                request.turn_limit,
            )
            .await?;
        renderer.render_session_read(&read)?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_rejoin(
        &self,
        request: SessionRejoinRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let rejoin = self
            .session_service
            .rejoin_running_session(
                request.session_id,
                request.history_offset,
                request.history_limit,
                request.turn_offset,
                request.turn_limit,
            )
            .await?;
        renderer.render_session_rejoin(&rejoin)?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Running,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_rollback(
        &self,
        request: SessionRollbackRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        self.session_service
            .rollback_session(request.session_id, request.num_turns)
            .await?;
        let read = self
            .session_service
            .canonical_session_read(
                request.session_id,
                request.history_offset,
                request.history_limit,
                request.turn_offset,
                request.turn_limit,
            )
            .await?;
        renderer.render_session_read(&read)?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_fork(
        &self,
        request: SessionForkRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let fork = self
            .session_service
            .fork_session(request.source_session_id, request.title)
            .await?;
        let read = self
            .session_service
            .canonical_session_read(
                fork.forked_session.id,
                request.history_offset,
                request.history_limit,
                request.turn_offset,
                request.turn_limit,
            )
            .await?;
        renderer.render_session_read(&read)?;
        Ok(RunSummary {
            session_id: fork.forked_session.id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }

    async fn execute_session_steer(
        &self,
        request: SessionSteerRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        if request.prompt.trim().is_empty() {
            return Err(AppRunError::Message(
                "active-turn steer prompt must not be empty".to_string(),
            ));
        }
        let image_parts = load_image_attachments(&request.cwd, &request.image_paths)?;
        self.store_active_turn_steer_from_parts(request, image_parts, None, renderer)
            .await
    }

    async fn store_active_turn_steer_from_parts(
        &self,
        request: SessionSteerRequest,
        image_parts: Vec<ImagePart>,
        source_label: Option<String>,
        _renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        let (active_turn_id, _next_sequence_no) = self
            .store
            .protocol_event_store()
            .latest_turn_position_for_session(request.session_id)?
            .ok_or_else(|| {
                AppRunError::Message(format!(
                    "session {} has no active turn to steer",
                    request.session_id
                ))
            })?;
        let mut items = Vec::new();
        if !request.prompt.trim().is_empty() {
            items.push(UserInputItem::Text {
                text: request.prompt.clone(),
            });
        }
        items.extend(
            image_parts
                .into_iter()
                .map(|image| UserInputItem::Image { image }),
        );
        let mut additional_context = BTreeMap::new();
        additional_context.insert(
            "moyai.surface".to_string(),
            AdditionalContextEntry {
                value: source_label.unwrap_or_else(|| "session steer".to_string()),
                kind: AdditionalContextKind::Application,
            },
        );
        additional_context.insert(
            "moyai.cwd".to_string(),
            AdditionalContextEntry {
                value: request.cwd.to_string(),
                kind: AdditionalContextKind::Application,
            },
        );
        self.session_service
            .store_active_turn_steer(
                request.session_id,
                &SteerTurn {
                    expected_turn_id: active_turn_id,
                    items,
                    additional_context,
                    client_user_message_id: request.client_user_message_id.clone(),
                },
            )
            .await?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Running,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
        })
    }
}

fn latest_user_message_id_from_history_items(
    history_items: &[crate::protocol::HistoryItem],
) -> Option<crate::session::MessageId> {
    let mut ordered = history_items.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|item| (item.sequence_no, item.created_at_ms));
    ordered.iter().rev().find_map(|item| match &item.payload {
        crate::protocol::HistoryItemPayload::UserTurn {
            message_id: Some(message_id),
            ..
        } => Some(*message_id),
        _ => None,
    })
}

fn apply_session_model_parameters(model: &mut ModelConfig, parameters: &SessionModelParameters) {
    if let Some(value) = parameters.temperature {
        model.temperature = Some(value);
    }
    if let Some(value) = parameters.top_p {
        model.top_p = Some(value);
    }
    if let Some(value) = parameters.top_k {
        model.top_k = Some(value);
    }
    if let Some(value) = parameters.max_output_tokens {
        model.max_output_tokens = value;
    }
}

fn compose_run_effective_config(
    base_config: ResolvedConfig,
    session_settings: Option<&SessionRecord>,
    config_override: Option<PartialResolvedConfig>,
    request_model: &str,
    request_base_url: &str,
) -> ResolvedConfig {
    let mut effective_config = base_config;
    if let Some(session_settings) = session_settings {
        effective_config.model.model = session_settings.model.clone();
        effective_config.model.base_url = session_settings.base_url.clone();
        apply_session_model_parameters(
            &mut effective_config.model,
            &session_settings.model_parameters,
        );
        effective_config.permissions.access_mode = session_settings.access_mode;
    }
    if let Some(patch) = config_override {
        effective_config = apply_config_patch(effective_config, patch);
    }
    if !request_base_url.trim().is_empty() {
        effective_config.model.base_url = request_base_url.to_string();
    }
    if !request_model.trim().is_empty() {
        effective_config.model.model = request_model.to_string();
    }
    effective_config
}

#[cfg(test)]
fn app_session_model_parameters_override_runtime_config_fixture_passes() -> bool {
    let mut model = ModelConfig {
        temperature: Some(1.0),
        top_p: Some(1.0),
        top_k: Some(8),
        max_output_tokens: 1024,
        ..crate::config::ResolvedConfig::default().model
    };
    apply_session_model_parameters(
        &mut model,
        &SessionModelParameters {
            temperature: Some(0.2),
            top_p: Some(0.8),
            top_k: Some(40),
            max_output_tokens: Some(4096),
        },
    );
    model.temperature == Some(0.2)
        && model.top_p == Some(0.8)
        && model.top_k == Some(40)
        && model.max_output_tokens == 4096
}

#[cfg(test)]
fn app_config_override_wins_over_session_settings_fixture_passes() -> bool {
    let mut base = ResolvedConfig::default();
    base.model.model = "base-model".to_string();
    base.model.base_url = "http://base:1234".to_string();
    base.model.max_output_tokens = 1024;
    base.permissions.access_mode = crate::config::AccessMode::Default;

    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "Session".to_string(),
        status: SessionStatus::Completed,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: "session-model".to_string(),
        base_url: "http://session:1234".to_string(),
        access_mode: crate::config::AccessMode::FullAccess,
        model_parameters: SessionModelParameters {
            temperature: Some(0.2),
            top_p: Some(0.8),
            top_k: Some(40),
            max_output_tokens: Some(2048),
        },
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: Some(1),
    };
    let override_config = PartialResolvedConfig {
        model: Some(crate::config::model::PartialModelConfig {
            model: Some("override-model".to_string()),
            base_url: Some("http://override:1234".to_string()),
            temperature: Some(0.7),
            max_output_tokens: Some(4096),
            ..crate::config::model::PartialModelConfig::default()
        }),
        permissions: Some(crate::config::model::PartialPermissionsConfig {
            access_mode: Some(crate::config::AccessMode::AutoReview),
            ..crate::config::model::PartialPermissionsConfig::default()
        }),
        ..PartialResolvedConfig::default()
    };

    let effective =
        compose_run_effective_config(base, Some(&session), Some(override_config), "", "");
    effective.model.model == "override-model"
        && effective.model.base_url == "http://override:1234"
        && effective.model.temperature == Some(0.7)
        && effective.model.max_output_tokens == 4096
        && effective.permissions.access_mode == crate::config::AccessMode::AutoReview
}

#[cfg(test)]
fn app_session_settings_remain_when_request_override_is_empty_fixture_passes() -> bool {
    let mut base = ResolvedConfig::default();
    base.model.model = "base-model".to_string();
    base.model.base_url = "http://base:1234".to_string();

    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "Session".to_string(),
        status: SessionStatus::Completed,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: "session-model".to_string(),
        base_url: "http://session:1234".to_string(),
        access_mode: crate::config::AccessMode::FullAccess,
        model_parameters: SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: Some(1),
    };

    let effective = compose_run_effective_config(base, Some(&session), None, "", "");
    effective.model.model == "session-model"
        && effective.model.base_url == "http://session:1234"
        && effective.permissions.access_mode == crate::config::AccessMode::FullAccess
}

#[cfg(test)]
fn app_per_run_model_and_base_url_win_over_config_override_fixture_passes() -> bool {
    let override_config = PartialResolvedConfig {
        model: Some(crate::config::model::PartialModelConfig {
            model: Some("override-model".to_string()),
            base_url: Some("http://override:1234".to_string()),
            ..crate::config::model::PartialModelConfig::default()
        }),
        ..PartialResolvedConfig::default()
    };
    let effective = compose_run_effective_config(
        ResolvedConfig::default(),
        None,
        Some(override_config),
        "request-model",
        "http://request:1234",
    );
    effective.model.model == "request-model" && effective.model.base_url == "http://request:1234"
}

fn build_user_thread_op(
    turn_id: crate::protocol::TurnId,
    session_context: &crate::session::SessionContext,
    config: &crate::config::ResolvedConfig,
    prepared: &PreparedRunTurn,
    images: &[ImagePart],
    editor_context: Option<crate::session::EditorContext>,
) -> ThreadOp {
    let mut items = Vec::new();
    if !prepared.prompt.is_empty() {
        items.push(UserInputItem::Text {
            text: prepared.prompt.clone(),
        });
    }
    items.extend(
        images
            .iter()
            .cloned()
            .map(|image| UserInputItem::Image { image }),
    );
    ThreadOp::user_turn(UserTurn {
        turn_id,
        items,
        prompt_dispatch: prepared.prompt_dispatch.clone(),
        editor_context,
        context: build_initial_turn_context(
            session_context,
            config,
            &prepared.initial_state,
            images,
        ),
    })
}

async fn hydrate_configured_model_from_provider(
    config: &mut crate::config::ResolvedConfig,
    require_vision: bool,
) -> Result<(), AppRunError> {
    let configured_model = config.model.model.trim().to_string();
    if configured_model.is_empty() {
        return Err(AppRunError::Message(
            "configured model is empty".to_string(),
        ));
    }

    let report = check_model_availability(config, None, None, require_vision).await;
    apply_model_availability_report_to_config(&mut config.model, &report)
        .map_err(|error| AppRunError::Message(error.to_string()))?;
    Ok(())
}

fn load_image_attachments(
    cwd: &Utf8Path,
    image_paths: &[Utf8PathBuf],
) -> Result<Vec<ImagePart>, AppRunError> {
    if image_paths.len() > MAX_IMAGE_ATTACHMENTS_PER_TURN {
        return Err(AppRunError::Message(format!(
            "too many image attachments: {} provided, maximum is {}",
            image_paths.len(),
            MAX_IMAGE_ATTACHMENTS_PER_TURN
        )));
    }
    let mut images = Vec::new();
    for image_path in image_paths {
        let resolved = if image_path.is_absolute() {
            image_path.clone()
        } else {
            cwd.join(image_path)
        };
        let metadata = fs::metadata(resolved.as_std_path()).map_err(|error| {
            AppRunError::Message(format!("failed to stat image `{resolved}`: {error}"))
        })?;
        if !metadata.is_file() {
            return Err(AppRunError::Message(format!(
                "image attachment `{resolved}` is not a file"
            )));
        }
        if metadata.len() > MAX_IMAGE_ATTACHMENT_BYTES {
            return Err(AppRunError::Message(format!(
                "image attachment `{resolved}` is {} bytes; maximum is {} bytes",
                metadata.len(),
                MAX_IMAGE_ATTACHMENT_BYTES
            )));
        }
        let mime_type = image_mime_type(&resolved).ok_or_else(|| {
            AppRunError::Message(format!(
                "unsupported image attachment extension for `{resolved}`; supported: png, jpg, jpeg, webp, gif"
            ))
        })?;
        let bytes = fs::read(resolved.as_std_path()).map_err(|error| {
            AppRunError::Message(format!("failed to read image `{resolved}`: {error}"))
        })?;
        images.push(ImagePart {
            source_path: Some(resolved),
            mime_type: mime_type.to_string(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            byte_len: metadata.len(),
        });
    }
    Ok(images)
}

fn image_mime_type(path: &Utf8Path) -> Option<&'static str> {
    match path.extension().map(|value| value.to_ascii_lowercase()) {
        Some(value) if value == "png" => Some("image/png"),
        Some(value) if value == "jpg" || value == "jpeg" => Some("image/jpeg"),
        Some(value) if value == "webp" => Some("image/webp"),
        Some(value) if value == "gif" => Some("image/gif"),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct PreparedRunTurn {
    prompt: String,
    prompt_dispatch: Option<PromptDispatchPart>,
    initial_state: SessionStateSnapshot,
}

fn prepare_run_turn(
    workspace: &crate::workspace::Workspace,
    request: &RunRequest,
) -> Result<PreparedRunTurn, AppRunError> {
    let mut prompt = request.prompt.clone();
    let mut prompt_dispatch = request
        .prompt_dispatch
        .clone()
        .unwrap_or_else(|| PromptDispatchPart::raw(&request.prompt));
    let mut initial_state = SessionStateSnapshot::default();

    if let Some(review_request) = &request.review_request {
        let review_scope = match review_request {
            ReviewRequest::Uncommitted => uncommitted_review_scope(workspace),
            ReviewRequest::Branch { base_ref } => branch_review_scope(workspace, base_ref),
        }
        .map_err(|error| AppRunError::Message(error.to_string()))?;
        prompt = build_review_prompt(request.prompt.trim(), &review_scope);
        prompt_dispatch = prompt_dispatch.with_transform(
            &prompt,
            DispatchTransformKind::ReviewEntrypoint,
            Some(review_scope.label()),
        );
        initial_state.route = TaskRoute::Summary;
        initial_state.review_scope = Some(review_scope.clone());
        initial_state.active_targets = review_scope.changed_files.clone();
    } else if let Some(expanded) = maybe_expand_workflow_command(workspace, &prompt)? {
        prompt = expanded.prompt;
        prompt_dispatch = prompt_dispatch.with_transform(
            &prompt,
            DispatchTransformKind::WorkflowCommand,
            Some(format!("/{}", expanded.name)),
        );
    }

    Ok(PreparedRunTurn {
        prompt,
        prompt_dispatch: Some(prompt_dispatch),
        initial_state,
    })
}

fn build_initial_turn_context(
    session_context: &crate::session::SessionContext,
    config: &crate::config::ResolvedConfig,
    state: &SessionStateSnapshot,
    images: &[ImagePart],
) -> TurnContext {
    let allowed_tools = Vec::new();
    TurnContext {
        session_id: session_context.session.id,
        cwd: session_context.workspace.cwd.clone(),
        workspace_root: session_context.workspace.root.clone(),
        provider: "openai_compat".to_string(),
        model: config.model.model.clone(),
        base_url: config.model.base_url.clone(),
        access_mode: config.permissions.access_mode,
        sandbox: sandbox_profile_for_access_mode(config.permissions.access_mode),
        shell_family: config.shell.family.unwrap_or_else(default_shell_family),
        model_capabilities: ProtocolModelCapabilities {
            supports_tools: config.model.supports_tools,
            supports_reasoning: config.model.supports_reasoning,
            supports_images: config.model.supports_images,
            parallel_tool_calls: crate::llm::control_plane_parallel_tool_calls_projection(
                allowed_tools.len(),
                config.model.parallel_tool_calls,
                config.model.max_parallel_predictions,
            ),
            context_window: config.model.context_window,
            max_output_tokens: config.model.max_output_tokens,
        },
        route: state.route,
        process_phase: state.process_phase,
        active_contract: ActiveWorkContractProjection {
            route: state.route,
            process_phase: state.process_phase,
            active_work_kind: Some(state.route.key().to_string()),
            summary: "Initial user turn context before reducer projection.".to_string(),
            active_targets: state.active_targets.clone(),
            operation_intents: Vec::new(),
            required_verification_commands: state.verification.required_commands.clone(),
            allowed_tools: allowed_tools.clone(),
            forbidden_tools: Vec::new(),
            projection_id: ProjectionId::new(),
        },
        allowed_tools,
        tool_choice: ToolChoice::Auto,
        images: images.to_vec(),
        output_contract: OutputContract {
            final_answer_required: true,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: state
            .implementation_handoff
            .as_ref()
            .and_then(|handoff| handoff.continuation_contract.clone()),
        turn_decision_projection: None,
    }
}

fn sandbox_profile_for_access_mode(access_mode: crate::config::AccessMode) -> SandboxProfile {
    match access_mode {
        crate::config::AccessMode::Default | crate::config::AccessMode::AutoReview => {
            SandboxProfile::WorkspaceWrite
        }
        crate::config::AccessMode::FullAccess => SandboxProfile::FullAccess,
    }
}

fn default_shell_family() -> crate::config::ShellFamily {
    if cfg!(windows) {
        crate::config::ShellFamily::PowerShell
    } else {
        crate::config::ShellFamily::Bash
    }
}

#[derive(Debug, Clone)]
struct WorkflowExpansion {
    name: String,
    prompt: String,
}

fn maybe_expand_workflow_command(
    workspace: &crate::workspace::Workspace,
    prompt: &str,
) -> Result<Option<WorkflowExpansion>, AppRunError> {
    let Some((name, args)) = parse_workflow_invocation(prompt) else {
        return Ok(None);
    };
    let path = workspace
        .root
        .join(".moyai/commands")
        .join(format!("{name}.md"));
    if !path.exists() {
        return Ok(None);
    }
    let template = fs::read_to_string(path.as_std_path()).map_err(|error| {
        AppRunError::Message(format!("failed to read workflow `{path}`: {error}"))
    })?;
    let expanded_body = if template.contains("{{args}}") {
        template.replace("{{args}}", args.as_deref().unwrap_or(""))
    } else if let Some(args) = args.as_deref().filter(|value| !value.is_empty()) {
        format!("{template}\n\nUser arguments:\n{args}")
    } else {
        template
    };
    let relative = path
        .strip_prefix(workspace.root.as_path())
        .map(|value| value.as_str().replace('\\', "/"))
        .unwrap_or_else(|_| path.as_str().replace('\\', "/"));
    Ok(Some(WorkflowExpansion {
        name: name.clone(),
        prompt: format!(
            "Reusable workflow command: /{name}\nSource: {relative}\n\nWorkflow instructions:\n{expanded_body}"
        ),
    }))
}

fn parse_workflow_invocation(prompt: &str) -> Option<(String, Option<String>)> {
    let trimmed = prompt.trim_start();
    let rest = trimmed.strip_prefix('/')?;
    let name_len = rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .count();
    if name_len == 0 {
        return None;
    }
    let name = rest[..name_len].to_string();
    let args = rest[name_len..].trim();
    Some((name, (!args.is_empty()).then(|| args.to_string())))
}

fn build_review_prompt(raw_prompt: &str, scope: &crate::session::ReviewScope) -> String {
    let mode_line = match scope.mode {
        crate::session::ReviewScopeMode::Uncommitted => {
            "Review the current uncommitted workspace changes.".to_string()
        }
        crate::session::ReviewScopeMode::Branch => format!(
            "Review the current branch diff against {}.",
            scope
                .base_ref
                .as_deref()
                .unwrap_or("the requested base ref")
        ),
    };
    let scope_files = if scope.changed_files.is_empty() {
        "- no changed files were detected by git".to_string()
    } else {
        scope
            .changed_files
            .iter()
            .map(|path| format!("- {}", path))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let extra_focus = if raw_prompt.is_empty() {
        String::new()
    } else {
        format!("\nAdditional review request:\n{raw_prompt}\n")
    };
    format!(
        "{mode_line}\nBase ref: {}\nHead ref: {}\nGit summary: {}\nChanged files:\n{scope_files}{extra_focus}\nInspect only this scope, gather evidence, and report findings first with severity, path, rationale, and impact. If no material issue is found, say so explicitly.",
        scope.base_ref.as_deref().unwrap_or("HEAD"),
        scope.head_ref.as_deref().unwrap_or("HEAD"),
        scope.summary,
    )
}

struct RendererSink<'a> {
    renderer: &'a mut dyn EventRenderer,
    show_reasoning: bool,
}

impl<'a> RunEventSink for RendererSink<'a> {
    fn emit(&mut self, event: crate::session::RunEvent) -> Result<(), RuntimeError> {
        if matches!(event, crate::session::RunEvent::ReasoningDelta { .. }) && !self.show_reasoning
        {
            return Ok(());
        }
        self.renderer
            .render(&event)
            .map_err(|error| RuntimeError::Message(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn session_model_parameters_override_runtime_config() {
        assert!(super::app_session_model_parameters_override_runtime_config_fixture_passes());
    }

    #[test]
    fn config_override_wins_over_session_settings() {
        assert!(super::app_config_override_wins_over_session_settings_fixture_passes());
    }

    #[test]
    fn session_settings_remain_when_request_override_is_empty() {
        assert!(super::app_session_settings_remain_when_request_override_is_empty_fixture_passes());
    }

    #[test]
    fn per_run_model_and_base_url_win_over_config_override() {
        assert!(super::app_per_run_model_and_base_url_win_over_config_override_fixture_passes());
    }
}
