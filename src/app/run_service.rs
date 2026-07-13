use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use base64::Engine as _;
use camino::{Utf8Path, Utf8PathBuf};
use futures_util::FutureExt;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::agent::{AgentLoop, AgentRunRequest, RuntimeInputView};
use crate::app::session_title::{derive_session_title, is_placeholder_session_title};
use crate::app::{
    AppCommand, ReviewRequest, RunRequest, SessionArchiveRequest, SessionCompactRequest,
    SessionEventsRequest, SessionForkRequest, SessionGoalClearRequest, SessionGoalGetRequest,
    SessionGoalSetRequest, SessionHistoryRequest, SessionIdleAdmissionRequest,
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
    ConfigModelCatalog, ModelAvailabilityReport, ModelCatalog,
    apply_model_availability_report_to_config, check_model_availability,
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
    SessionStateSnapshot, SessionStatus, TaskRoute, ThreadGoalClearResult, ThreadGoalGetResult,
    ThreadGoalSetResult, ThreadGoalStatus, validate_thread_goal_objective,
};
use crate::storage::{
    StoreBundle,
    session_repo::{RUN_ADMISSION_HEARTBEAT_INTERVAL_MS, RunAdmissionLeaseRenewalOutcome},
};
use crate::workspace::{branch_review_scope, uncommitted_review_scope};

const MAX_IMAGE_ATTACHMENTS_PER_TURN: usize = 8;
const MAX_IMAGE_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;
const MAX_GOAL_IDLE_CONTINUATIONS_PER_RUN: usize = 3;
const PROVIDER_PROBE_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Default)]
struct ProviderProbeCache {
    entries: Arc<Mutex<HashMap<String, CachedProbeReport>>>,
    probe_gates: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
}

struct CachedProbeReport {
    checked_at: Instant,
    report: ModelAvailabilityReport,
}

impl ProviderProbeCache {
    async fn get_or_probe<F, Fut>(&self, key: String, probe: F) -> ModelAvailabilityReport
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ModelAvailabilityReport>,
    {
        {
            let mut entries = self.entries.lock().await;
            entries.retain(|_, cached| cached.checked_at.elapsed() < PROVIDER_PROBE_CACHE_TTL);
            if let Some(cached) = entries.get(&key) {
                return cached.report.clone();
            }
        }
        let probe_gate = self.probe_gate(&key).await;
        let _probe_guard = probe_gate.lock().await;
        {
            let mut entries = self.entries.lock().await;
            entries.retain(|_, cached| cached.checked_at.elapsed() < PROVIDER_PROBE_CACHE_TTL);
            if let Some(cached) = entries.get(&key) {
                return cached.report.clone();
            }
        }
        let report = probe().await;
        let mut entries = self.entries.lock().await;
        entries.retain(|_, cached| cached.checked_at.elapsed() < PROVIDER_PROBE_CACHE_TTL);
        if let Some(cached) = entries.get(&key) {
            return cached.report.clone();
        }
        entries.insert(
            key,
            CachedProbeReport {
                checked_at: Instant::now(),
                report: report.clone(),
            },
        );
        report
    }

    async fn probe_gate(&self, key: &str) -> Arc<Mutex<()>> {
        let mut gates = self.probe_gates.lock().await;
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(key).and_then(Weak::upgrade) {
            return gate;
        }
        let gate = Arc::new(Mutex::new(()));
        gates.insert(key.to_string(), Arc::downgrade(&gate));
        gate
    }

    async fn get_or_probe_until_cancelled<F, Fut>(
        &self,
        key: String,
        cancel: &CancellationToken,
        probe: F,
    ) -> Option<ModelAvailabilityReport>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ModelAvailabilityReport>,
    {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            report = self.get_or_probe(key, probe) => Some(report),
        }
    }
}

#[derive(Clone)]
pub struct RunService {
    store: StoreBundle,
    config: crate::config::ResolvedConfig,
    workspace: crate::workspace::Workspace,
    session_service: crate::session::SessionService,
    agent_loop: AgentLoop,
    session_event_hub: SessionRuntimeEventHub,
    provider_probe_cache: ProviderProbeCache,
    agent_runtime: Arc<crate::app::AgentRuntime>,
}

impl RunService {
    pub fn new(
        store: StoreBundle,
        config: crate::config::ResolvedConfig,
        workspace: crate::workspace::Workspace,
        session_service: crate::session::SessionService,
        agent_loop: AgentLoop,
        session_event_hub: SessionRuntimeEventHub,
        agent_runtime: Arc<crate::app::AgentRuntime>,
    ) -> Self {
        Self {
            store,
            config,
            workspace,
            session_service,
            agent_loop,
            session_event_hub,
            provider_probe_cache: ProviderProbeCache::default(),
            agent_runtime,
        }
    }

    pub fn agent_activity_records(
        &self,
        root_session_id: crate::session::SessionId,
    ) -> Vec<crate::app::AgentActivityRecord> {
        self.agent_runtime.activity_records(root_session_id)
    }

    pub async fn durable_agent_activity_records(
        &self,
        root_session_id: crate::session::SessionId,
    ) -> Result<Vec<crate::app::AgentActivityRecord>, AppRunError> {
        self.agent_runtime
            .durable_activity_records(root_session_id)
            .await
            .map_err(AppRunError::Message)
    }

    pub fn cancel_agent_tree(&self, session_id: crate::session::SessionId) -> bool {
        self.agent_runtime.cancel_tree_for_session(session_id)
    }

    pub async fn wait_for_agent_tree_quiescence(
        &self,
        root_session_id: crate::session::SessionId,
    ) -> Result<(), AppRunError> {
        self.agent_runtime
            .wait_for_tree_quiescence(root_session_id)
            .await
            .map_err(AppRunError::Message)
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
            AppCommand::SessionGoalGet(request) => {
                self.execute_session_goal_get(request, renderer).await
            }
            AppCommand::SessionGoalSet(request) => {
                self.execute_session_goal_set(request, renderer).await
            }
            AppCommand::SessionGoalClear(request) => {
                self.execute_session_goal_clear(request, renderer).await
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
        let allow_idle_goal_continuation =
            allows_goal_idle_continuation_after_run(&request.prompt)?;
        let mut summary = self
            .execute_single_run(request.clone(), renderer, prompt)
            .await?;
        if !allow_idle_goal_continuation {
            return Ok(summary);
        }

        for _ in 0..MAX_GOAL_IDLE_CONTINUATIONS_PER_RUN {
            self.wait_for_agent_tree_quiescence(summary.session_id)
                .await?;
            if !self
                .should_start_idle_goal_continuation(summary.session_id, &request.cancel)
                .await?
            {
                break;
            }
            let continuation_request = RunRequest {
                prompt: String::new(),
                session_id: Some(summary.session_id),
                continue_last: false,
                title: None,
                cwd: request.cwd.clone(),
                model: request.model.clone(),
                base_url: request.base_url.clone(),
                config_override: request.config_override.clone(),
                output_mode: request.output_mode,
                show_reasoning: request.show_reasoning,
                prompt_dispatch: None,
                editor_context: None,
                review_request: None,
                image_paths: Vec::new(),
                cancel: request.cancel.clone(),
                live_config: request.live_config.clone(),
                agent_confirmation: request.agent_confirmation.clone(),
                agent_context: request.agent_context.clone(),
            };
            summary = self
                .execute_single_run(continuation_request, renderer, prompt)
                .await?;
        }

        Ok(summary)
    }

    async fn execute_single_run(
        &self,
        mut request: RunRequest,
        renderer: &mut dyn EventRenderer,
        prompt: &mut dyn ConfirmationPrompt,
    ) -> Result<RunSummary, AppRunError> {
        let slash_goal_command = parse_goal_slash_command(&request.prompt)?;
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
        if let Some(command) = slash_goal_command.clone() {
            match command {
                GoalSlashCommand::SetObjective(objective) => {
                    request.prompt = objective.clone();
                    request.prompt_dispatch = Some(PromptDispatchPart::raw(&objective));
                }
                GoalSlashCommand::Get
                | GoalSlashCommand::Clear
                | GoalSlashCommand::SetStatus(_) => {
                    let session_id = self
                        .session_id_for_goal_slash_control(&selector)
                        .await?
                        .ok_or_else(|| {
                            AppRunError::Message(
                                "Usage: /goal [<objective>|clear|pause|resume]. The session must start before you can view or change a goal."
                                    .to_string(),
                            )
                        })?;
                    return self
                        .execute_goal_slash_control(session_id, command, renderer)
                        .await;
                }
            }
        }
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
                .is_none_or(is_placeholder_session_title)
            && !request.prompt.trim().is_empty();
        let image_parts = load_image_attachments(&request.cwd, &request.image_paths)?;
        let prepared = prepare_run_turn(&self.workspace, &request)?;
        if let Some(existing) = session_settings.as_ref()
            && (self.store.active_runs().is_active(existing.id)
                || (matches!(
                    existing.status,
                    SessionStatus::Running | SessionStatus::AwaitingUser
                ) && self
                    .store
                    .session_repo()
                    .has_fresh_run_admission(existing.id)
                    .await?))
            && request.review_request.is_none()
            && !prepared.prompt.trim().is_empty()
        {
            return self
                .store_active_turn_steer_from_parts(
                    SessionSteerRequest {
                        session_id: existing.id,
                        prompt: prepared.prompt.clone(),
                        cwd: request.cwd.clone(),
                        image_paths: request.image_paths.clone(),
                        client_user_message_id: None,
                    },
                    image_parts,
                    Some("run request against active session".to_string()),
                    renderer,
                )
                .await;
        }
        self.hydrate_configured_model_from_provider(
            &mut effective_config,
            !image_parts.is_empty(),
            &request.cancel,
        )
        .await?;
        let model = ConfigModelCatalog::new(effective_config.clone()).resolve(None)?;
        if !image_parts.is_empty() && !effective_config.model.supports_images {
            return Err(AppRunError::Message(format!(
                "configured model `{}` does not advertise image support; choose a vision-capable model before sending images",
                effective_config.model.model
            )));
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
        if request.agent_context.is_none() {
            for edge in self
                .store
                .session_repo()
                .list_session_spawn_edges(session_context.session.id)
                .await?
            {
                if self.store.active_runs().is_active(edge.child_session_id)
                    || self
                        .store
                        .session_repo()
                        .has_fresh_run_admission(edge.child_session_id)
                        .await?
                {
                    return Err(AppRunError::Message(format!(
                        "session {} still has active sub-agent {}; wait for it to finish or cancel the agent tree before starting another root turn",
                        session_context.session.id, edge.agent_path
                    )));
                }
            }
        }
        let supplied_agent_context = request.agent_context.clone();
        if let Some(context) = supplied_agent_context.as_ref() {
            if context.session_id() != session_context.session.id {
                return Err(AppRunError::Message(format!(
                    "agent context session {} does not match requested session {}",
                    context.session_id(),
                    session_context.session.id
                )));
            }
        }
        let root_confirmation =
            if supplied_agent_context.is_none() && effective_config.multi_agent.enabled {
                Some(request.agent_confirmation.clone().ok_or_else(|| {
                    AppRunError::Message(
                        "multi-agent execution requires a shared permission confirmation channel"
                            .to_string(),
                    )
                })?)
            } else {
                None
            };
        let process_run_lease = self
            .store
            .try_acquire_run_process_lease(session_context.session.id)?;
        let Some(admission_id) = self
            .store
            .session_repo()
            .admit_session_run(session_context.session.id)
            .await?
        else {
            let current = self
                .store
                .session_repo()
                .get_session(session_context.session.id)
                .await?;
            return Err(AppRunError::Message(format!(
                "session {} could not start because its current status is {}; only one run may be admitted per session",
                current.id,
                current.status.key()
            )));
        };
        let session_id = session_context.session.id;
        let protocol_turn_id = crate::protocol::TurnId::new();
        let mut root_agent_execution = None;
        let agent_context = if let Some(context) = supplied_agent_context {
            Some(context)
        } else if let Some(confirmation) = root_confirmation {
            let execution = match self.agent_runtime.begin_root(
                &session_context,
                effective_config.clone(),
                confirmation,
                request.live_config.clone(),
                request.cancel.clone(),
            ) {
                Ok(execution) => execution,
                Err(error) => {
                    let result = finish_admitted_run(
                        &self.store,
                        session_id,
                        &admission_id,
                        protocol_turn_id,
                        request.cancel.is_cancelled(),
                        Err(AppRunError::Message(error)),
                        Ok(()),
                    )
                    .await;
                    drop(process_run_lease);
                    return result;
                }
            };
            request.cancel = execution.cancel_token();
            let context = execution.context.clone();
            root_agent_execution = Some(execution);
            Some(context)
        } else {
            None
        };
        let heartbeat_stop = CancellationToken::new();
        let heartbeat_repo = self.store.session_repo();
        let heartbeat_admission_id = admission_id.clone();
        let heartbeat_run_cancel = request.cancel.clone();
        let heartbeat_agent_context = agent_context.clone();
        let heartbeat_failure_cancel = agent_context
            .as_ref()
            .map(crate::app::AgentRunContext::tree_cancel_token)
            .unwrap_or_else(|| request.cancel.clone());
        let heartbeat_task = spawn_run_admission_heartbeat(
            session_id,
            admission_id.clone(),
            request.cancel.clone(),
            heartbeat_failure_cancel,
            heartbeat_stop.clone(),
            Duration::from_millis(RUN_ADMISSION_HEARTBEAT_INTERVAL_MS),
            move || {
                let repo = heartbeat_repo.clone();
                let admission_id = heartbeat_admission_id.clone();
                let run_cancel = heartbeat_run_cancel.clone();
                let agent_context = heartbeat_agent_context.clone();
                async move {
                    renew_admitted_run_lease_with_terminal_cancel(
                        repo,
                        session_id,
                        admission_id,
                        protocol_turn_id,
                        run_cancel,
                        agent_context,
                    )
                    .await
                }
            },
        );
        let admitted_result: Result<RunSummary, AppRunError> = async {
            let mut active_run = self
                .store
                .active_runs()
                .try_start(session_id, request.cancel.clone())?;
            if let Some(GoalSlashCommand::SetObjective(objective)) = slash_goal_command {
                self.set_goal_from_slash(session_id, &objective, renderer)
                    .await?;
            }
            let mut renderer_sink = RendererSink {
                renderer,
                show_reasoning: request.show_reasoning,
            };
            let recorder = NativeHarnessRecorder::start_harness_only_for_turn(
                &self.store,
                Some(session_id),
                self.workspace.root.clone(),
                protocol_turn_id,
            )?;
            active_run.set_turn_id(protocol_turn_id)?;
            let mut harness_sink = HarnessRecordingSink::new(recorder, &mut renderer_sink);
            let mut sink = ProtocolRecordingSink::new(
                self.store.protocol_event_store(),
                Some(session_id),
                protocol_turn_id,
                &mut harness_sink,
            )
            .with_admission_id(admission_id.clone())
            .with_runtime_event_publisher(self.session_event_hub.publisher());
            sink.emit(crate::session::RunEvent::SessionStarted {
                session_id,
                title: session_context.session.title.clone(),
            })?;

            let user_message_id = if prepared.prompt.trim().is_empty() {
                let runtime_input = self.runtime_input_view(session_id).await?;
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
                        &admission_id,
                        user_turn,
                        Some(effective_config.model.model.clone()),
                        prepared.initial_state.clone(),
                        protocol_turn_id,
                        sink.reserve_sequence_no(),
                    )
                    .await?;
                sink.emit_pre_recorded(crate::session::RunEvent::UserTurnStored {
                    session_id,
                    message_id: user_message.id,
                    turn: Box::new(user_turn.clone()),
                })?;
                sink.emit(crate::session::RunEvent::UserMessageStored {
                    message_id: user_message.id,
                })?;
                user_message.id
            };

            if !self
                .store
                .session_repo()
                .activate_admitted_turn(session_id, &admission_id, protocol_turn_id)
                .await?
            {
                return Err(AppRunError::Message(format!(
                    "run admission {admission_id} no longer owns session {session_id} while publishing its active turn"
                )));
            }

            let runtime_input = self.runtime_input_view(session_id).await?;
            let state = self.session_service.load_state(session_id).await?;
            let mut tree_confirmation = agent_context
                .as_ref()
                .map(crate::app::AgentRunContext::confirmation_prompt);
            let active_prompt: &mut dyn ConfirmationPrompt = match tree_confirmation.as_mut() {
                Some(confirmation) => confirmation,
                None => prompt,
            };
            let summary = self
                .agent_loop
                .run(
                    AgentRunRequest {
                        session: session_context,
                        admission_id: admission_id.clone(),
                        user_message_id,
                        protocol_turn_id,
                        runtime_input,
                        state,
                        config: effective_config.clone(),
                        model,
                        cancel: request.cancel.clone(),
                        live_config: request.live_config.clone(),
                        steer_rx: active_run.take_steer_receiver(),
                        is_sub_agent: agent_context
                            .as_ref()
                            .is_some_and(crate::app::AgentRunContext::is_sub_agent),
                        agent_context: agent_context.clone(),
                    },
                    active_prompt,
                    &mut sink,
                )
                .await?;
            drop(sink);
            renderer.finish(&summary)?;
            drop(active_run);
            if should_generate_session_title
                && let Some(title) = derive_session_title(&prepared.prompt)
            {
                let _ = self
                    .session_service
                    .update_session_title(session_id, title)
                    .await;
            }
            Ok(summary)
        }
        .await;
        heartbeat_stop.cancel();
        let heartbeat_result = match heartbeat_task.await {
            Ok(result) => result,
            Err(error) => {
                request.cancel.cancel();
                Err(crate::error::StorageError::Message(format!(
                    "run admission heartbeat task failed: {error}"
                )))
            }
        };
        let result = finish_admitted_run(
            &self.store,
            session_id,
            &admission_id,
            protocol_turn_id,
            request.cancel.is_cancelled(),
            admitted_result,
            heartbeat_result,
        )
        .await;
        if let Some(execution) = root_agent_execution.take() {
            self.agent_runtime
                .complete_root(execution, &result, request.cancel.is_cancelled());
        }
        drop(process_run_lease);
        result
    }

    async fn should_start_idle_goal_continuation(
        &self,
        session_id: crate::session::SessionId,
        cancel: &CancellationToken,
    ) -> Result<bool, AppRunError> {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        let admission = self
            .session_service
            .evaluate_idle_turn_admission(session_id, false, false)
            .await?;
        if !admission.admitted {
            return Ok(false);
        }
        let goal = self
            .store
            .session_repo()
            .get_thread_goal(session_id)
            .await?;
        Ok(goal.is_some_and(|goal| goal.status == ThreadGoalStatus::Active))
    }

    async fn session_id_for_goal_slash_control(
        &self,
        selector: &SessionSelector,
    ) -> Result<Option<crate::session::SessionId>, AppRunError> {
        match selector {
            SessionSelector::ById(session_id) => Ok(Some(*session_id)),
            SessionSelector::Latest => Ok(self
                .store
                .session_repo()
                .latest_session(self.workspace.project_id)
                .await?
                .map(|session| session.id)),
            SessionSelector::New => Ok(None),
        }
    }

    async fn execute_goal_slash_control(
        &self,
        session_id: crate::session::SessionId,
        command: GoalSlashCommand,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        match command {
            GoalSlashCommand::Get => {
                self.execute_session_goal_get(SessionGoalGetRequest { session_id }, renderer)
                    .await
            }
            GoalSlashCommand::Clear => {
                self.execute_session_goal_clear(SessionGoalClearRequest { session_id }, renderer)
                    .await
            }
            GoalSlashCommand::SetStatus(status) => {
                self.execute_session_goal_set(
                    SessionGoalSetRequest {
                        session_id,
                        objective: None,
                        status: Some(status),
                        token_budget: None,
                    },
                    renderer,
                )
                .await
            }
            GoalSlashCommand::SetObjective(_) => unreachable!("objective goal slash starts a run"),
        }
    }

    async fn set_goal_from_slash(
        &self,
        session_id: crate::session::SessionId,
        objective: &str,
        renderer: &mut dyn EventRenderer,
    ) -> Result<(), AppRunError> {
        let repo = self.store.session_repo();
        let current = account_goal_before_external_mutation(&repo, session_id).await?;
        let goal = if current.is_some() {
            repo.update_thread_goal(
                session_id,
                Some(objective),
                Some(ThreadGoalStatus::Active),
                None,
            )
            .await?
            .ok_or_else(|| {
                AppRunError::Message("thread goal disappeared during /goal update".to_string())
            })?
        } else {
            repo.replace_thread_goal(session_id, objective, ThreadGoalStatus::Active, None)
                .await?
        };
        renderer.render_thread_goal_set(&ThreadGoalSetResult { goal })?;
        Ok(())
    }

    async fn session_settings_for_selector(
        &self,
        selector: &SessionSelector,
    ) -> Result<Option<SessionRecord>, AppRunError> {
        Ok(self
            .session_service
            .resolve_session_for_workspace(selector, &self.workspace)
            .await?)
    }

    async fn hydrate_configured_model_from_provider(
        &self,
        config: &mut crate::config::ResolvedConfig,
        require_vision: bool,
        cancel: &CancellationToken,
    ) -> Result<(), AppRunError> {
        let configured_model = config.model.model.trim().to_string();
        if configured_model.is_empty() {
            return Err(AppRunError::Message(
                "configured model is empty".to_string(),
            ));
        }
        let key = format!(
            "{}:{require_vision}",
            serde_json::to_string(&config.model)
                .map_err(|error| AppRunError::Message(error.to_string()))?
        );
        let report = self
            .provider_probe_cache
            .get_or_probe_until_cancelled(key, cancel, || {
                check_model_availability(config, None, None, require_vision)
            })
            .await
            .ok_or_else(|| {
                AppRunError::Message(
                    "run cancelled while checking provider and model readiness".to_string(),
                )
            })?;
        apply_model_availability_report_to_config(&mut config.model, &report)
            .map_err(|error| AppRunError::Message(error.to_string()))?;
        Ok(())
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
            metrics: Default::default(),
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
            metrics: Default::default(),
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
            metrics: Default::default(),
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
            metrics: Default::default(),
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
            status: SessionStatus::Cancelled,
            finish_reason: Some(crate::session::FinishReason::Cancelled),
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
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
            metrics: Default::default(),
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
            metrics: Default::default(),
        })
    }

    async fn execute_session_goal_get(
        &self,
        request: SessionGoalGetRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        self.store
            .session_repo()
            .get_session(request.session_id)
            .await?;
        let result = ThreadGoalGetResult {
            goal: self
                .store
                .session_repo()
                .get_thread_goal(request.session_id)
                .await?,
        };
        renderer.render_thread_goal_get(&result)?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        })
    }

    async fn execute_session_goal_set(
        &self,
        request: SessionGoalSetRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        self.store
            .session_repo()
            .get_session(request.session_id)
            .await?;
        if request.objective.is_none() && request.status.is_none() && request.token_budget.is_none()
        {
            return Err(AppRunError::Message(
                "session goal set requires objective, --status, --token-budget, or --clear-token-budget".to_string(),
            ));
        }
        let repo = self.store.session_repo();
        let current = account_goal_before_external_mutation(&repo, request.session_id).await?;
        let goal = if let Some(objective) = request.objective.as_deref() {
            let objective = objective.trim();
            validate_thread_goal_objective(objective).map_err(AppRunError::Message)?;
            match current {
                Some(_) => repo
                    .update_thread_goal(
                        request.session_id,
                        Some(objective),
                        request.status,
                        request.token_budget,
                    )
                    .await?
                    .ok_or_else(|| {
                        AppRunError::Message("thread goal disappeared during update".to_string())
                    })?,
                None => {
                    let token_budget = request.token_budget.unwrap_or(None);
                    repo.replace_thread_goal(
                        request.session_id,
                        objective,
                        request.status.unwrap_or(ThreadGoalStatus::Active),
                        token_budget,
                    )
                    .await?
                }
            }
        } else {
            repo.update_thread_goal(
                request.session_id,
                None,
                request.status,
                request.token_budget,
            )
            .await?
            .ok_or_else(|| {
                AppRunError::Message(format!(
                    "session {} has no goal to update",
                    request.session_id
                ))
            })?
        };
        let result = ThreadGoalSetResult { goal };
        renderer.render_thread_goal_set(&result)?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        })
    }

    async fn execute_session_goal_clear(
        &self,
        request: SessionGoalClearRequest,
        renderer: &mut dyn EventRenderer,
    ) -> Result<RunSummary, AppRunError> {
        self.store
            .session_repo()
            .get_session(request.session_id)
            .await?;
        account_goal_before_external_mutation(&self.store.session_repo(), request.session_id)
            .await?;
        let result = ThreadGoalClearResult {
            thread_id: request.session_id,
            cleared: self
                .store
                .session_repo()
                .delete_thread_goal(request.session_id)
                .await?,
        };
        renderer.render_thread_goal_clear(&result)?;
        Ok(RunSummary {
            session_id: request.session_id,
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
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
            metrics: Default::default(),
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
            metrics: Default::default(),
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
            metrics: Default::default(),
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
            metrics: Default::default(),
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
            metrics: Default::default(),
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
            metrics: Default::default(),
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
            metrics: Default::default(),
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
            metrics: Default::default(),
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
            metrics: Default::default(),
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
            metrics: Default::default(),
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
            metrics: Default::default(),
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
        let active_turn_id = self
            .store
            .session_repo()
            .active_turn_for_session(request.session_id)
            .await?
            .ok_or_else(|| {
                AppRunError::Message(format!(
                    "session {} has no published active turn to steer",
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
            metrics: Default::default(),
        })
    }
}

async fn renew_admitted_run_lease_with_terminal_cancel(
    repo: crate::storage::SqliteSessionRepository,
    session_id: crate::session::SessionId,
    admission_id: String,
    turn_id: crate::protocol::TurnId,
    run_cancel: CancellationToken,
    agent_context: Option<crate::app::AgentRunContext>,
) -> Result<RunAdmissionLeaseRenewalOutcome, crate::error::StorageError> {
    let outcome = repo
        .renew_admitted_run_lease(session_id, &admission_id, turn_id)
        .await?;
    if outcome != RunAdmissionLeaseRenewalOutcome::GracefulTerminal {
        return Ok(outcome);
    }

    let status = repo.get_session(session_id).await?.status;
    let completed_by_this_turn = status == SessionStatus::Completed
        && repo
            .corroborated_terminal_status_for_turn(session_id, turn_id)
            .await?
            == Some(SessionStatus::Completed);
    if !completed_by_this_turn {
        run_cancel.cancel();
        if let Some(agent_context) = agent_context {
            let _ = agent_context.cancel_for_durable_terminal();
        }
    }
    Ok(outcome)
}

fn spawn_run_admission_heartbeat<Renew, RenewFuture>(
    session_id: crate::session::SessionId,
    admission_id: String,
    run_cancel: CancellationToken,
    failure_cancel: CancellationToken,
    heartbeat_stop: CancellationToken,
    heartbeat_interval: Duration,
    renew: Renew,
) -> tokio::task::JoinHandle<Result<(), crate::error::StorageError>>
where
    Renew: FnMut() -> RenewFuture + Send + 'static,
    RenewFuture:
        Future<Output = Result<RunAdmissionLeaseRenewalOutcome, crate::error::StorageError>>,
{
    // Permission surfaces intentionally wait for a human through a synchronous confirmation
    // boundary. Keep lease renewal off the foreground executor so a current-thread runtime can
    // remain blocked for an arbitrary confirmation interval without forfeiting run ownership.
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let thread_admission_id = admission_id.clone();
    let cancel_on_thread_failure = run_cancel.clone();
    let tree_cancel_on_thread_failure = failure_cancel.clone();
    let thread_spawn = std::thread::Builder::new()
        .name("moyai-run-admission-heartbeat".to_string())
        .spawn(move || {
            let result = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(async move {
                    let heartbeat = maintain_run_admission_lease(
                        session_id,
                        thread_admission_id.clone(),
                        heartbeat_stop,
                        heartbeat_interval,
                        renew,
                    );
                    match AssertUnwindSafe(heartbeat).catch_unwind().await {
                        Ok(result) => result,
                        Err(payload) => {
                            let message = payload
                                .downcast_ref::<&str>()
                                .map(|message| (*message).to_string())
                                .or_else(|| payload.downcast_ref::<String>().cloned())
                                .unwrap_or_else(|| "non-string panic payload".to_string());
                            Err(crate::error::StorageError::Message(format!(
                                "run admission heartbeat panicked for session {session_id} admission {thread_admission_id}: {message}"
                            )))
                        }
                    }
                }),
                Err(error) => Err(crate::error::StorageError::Message(format!(
                    "failed to build run admission heartbeat runtime for session {session_id} admission {thread_admission_id}: {error}"
                ))),
            };
            if result.is_err() {
                cancel_on_thread_failure.cancel();
                tree_cancel_on_thread_failure.cancel();
            }
            let _ = result_tx.send(result);
        });

    let thread_spawn_error = thread_spawn.err().map(|error| {
        run_cancel.cancel();
        failure_cancel.cancel();
        crate::error::StorageError::Message(format!(
            "failed to start run admission heartbeat thread for session {session_id} admission {admission_id}: {error}"
        ))
    });
    tokio::spawn(async move {
        if let Some(error) = thread_spawn_error {
            return Err(error);
        }
        result_rx.await.map_err(|_| {
            run_cancel.cancel();
            failure_cancel.cancel();
            crate::error::StorageError::Message(format!(
                "run admission heartbeat thread stopped without a result for session {session_id} admission {admission_id}"
            ))
        })?
    })
}

async fn maintain_run_admission_lease<Renew, RenewFuture>(
    session_id: crate::session::SessionId,
    admission_id: String,
    heartbeat_stop: CancellationToken,
    heartbeat_interval: Duration,
    mut renew: Renew,
) -> Result<(), crate::error::StorageError>
where
    Renew: FnMut() -> RenewFuture,
    RenewFuture:
        Future<Output = Result<RunAdmissionLeaseRenewalOutcome, crate::error::StorageError>>,
{
    loop {
        tokio::select! {
            biased;
            _ = heartbeat_stop.cancelled() => return Ok(()),
            _ = tokio::time::sleep(heartbeat_interval) => {
                match renew().await? {
                    RunAdmissionLeaseRenewalOutcome::Renewed => {}
                    RunAdmissionLeaseRenewalOutcome::GracefulTerminal => return Ok(()),
                    RunAdmissionLeaseRenewalOutcome::SupersededOrExpired => {
                        return Err(crate::error::StorageError::Message(format!(
                        "run admission {admission_id} lost its lease for session {session_id}"
                        )));
                    }
                }
            }
        }
    }
}

async fn finish_admitted_run(
    store: &StoreBundle,
    session_id: crate::session::SessionId,
    admission_id: &str,
    protocol_turn_id: crate::protocol::TurnId,
    cancellation_requested: bool,
    admitted_result: Result<RunSummary, AppRunError>,
    heartbeat_result: Result<(), crate::error::StorageError>,
) -> Result<RunSummary, AppRunError> {
    let heartbeat_failed = heartbeat_result.is_err();
    let admitted_result = match heartbeat_result {
        Ok(()) => admitted_result,
        Err(heartbeat_error) => match admitted_result {
            Ok(summary) => match store
                .session_repo()
                .corroborated_terminal_status_for_turn(session_id, protocol_turn_id)
                .await
            {
                Ok(Some(SessionStatus::Completed)) => {
                    // The exact turn's durable session/protocol commit is authoritative. The
                    // heartbeat failure remains diagnostic and must not reverse completed work.
                    Ok(summary)
                }
                Ok(_) => Err(AppRunError::Storage(heartbeat_error)),
                Err(authority_error) => Err(AppRunError::Message(format!(
                    "run admission heartbeat failed: {heartbeat_error}; additionally failed to verify durable terminal truth: {authority_error}"
                ))),
            },
            Err(run_error) => Err(AppRunError::Message(format!(
                "{run_error}; additionally the run admission heartbeat failed: {heartbeat_error}"
            ))),
        },
    };
    let settled = settle_admitted_run_result(
        store,
        session_id,
        admission_id,
        protocol_turn_id,
        cancellation_requested && !heartbeat_failed,
        admitted_result,
    )
    .await;
    let released = store
        .session_repo()
        .release_stopped_run_admission(session_id, admission_id)
        .await;
    match (settled, released) {
        (result, Ok(_)) => result,
        (Ok(_), Err(release_error)) => Err(AppRunError::Storage(release_error)),
        (Err(run_error), Err(release_error)) => Err(AppRunError::Message(format!(
            "{run_error}; additionally failed to release run admission {admission_id}: {release_error}"
        ))),
    }
}

async fn settle_admitted_run_result(
    store: &StoreBundle,
    session_id: crate::session::SessionId,
    admission_id: &str,
    protocol_turn_id: crate::protocol::TurnId,
    cancelled: bool,
    result: Result<RunSummary, AppRunError>,
) -> Result<RunSummary, AppRunError> {
    let Err(error) = result else {
        return result;
    };
    let (status, event) = if cancelled {
        (
            SessionStatus::Cancelled,
            crate::session::RunEvent::SessionInterrupted {
                session_id,
                reason: "run cancelled by user".to_string(),
            },
        )
    } else {
        (
            SessionStatus::Failed,
            crate::session::RunEvent::SessionFailed {
                session_id,
                message: error.to_string(),
            },
        )
    };
    let repo = store.session_repo();
    let terminalized = repo
        .terminalize_admitted_session_with_protocol_event(
            session_id,
            admission_id,
            status,
            &event,
            protocol_turn_id,
            None,
        )
        .await
        .map_err(|cleanup_error| {
            AppRunError::Message(format!(
                "{error}; additionally failed to settle admitted run {admission_id}: {cleanup_error}"
            ))
        })?;
    let _ = terminalized;
    Err(error)
}

async fn account_goal_before_external_mutation(
    repo: &crate::storage::SqliteSessionRepository,
    session_id: crate::session::SessionId,
) -> Result<Option<crate::session::ThreadGoal>, AppRunError> {
    let current = repo.get_thread_goal_with_id(session_id).await?;
    if current.as_ref().is_some_and(|(goal, _goal_id)| {
        matches!(
            goal.status,
            crate::session::ThreadGoalStatus::Active
                | crate::session::ThreadGoalStatus::BudgetLimited
        )
    }) {
        let (_goal, goal_id) = current.expect("current goal checked above");
        repo.account_thread_goal_usage_for_goal(session_id, 0, Some(goal_id.as_str()))
            .await
            .map_err(AppRunError::from)
    } else {
        Ok(current.map(|(goal, _goal_id)| goal))
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum GoalSlashCommand {
    Get,
    Clear,
    SetStatus(ThreadGoalStatus),
    SetObjective(String),
}

fn parse_goal_slash_command(prompt: &str) -> Result<Option<GoalSlashCommand>, AppRunError> {
    let trimmed = prompt.trim();
    let Some(rest) = trimmed.strip_prefix("/goal") else {
        return Ok(None);
    };
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return Ok(None);
    }

    let arg = rest.trim();
    if arg.is_empty() {
        return Ok(Some(GoalSlashCommand::Get));
    }

    match arg.to_ascii_lowercase().as_str() {
        "clear" => Ok(Some(GoalSlashCommand::Clear)),
        "pause" => Ok(Some(GoalSlashCommand::SetStatus(ThreadGoalStatus::Paused))),
        "resume" => Ok(Some(GoalSlashCommand::SetStatus(ThreadGoalStatus::Active))),
        "edit" => Err(AppRunError::Message(
            "Usage: /goal [<objective>|clear|pause|resume]. /goal edit is not available in this surface; send /goal <new objective> to replace the goal."
                .to_string(),
        )),
        _ => {
            validate_thread_goal_objective(arg).map_err(AppRunError::Message)?;
            Ok(Some(GoalSlashCommand::SetObjective(arg.to_string())))
        }
    }
}

fn allows_goal_idle_continuation_after_run(prompt: &str) -> Result<bool, AppRunError> {
    Ok(match parse_goal_slash_command(prompt)? {
        Some(GoalSlashCommand::Get)
        | Some(GoalSlashCommand::Clear)
        | Some(GoalSlashCommand::SetStatus(ThreadGoalStatus::Paused)) => false,
        Some(GoalSlashCommand::SetStatus(ThreadGoalStatus::Active))
        | Some(GoalSlashCommand::SetObjective(_))
        | None => true,
        Some(GoalSlashCommand::SetStatus(_)) => false,
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use camino::Utf8PathBuf;

    use crate::config::ProviderMetadataMode;
    use crate::llm::{ModelAvailabilityReport, ModelAvailabilityStatus};
    use crate::session::{
        NewSession, ProjectId, ProjectRepository, SessionRepository, ThreadGoalStatus,
    };
    use crate::storage::{SqliteStore, StoragePaths, StoreBundle};

    fn probe_report(model: &str) -> ModelAvailabilityReport {
        ModelAvailabilityReport {
            gate: "model_availability".to_string(),
            status: ModelAvailabilityStatus::Pass,
            generated_by: "test".to_string(),
            model: model.to_string(),
            base_url: "http://local".to_string(),
            provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
            v1_present: true,
            native_present: false,
            require_vision: false,
            vision_capable: false,
            vision_probe_passed: false,
            vision_probes: Vec::new(),
            tool_use_capable: Some(true),
            capability_overrides: Vec::new(),
            tool_call_probe_passed: true,
            tool_call_probes: Vec::new(),
            reasoning_capable: Some(false),
            context: Some(8192),
            max_output_tokens: Some(1024),
            max_parallel_predictions: Some(1),
            matched_model: None,
            v1_models: vec![model.to_string()],
            native_models: Vec::new(),
            openai_error: None,
            native_error: None,
            checked_at_ms: 1,
        }
    }

    async fn heartbeat_active_turn_fixture(
        title: &str,
    ) -> (
        StoreBundle,
        crate::session::SessionId,
        String,
        crate::protocol::TurnId,
        crate::session::MessageRecord,
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8PathBuf::from_path_buf(temp.keep().join("data")).expect("utf8 data dir");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let sqlite = SqliteStore::open(&paths).expect("store");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let project_id = ProjectId::new();
        store
            .project_repo()
            .upsert_project(project_id, &data_dir, "test", "none")
            .await
            .expect("project");
        let session = store
            .session_repo()
            .create_session(NewSession {
                project_id,
                title: title.to_string(),
                cwd: data_dir,
                model: "model".to_string(),
                base_url: "http://localhost:1234".to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("session");
        let admission_id = store
            .session_repo()
            .admit_session_run(session.id)
            .await
            .expect("admission")
            .expect("admitted");
        let turn_id = crate::protocol::TurnId::new();
        assert!(
            store
                .session_repo()
                .activate_admitted_turn(session.id, &admission_id, turn_id)
                .await
                .expect("activate turn")
        );
        let (assistant, _) = store
            .session_repo()
            .append_assistant_message_with_protocol_start(
                crate::session::NewMessage {
                    session_id: session.id,
                    parent_message_id: None,
                    role: crate::session::MessageRole::Assistant,
                    metadata: crate::session::MessageMetadata::Assistant(
                        crate::session::AssistantMessageMeta {
                            model: "model".to_string(),
                            base_url: "http://localhost:1234".to_string(),
                            finish_reason: None,
                            token_usage: None,
                            summary: false,
                        },
                    ),
                },
                &admission_id,
                turn_id,
                None,
                "model".to_string(),
            )
            .await
            .expect("assistant");
        (store, session.id, admission_id, turn_id, assistant)
    }

    fn successful_run_summary(session_id: crate::session::SessionId) -> crate::session::RunSummary {
        crate::session::RunSummary {
            session_id,
            assistant_message_id: None,
            status: crate::session::SessionStatus::Completed,
            finish_reason: Some(crate::session::FinishReason::Stop),
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }
    }

    async fn commit_completed_turn(
        store: &StoreBundle,
        session_id: crate::session::SessionId,
        admission_id: &str,
        turn_id: crate::protocol::TurnId,
        assistant: &crate::session::MessageRecord,
    ) -> crate::storage::session_repo::AdmittedTerminalCommit {
        store
            .session_repo()
            .update_admitted_message_metadata_and_status_with_protocol_event(
                session_id,
                admission_id,
                assistant.id,
                &crate::session::MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                    model: "model".to_string(),
                    base_url: "http://localhost:1234".to_string(),
                    finish_reason: Some(crate::session::FinishReason::Stop),
                    token_usage: None,
                    summary: false,
                }),
                crate::session::SessionStatus::Completed,
                &crate::session::RunEvent::SessionCompleted {
                    session_id,
                    finish_reason: Some(crate::session::FinishReason::Stop),
                },
                turn_id,
                None,
                None,
                None,
            )
            .await
            .expect("terminal commit")
    }

    async fn panicking_heartbeat_renewal() -> Result<
        crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome,
        crate::error::StorageError,
    > {
        panic!("injected heartbeat panic")
    }

    async fn gated_panicking_heartbeat_renewal(
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    ) -> Result<
        crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome,
        crate::error::StorageError,
    > {
        entered.notify_one();
        release.notified().await;
        panic!("injected gated heartbeat panic")
    }

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

    #[test]
    fn parses_goal_slash_controls() {
        assert_eq!(
            super::parse_goal_slash_command("/goal").unwrap(),
            Some(super::GoalSlashCommand::Get)
        );
        assert_eq!(
            super::parse_goal_slash_command("  /goal clear  ").unwrap(),
            Some(super::GoalSlashCommand::Clear)
        );
        assert_eq!(
            super::parse_goal_slash_command("/goal pause").unwrap(),
            Some(super::GoalSlashCommand::SetStatus(ThreadGoalStatus::Paused))
        );
        assert_eq!(
            super::parse_goal_slash_command("/goal resume").unwrap(),
            Some(super::GoalSlashCommand::SetStatus(ThreadGoalStatus::Active))
        );
    }

    #[test]
    fn parses_goal_slash_objective_without_matching_other_commands() {
        assert_eq!(
            super::parse_goal_slash_command("/goal finish the release checklist").unwrap(),
            Some(super::GoalSlashCommand::SetObjective(
                "finish the release checklist".to_string()
            ))
        );
        assert_eq!(
            super::parse_goal_slash_command("/goalkeeper").unwrap(),
            None
        );
        assert_eq!(
            super::parse_goal_slash_command("/other goal").unwrap(),
            None
        );
    }

    #[test]
    fn rejects_goal_edit_without_surface_editor() {
        assert!(super::parse_goal_slash_command("/goal edit").is_err());
    }

    #[test]
    fn goal_idle_continuation_policy_matches_goal_commands() {
        assert!(super::allows_goal_idle_continuation_after_run("build the feature").unwrap());
        assert!(
            super::allows_goal_idle_continuation_after_run("/goal finish the release checklist")
                .unwrap()
        );
        assert!(super::allows_goal_idle_continuation_after_run("/goal resume").unwrap());
        assert!(!super::allows_goal_idle_continuation_after_run("/goal").unwrap());
        assert!(!super::allows_goal_idle_continuation_after_run("/goal clear").unwrap());
        assert!(!super::allows_goal_idle_continuation_after_run("/goal pause").unwrap());
    }

    #[tokio::test]
    async fn provider_probe_cache_reuses_identity_and_invalidates_on_key_change() {
        let cache = super::ProviderProbeCache::default();
        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let calls = Arc::clone(&calls);
            cache
                .get_or_probe("provider-a".to_string(), move || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    probe_report("model-a")
                })
                .await;
        }
        let changed_calls = Arc::clone(&calls);
        cache
            .get_or_probe("provider-b".to_string(), move || async move {
                changed_calls.fetch_add(1, Ordering::SeqCst);
                probe_report("model-b")
            })
            .await;

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn provider_probe_cache_does_not_serialize_different_provider_keys() {
        let cache = super::ProviderProbeCache::default();
        let (first_entered_tx, first_entered_rx) = tokio::sync::oneshot::channel();
        let (release_first_tx, release_first_rx) = tokio::sync::oneshot::channel();
        let first_cache = cache.clone();
        let first = tokio::spawn(async move {
            first_cache
                .get_or_probe("provider-a".to_string(), move || async move {
                    let _ = first_entered_tx.send(());
                    let _ = release_first_rx.await;
                    probe_report("model-a")
                })
                .await
        });
        first_entered_rx.await.expect("first probe entered");

        let (second_entered_tx, second_entered_rx) = tokio::sync::oneshot::channel();
        let second_cache = cache.clone();
        let second = tokio::spawn(async move {
            second_cache
                .get_or_probe("provider-b".to_string(), move || async move {
                    let _ = second_entered_tx.send(());
                    probe_report("model-b")
                })
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(1), second_entered_rx)
            .await
            .expect("an unrelated provider probe must not wait for the first network request")
            .expect("second probe entered");
        release_first_tx.send(()).expect("release first probe");
        assert_eq!(first.await.expect("first probe task").model, "model-a");
        assert_eq!(second.await.expect("second probe task").model, "model-b");
    }

    #[tokio::test]
    async fn provider_probe_cache_single_flights_concurrent_misses_for_the_same_key() {
        let cache = super::ProviderProbeCache::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let (first_entered_tx, first_entered_rx) = tokio::sync::oneshot::channel();
        let (release_first_tx, release_first_rx) = tokio::sync::oneshot::channel();
        let first_cache = cache.clone();
        let first_calls = Arc::clone(&calls);
        let first = tokio::spawn(async move {
            first_cache
                .get_or_probe("provider-a".to_string(), move || async move {
                    first_calls.fetch_add(1, Ordering::SeqCst);
                    let _ = first_entered_tx.send(());
                    let _ = release_first_rx.await;
                    probe_report("model-a")
                })
                .await
        });
        first_entered_rx.await.expect("first probe entered");

        let second_cache = cache.clone();
        let second_calls = Arc::clone(&calls);
        let second = tokio::spawn(async move {
            second_cache
                .get_or_probe("provider-a".to_string(), move || async move {
                    second_calls.fetch_add(1, Ordering::SeqCst);
                    probe_report("duplicate-model")
                })
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a concurrent miss for the same key must wait for the in-flight probe"
        );

        release_first_tx.send(()).expect("release first probe");
        assert_eq!(first.await.expect("first probe task").model, "model-a");
        assert_eq!(second.await.expect("second probe task").model, "model-a");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn provider_probe_cache_cancellation_drops_the_in_flight_readiness_probe() {
        let cache = super::ProviderProbeCache::default();
        let cancel = tokio_util::sync::CancellationToken::new();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let probe_cache = cache.clone();
        let probe_cancel = cancel.clone();
        let probe = tokio::spawn(async move {
            probe_cache
                .get_or_probe_until_cancelled(
                    "provider-a".to_string(),
                    &probe_cancel,
                    move || async move {
                        let _ = entered_tx.send(());
                        std::future::pending::<ModelAvailabilityReport>().await
                    },
                )
                .await
        });
        entered_rx.await.expect("readiness probe entered");

        cancel.cancel();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), probe)
                .await
                .expect("cancelled readiness probe must stop promptly")
                .expect("readiness probe task")
                .is_none()
        );

        let report = cache
            .get_or_probe("provider-a".to_string(), || async {
                probe_report("model-after-cancel")
            })
            .await;
        assert_eq!(report.model, "model-after-cancel");
    }

    #[tokio::test]
    async fn every_post_admission_setup_error_settles_the_owned_run() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8PathBuf::from_path_buf(temp.keep().join("data")).expect("utf8 data dir");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let sqlite = SqliteStore::open(&paths).expect("store");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let project_id = ProjectId::new();
        store
            .project_repo()
            .upsert_project(project_id, &data_dir, "test", "none")
            .await
            .expect("project");

        for setup_stage in [
            "active run registration",
            "goal setup",
            "harness recorder setup",
            "active turn registration",
            "canonical turn setup",
        ] {
            let session = store
                .session_repo()
                .create_session(NewSession {
                    project_id,
                    title: setup_stage.to_string(),
                    cwd: data_dir.clone(),
                    model: "model".to_string(),
                    base_url: "http://localhost:1234".to_string(),
                    access_mode: crate::config::AccessMode::Default,
                })
                .await
                .expect("session");
            let admission_id = store
                .session_repo()
                .admit_session_run(session.id)
                .await
                .expect("admission")
                .expect("admitted");
            let failure = super::settle_admitted_run_result(
                &store,
                session.id,
                &admission_id,
                crate::protocol::TurnId::new(),
                false,
                Err(crate::error::AppRunError::Message(format!(
                    "{setup_stage} failed"
                ))),
            )
            .await
            .expect_err("setup failure returned");

            assert!(failure.to_string().contains(setup_stage));
            assert_eq!(
                store
                    .session_repo()
                    .get_session(session.id)
                    .await
                    .expect("settled session")
                    .status,
                crate::session::SessionStatus::Failed
            );
            assert!(
                store
                    .session_repo()
                    .admit_session_run(session.id)
                    .await
                    .expect("readmission")
                    .is_some(),
                "{setup_stage} must release durable admission ownership"
            );
        }
    }

    #[tokio::test]
    async fn background_heartbeat_extends_the_admission_while_the_run_is_waiting() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8PathBuf::from_path_buf(temp.keep().join("data")).expect("utf8 data dir");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let sqlite = SqliteStore::open(&paths).expect("store");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let project_id = ProjectId::new();
        store
            .project_repo()
            .upsert_project(project_id, &data_dir, "test", "none")
            .await
            .expect("project");
        let session = store
            .session_repo()
            .create_session(crate::session::NewSession {
                project_id,
                title: "heartbeat".to_string(),
                cwd: data_dir,
                model: "model".to_string(),
                base_url: "http://localhost:1234".to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("session");
        let admitted_at_ms = crate::runtime::SystemClock::now_ms();
        let admission_id = store
            .session_repo()
            .admit_session_run_at(session.id, admitted_at_ms, 3_000)
            .await
            .expect("admission")
            .expect("admitted");
        let turn_id = crate::protocol::TurnId::new();
        assert!(
            store
                .session_repo()
                .activate_admitted_turn(session.id, &admission_id, turn_id)
                .await
                .expect("activate turn")
        );

        let run_cancel = tokio_util::sync::CancellationToken::new();
        let heartbeat_stop = tokio_util::sync::CancellationToken::new();
        let heartbeat_repo = store.session_repo();
        let heartbeat_admission_id = admission_id.clone();
        let heartbeat_task = super::spawn_run_admission_heartbeat(
            session.id,
            admission_id,
            run_cancel.clone(),
            run_cancel.clone(),
            heartbeat_stop.clone(),
            std::time::Duration::from_millis(5),
            move || {
                let repo = heartbeat_repo.clone();
                let admission_id = heartbeat_admission_id.clone();
                async move {
                    repo.renew_admitted_run_lease(session.id, &admission_id, turn_id)
                        .await
                }
            },
        );

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if store
                    .session_repo()
                    .has_fresh_run_admission_at(session.id, admitted_at_ms + 5_000)
                    .await
                    .expect("fresh admission")
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("background heartbeat did not extend the lease");
        assert!(!run_cancel.is_cancelled());
        heartbeat_stop.cancel();
        heartbeat_task
            .await
            .expect("heartbeat task")
            .expect("heartbeat result");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn heartbeat_progresses_while_foreground_runtime_is_synchronously_blocked() {
        let renewal_count = Arc::new(AtomicUsize::new(0));
        let heartbeat_renewal_count = Arc::clone(&renewal_count);
        let run_cancel = tokio_util::sync::CancellationToken::new();
        let heartbeat_stop = tokio_util::sync::CancellationToken::new();
        let heartbeat_task = super::spawn_run_admission_heartbeat(
            crate::session::SessionId::new(),
            "blocked-permission-admission".to_string(),
            run_cancel.clone(),
            run_cancel.clone(),
            heartbeat_stop.clone(),
            std::time::Duration::from_millis(5),
            move || {
                heartbeat_renewal_count.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Ok(
                    crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome::Renewed,
                ))
            },
        );

        // Mirrors the synchronous human-confirmation boundary on a current-thread runtime.
        std::thread::sleep(std::time::Duration::from_millis(150));

        assert!(
            renewal_count.load(Ordering::SeqCst) > 0,
            "lease renewal must not depend on the foreground executor making progress"
        );
        assert!(!run_cancel.is_cancelled());
        heartbeat_stop.cancel();
        heartbeat_task
            .await
            .expect("heartbeat task")
            .expect("heartbeat result");
    }

    #[tokio::test]
    async fn durable_interrupt_cancels_a_foreground_permission_wait() {
        let (store, session_id, admission_id, turn_id, _) =
            heartbeat_active_turn_fixture("external interrupt heartbeat").await;
        assert!(
            store
                .session_repo()
                .terminalize_admitted_session_with_protocol_event(
                    session_id,
                    &admission_id,
                    crate::session::SessionStatus::Cancelled,
                    &crate::session::RunEvent::SessionInterrupted {
                        session_id,
                        reason: "external stop".to_string(),
                    },
                    turn_id,
                    None,
                )
                .await
                .expect("external interrupt")
        );
        let run_cancel = tokio_util::sync::CancellationToken::new();

        assert_eq!(
            super::renew_admitted_run_lease_with_terminal_cancel(
                store.session_repo(),
                session_id,
                admission_id,
                turn_id,
                run_cancel.clone(),
                None,
            )
            .await
            .expect("terminal renewal"),
            crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome::GracefulTerminal
        );
        assert!(run_cancel.is_cancelled());
    }

    #[tokio::test]
    async fn own_completed_turn_does_not_cancel_after_terminal_heartbeat_race() {
        let (store, session_id, admission_id, turn_id, assistant) =
            heartbeat_active_turn_fixture("own completed heartbeat").await;
        assert_eq!(
            commit_completed_turn(&store, session_id, &admission_id, turn_id, &assistant).await,
            crate::storage::session_repo::AdmittedTerminalCommit::Applied
        );
        let run_cancel = tokio_util::sync::CancellationToken::new();

        assert_eq!(
            super::renew_admitted_run_lease_with_terminal_cancel(
                store.session_repo(),
                session_id,
                admission_id,
                turn_id,
                run_cancel.clone(),
                None,
            )
            .await
            .expect("terminal renewal"),
            crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome::GracefulTerminal
        );
        assert!(!run_cancel.is_cancelled());
    }

    #[tokio::test]
    async fn terminal_commit_wins_the_heartbeat_barrier_without_reversing_success() {
        let (store, session_id, admission_id, turn_id, assistant) =
            heartbeat_active_turn_fixture("terminal heartbeat barrier").await;
        let renewal_entered = Arc::new(tokio::sync::Notify::new());
        let allow_renewal = Arc::new(tokio::sync::Notify::new());
        let heartbeat_repo = store.session_repo();
        let heartbeat_admission_id = admission_id.clone();
        let heartbeat_renewal_entered = Arc::clone(&renewal_entered);
        let heartbeat_allow_renewal = Arc::clone(&allow_renewal);
        let run_cancel = tokio_util::sync::CancellationToken::new();
        let heartbeat_task = super::spawn_run_admission_heartbeat(
            session_id,
            admission_id.clone(),
            run_cancel.clone(),
            run_cancel.clone(),
            tokio_util::sync::CancellationToken::new(),
            std::time::Duration::from_millis(1),
            move || {
                let repo = heartbeat_repo.clone();
                let admission_id = heartbeat_admission_id.clone();
                let renewal_entered = Arc::clone(&heartbeat_renewal_entered);
                let allow_renewal = Arc::clone(&heartbeat_allow_renewal);
                async move {
                    renewal_entered.notify_one();
                    allow_renewal.notified().await;
                    repo.renew_admitted_run_lease(session_id, &admission_id, turn_id)
                        .await
                }
            },
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            renewal_entered.notified(),
        )
        .await
        .expect("heartbeat did not reach the terminal barrier");

        assert_eq!(
            store
                .session_repo()
                .update_admitted_message_metadata_and_status_with_protocol_event(
                    session_id,
                    &admission_id,
                    assistant.id,
                    &crate::session::MessageMetadata::Assistant(
                        crate::session::AssistantMessageMeta {
                            model: "model".to_string(),
                            base_url: "http://localhost:1234".to_string(),
                            finish_reason: Some(crate::session::FinishReason::Stop),
                            token_usage: None,
                            summary: false,
                        },
                    ),
                    crate::session::SessionStatus::Completed,
                    &crate::session::RunEvent::SessionCompleted {
                        session_id,
                        finish_reason: Some(crate::session::FinishReason::Stop),
                    },
                    turn_id,
                    None,
                    None,
                    None,
                )
                .await
                .expect("terminal commit"),
            crate::storage::session_repo::AdmittedTerminalCommit::Applied
        );
        allow_renewal.notify_one();
        let heartbeat_result = heartbeat_task.await.expect("heartbeat task");
        assert!(!run_cancel.is_cancelled());
        let completed = super::finish_admitted_run(
            &store,
            session_id,
            &admission_id,
            turn_id,
            run_cancel.is_cancelled(),
            Ok(successful_run_summary(session_id)),
            heartbeat_result,
        )
        .await
        .expect("graceful heartbeat must not reverse terminal success");
        assert_eq!(completed.status, crate::session::SessionStatus::Completed);
    }

    #[tokio::test]
    async fn completed_commit_after_heartbeat_error_remains_the_durable_authority() {
        let (store, session_id, admission_id, turn_id, assistant) =
            heartbeat_active_turn_fixture("heartbeat error before completed commit").await;
        let renewal_entered = Arc::new(tokio::sync::Notify::new());
        let release_error = Arc::new(tokio::sync::Notify::new());
        let heartbeat_renewal_entered = Arc::clone(&renewal_entered);
        let heartbeat_release_error = Arc::clone(&release_error);
        let run_cancel = tokio_util::sync::CancellationToken::new();
        let heartbeat_task = super::spawn_run_admission_heartbeat(
            session_id,
            admission_id.clone(),
            run_cancel.clone(),
            run_cancel.clone(),
            tokio_util::sync::CancellationToken::new(),
            std::time::Duration::from_millis(1),
            move || {
                let renewal_entered = Arc::clone(&heartbeat_renewal_entered);
                let release_error = Arc::clone(&heartbeat_release_error);
                async move {
                    renewal_entered.notify_one();
                    release_error.notified().await;
                    Err(crate::error::StorageError::Message(
                        "heartbeat failed immediately before completion".to_string(),
                    ))
                }
            },
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            renewal_entered.notified(),
        )
        .await
        .expect("heartbeat did not reach the error barrier");
        release_error.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), run_cancel.cancelled())
            .await
            .expect("heartbeat error did not cancel the run");
        let heartbeat_result = heartbeat_task.await.expect("heartbeat task");

        assert_eq!(
            commit_completed_turn(&store, session_id, &admission_id, turn_id, &assistant).await,
            crate::storage::session_repo::AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            store
                .session_repo()
                .corroborated_terminal_status_for_turn(session_id, turn_id)
                .await
                .expect("durable terminal truth"),
            Some(crate::session::SessionStatus::Completed)
        );
        let completed = super::finish_admitted_run(
            &store,
            session_id,
            &admission_id,
            turn_id,
            run_cancel.is_cancelled(),
            Ok(successful_run_summary(session_id)),
            heartbeat_result,
        )
        .await
        .expect("durable completion must override the heartbeat diagnostic");
        assert_eq!(completed.status, crate::session::SessionStatus::Completed);
    }

    #[tokio::test]
    async fn completed_commit_before_heartbeat_panic_remains_the_durable_authority() {
        let (store, session_id, admission_id, turn_id, assistant) =
            heartbeat_active_turn_fixture("completed commit before heartbeat panic").await;
        let renewal_entered = Arc::new(tokio::sync::Notify::new());
        let release_panic = Arc::new(tokio::sync::Notify::new());
        let heartbeat_renewal_entered = Arc::clone(&renewal_entered);
        let heartbeat_release_panic = Arc::clone(&release_panic);
        let run_cancel = tokio_util::sync::CancellationToken::new();
        let heartbeat_task = super::spawn_run_admission_heartbeat(
            session_id,
            admission_id.clone(),
            run_cancel.clone(),
            run_cancel.clone(),
            tokio_util::sync::CancellationToken::new(),
            std::time::Duration::from_millis(1),
            move || {
                gated_panicking_heartbeat_renewal(
                    Arc::clone(&heartbeat_renewal_entered),
                    Arc::clone(&heartbeat_release_panic),
                )
            },
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            renewal_entered.notified(),
        )
        .await
        .expect("heartbeat did not reach the panic barrier");
        assert_eq!(
            commit_completed_turn(&store, session_id, &admission_id, turn_id, &assistant).await,
            crate::storage::session_repo::AdmittedTerminalCommit::Applied
        );
        release_panic.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), run_cancel.cancelled())
            .await
            .expect("heartbeat panic did not cancel the run");
        let heartbeat_result = heartbeat_task.await.expect("heartbeat task");

        let completed = super::finish_admitted_run(
            &store,
            session_id,
            &admission_id,
            turn_id,
            run_cancel.is_cancelled(),
            Ok(successful_run_summary(session_id)),
            heartbeat_result,
        )
        .await
        .expect("durable completion must override the heartbeat panic diagnostic");
        assert_eq!(completed.status, crate::session::SessionStatus::Completed);
    }

    #[tokio::test]
    async fn heartbeat_error_finished_before_terminal_commit_settles_as_failure() {
        let (store, session_id, admission_id, turn_id, assistant) =
            heartbeat_active_turn_fixture("heartbeat storage error").await;
        let run_cancel = tokio_util::sync::CancellationToken::new();
        let heartbeat_task = super::spawn_run_admission_heartbeat(
            session_id,
            admission_id.clone(),
            run_cancel.clone(),
            run_cancel.clone(),
            tokio_util::sync::CancellationToken::new(),
            std::time::Duration::from_millis(1),
            || {
                std::future::ready(Err(crate::error::StorageError::Message(
                    "injected heartbeat storage error".to_string(),
                )))
            },
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), run_cancel.cancelled())
            .await
            .expect("storage failure did not cancel the run");
        let heartbeat_result = heartbeat_task.await.expect("heartbeat task");
        let failure = super::finish_admitted_run(
            &store,
            session_id,
            &admission_id,
            turn_id,
            run_cancel.is_cancelled(),
            Ok(successful_run_summary(session_id)),
            heartbeat_result,
        )
        .await
        .expect_err("heartbeat storage failure must fail the run");

        assert!(
            failure
                .to_string()
                .contains("injected heartbeat storage error")
        );
        assert_eq!(
            store
                .session_repo()
                .get_session(session_id)
                .await
                .expect("settled session")
                .status,
            crate::session::SessionStatus::Failed
        );
        assert_eq!(
            store
                .session_repo()
                .corroborated_terminal_status_for_turn(session_id, turn_id)
                .await
                .expect("failed terminal truth"),
            Some(crate::session::SessionStatus::Failed)
        );
        assert_eq!(
            commit_completed_turn(&store, session_id, &admission_id, turn_id, &assistant).await,
            crate::storage::session_repo::AdmittedTerminalCommit::NotOwned
        );
        assert!(
            store
                .session_repo()
                .admit_session_run(session_id)
                .await
                .expect("readmission")
                .is_some()
        );
    }

    #[tokio::test]
    async fn heartbeat_panic_cancels_and_still_settles_and_releases() {
        let (store, session_id, admission_id, turn_id, _) =
            heartbeat_active_turn_fixture("heartbeat panic").await;
        let run_cancel = tokio_util::sync::CancellationToken::new();
        let heartbeat_task = super::spawn_run_admission_heartbeat(
            session_id,
            admission_id.clone(),
            run_cancel.clone(),
            run_cancel.clone(),
            tokio_util::sync::CancellationToken::new(),
            std::time::Duration::from_millis(1),
            panicking_heartbeat_renewal,
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), run_cancel.cancelled())
            .await
            .expect("heartbeat panic did not cancel the run");
        let heartbeat_result = heartbeat_task.await.expect("heartbeat task");
        let failure = super::finish_admitted_run(
            &store,
            session_id,
            &admission_id,
            turn_id,
            run_cancel.is_cancelled(),
            Ok(successful_run_summary(session_id)),
            heartbeat_result,
        )
        .await
        .expect_err("heartbeat panic must fail the run");

        assert!(failure.to_string().contains("injected heartbeat panic"));
        assert_eq!(
            store
                .session_repo()
                .get_session(session_id)
                .await
                .expect("settled session")
                .status,
            crate::session::SessionStatus::Failed
        );
        assert!(
            store
                .session_repo()
                .admit_session_run(session_id)
                .await
                .expect("readmission")
                .is_some()
        );
    }
}
