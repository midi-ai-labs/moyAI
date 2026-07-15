use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use camino::{Utf8Path, Utf8PathBuf};
use futures_util::FutureExt;
use tokio_util::sync::CancellationToken;

use crate::agent::context_manager::ContextManager;
use crate::agent::mode::CollaborationMode;
use crate::agent::turn_context::TurnContext as RuntimeTurnContext;
use crate::agent::{AgentLoop, AgentRunRequest};
use crate::app::agent_runtime::{AgentRuntimeContinuationOutcome, AgentRuntimeExecution};
use crate::app::session_title::{derive_session_title, is_placeholder_session_title};
use crate::app::{
    AppCommand, ReviewRequest, RunRequest, SessionArchiveRequest, SessionEventsRequest,
    SessionForkRequest, SessionGoalClearRequest, SessionGoalGetRequest, SessionGoalSetRequest,
    SessionHistoryRequest, SessionIdleAdmissionRequest, SessionInterruptRequest,
    SessionListRequest, SessionLoadedRequest, SessionReadRequest, SessionRejoinRequest,
    SessionRollbackRequest, SessionSearchRequest, SessionSettingsUpdateRequest, SessionShowRequest,
    SessionSteerRequest, SessionTitleUpdateRequest, SessionTurnsRequest,
};
use crate::cli::{ConfirmationPrompt, EventRenderer};
use crate::config::model::PartialResolvedConfig;
use crate::config::{ModelConfig, ResolvedConfig, merge::apply_patch as apply_config_patch};
use crate::error::{AgentError, AppRunError, RuntimeError};
use crate::harness::{HarnessRecordingSink, NativeHarnessRecorder};
use crate::llm::model_policy::{ModelPolicy, ProviderCapabilities, ResolvedTurnPolicy};
use crate::protocol::{
    AdditionalContextEntry, AdditionalContextKind, ProtocolEventStore, ProtocolRecordingSink,
    SteerTurn, UserInputItem, UserTurn,
};
use crate::runtime::{
    RunCancellationCause, RunContinuationOutcome, RunControl, RunEventSink, SessionRuntimeEventHub,
};
use crate::session::{
    DispatchTransformKind, ImagePart, PromptDispatchPart, RunSummary, SessionModelParameters,
    SessionRecord, SessionRepository, SessionSelector, SessionSettingsPatch, SessionStartRequest,
    SessionStatus, ThreadGoalClearResult, ThreadGoalGetResult, ThreadGoalSetResult,
    ThreadGoalStatus, validate_thread_goal_objective,
};
use crate::storage::{
    StoreBundle,
    session_repo::{RUN_ADMISSION_HEARTBEAT_INTERVAL_MS, RunAdmissionLeaseRenewalOutcome},
};
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

    pub fn cancel_agent_tree(
        &self,
        session_id: crate::session::SessionId,
        root_cause: crate::protocol::TurnInterruptionCause,
    ) -> bool {
        self.agent_runtime
            .cancel_tree_for_session(session_id, root_cause)
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

    async fn context_manager(
        &self,
        session_id: crate::session::SessionId,
    ) -> Result<ContextManager, AppRunError> {
        let history_items = self
            .store
            .protocol_event_store()
            .list_history_items_for_session(session_id)?;
        let context = ContextManager::rehydrate(history_items);
        if !context.has_user_turn() {
            return Err(AppRunError::Message(
                "cannot build runtime input without a canonical protocol user turn".to_string(),
            ));
        }
        Ok(context)
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
            .execute_single_run(request.clone(), renderer, prompt, None)
            .await?;
        if !allow_idle_goal_continuation {
            return Ok(summary);
        }

        'continuations: loop {
            let preclaimed_root_execution = loop {
                self.wait_for_agent_tree_quiescence(summary.session_id)
                    .await?;
                let cancel = request.run_control.token();
                if !self
                    .should_start_idle_goal_continuation(summary.session_id, &cancel)
                    .await?
                {
                    break 'continuations;
                }
                match self
                    .agent_runtime
                    .begin_root_continuation(
                        summary.session_id,
                        request.run_control.clone(),
                        request.agent_confirmation.clone(),
                    )
                    .map_err(AppRunError::Message)?
                {
                    AgentRuntimeContinuationOutcome::Unmanaged => {
                        match request.run_control.begin_next_turn_after_success() {
                            RunContinuationOutcome::Admitted => break None,
                            RunContinuationOutcome::Blocked => break 'continuations,
                            RunContinuationOutcome::Invalid => {
                                return Err(AppRunError::Message(format!(
                                    "session {} admitted an idle goal continuation without a sealed successful prior turn",
                                    summary.session_id
                                )));
                            }
                        }
                    }
                    AgentRuntimeContinuationOutcome::Admitted(execution) => {
                        break Some(execution);
                    }
                    AgentRuntimeContinuationOutcome::Blocked => break 'continuations,
                    AgentRuntimeContinuationOutcome::NotReady => continue,
                    AgentRuntimeContinuationOutcome::Invalid => {
                        return Err(AppRunError::Message(format!(
                            "session {} admitted an idle goal continuation without the retained root run owner",
                            summary.session_id
                        )));
                    }
                }
            };
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
                show_reasoning_summary: request.show_reasoning_summary,
                prompt_dispatch: None,
                editor_context: None,
                review_request: None,
                image_paths: Vec::new(),
                run_control: request.run_control.clone(),
                live_config: request.live_config.clone(),
                agent_confirmation: request.agent_confirmation.clone(),
                agent_context: request.agent_context.clone(),
            };
            summary = self
                .execute_single_run(
                    continuation_request,
                    renderer,
                    prompt,
                    preclaimed_root_execution,
                )
                .await?;
        }

        Ok(summary)
    }

    async fn execute_single_run(
        &self,
        request: RunRequest,
        renderer: &mut dyn EventRenderer,
        prompt: &mut dyn ConfirmationPrompt,
        mut root_agent_execution: Option<AgentRuntimeExecution>,
    ) -> Result<RunSummary, AppRunError> {
        let run_control = request.run_control.clone();
        let result = self
            .execute_single_run_inner(request, renderer, prompt, &mut root_agent_execution)
            .await;
        if let Err(error) = &result {
            classify_run_error(&run_control, error);
        }
        if let Some(execution) = root_agent_execution.take() {
            self.agent_runtime
                .complete_root(execution, &result, run_control.cause());
        }
        result
    }

    async fn execute_single_run_inner(
        &self,
        mut request: RunRequest,
        renderer: &mut dyn EventRenderer,
        prompt: &mut dyn ConfirmationPrompt,
        root_agent_execution: &mut Option<AgentRuntimeExecution>,
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
        let effective_config = compose_run_effective_config(
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
                || (existing.status == SessionStatus::Running
                    && self
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
        let collaboration_mode =
            resolve_session_collaboration_mode(&self.session_service, session_context.session.id)
                .await?;
        let mode = collaboration_mode;
        let turn_policy = Arc::new(ResolvedTurnPolicy::resolve(
            &mode,
            ModelPolicy::from_config(&effective_config),
            ProviderCapabilities::from_config(&effective_config),
            effective_config.model.reasoning_summary,
        )?);
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
        let preclaimed_agent_context = root_agent_execution
            .as_ref()
            .map(|execution| execution.context.clone());
        if supplied_agent_context.is_some() && preclaimed_agent_context.is_some() {
            return Err(AppRunError::Message(
                "a run cannot combine a supplied agent context with a preclaimed root continuation"
                    .to_string(),
            ));
        }
        let provided_agent_context = supplied_agent_context.or(preclaimed_agent_context);
        if let Some(context) = provided_agent_context.as_ref() {
            if context.session_id() != session_context.session.id {
                return Err(AppRunError::Message(format!(
                    "agent context session {} does not match requested session {}",
                    context.session_id(),
                    session_context.session.id
                )));
            }
        }
        let root_confirmation =
            if provided_agent_context.is_none() && effective_config.multi_agent.enabled {
                Some(request.agent_confirmation.clone().ok_or_else(|| {
                    AppRunError::Message(
                        "multi-agent execution requires a shared permission confirmation channel"
                            .to_string(),
                    )
                })?)
            } else {
                None
            };
        let protocol_turn_id = crate::protocol::TurnId::new();
        let process_run_lease = self
            .store
            .try_acquire_run_process_lease(session_context.session.id)?;
        let Some(admission_id) = self
            .store
            .session_repo()
            .admit_session_turn(session_context.session.id, protocol_turn_id)
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
        let agent_context = if let Some(context) = provided_agent_context {
            Some(context)
        } else if let Some(confirmation) = root_confirmation {
            let execution = match self.agent_runtime.begin_root(
                &session_context,
                effective_config.clone(),
                confirmation,
                request.live_config.clone(),
                request.run_control.clone(),
            ) {
                Ok(execution) => execution,
                Err(error) => {
                    let result = finish_admitted_run(
                        &self.store,
                        session_id,
                        &admission_id,
                        protocol_turn_id,
                        &request.run_control,
                        Err(AppRunError::Message(error)),
                        Ok(()),
                    )
                    .await;
                    drop(process_run_lease);
                    return result;
                }
            };
            let context = execution.context.clone();
            *root_agent_execution = Some(execution);
            Some(context)
        } else {
            None
        };
        let heartbeat_stop = CancellationToken::new();
        let heartbeat_repo = self.store.session_repo();
        let heartbeat_protocol_store = self.store.protocol_event_store();
        let heartbeat_admission_id = admission_id.clone();
        let heartbeat_run_control = request.run_control.clone();
        let heartbeat_agent_context = agent_context.clone();
        let heartbeat_task = spawn_run_admission_heartbeat(
            session_id,
            admission_id.clone(),
            request.run_control.clone(),
            heartbeat_stop.clone(),
            Duration::from_millis(RUN_ADMISSION_HEARTBEAT_INTERVAL_MS),
            move || {
                let repo = heartbeat_repo.clone();
                let protocol_store = heartbeat_protocol_store.clone();
                let admission_id = heartbeat_admission_id.clone();
                let run_control = heartbeat_run_control.clone();
                let agent_context = heartbeat_agent_context.clone();
                async move {
                    renew_admitted_run_lease_with_terminal_cancel(
                        repo,
                        protocol_store,
                        session_id,
                        admission_id,
                        protocol_turn_id,
                        run_control,
                        agent_context,
                    )
                    .await
                }
            },
        );
        let turn_context = Arc::new(RuntimeTurnContext {
            turn_id: protocol_turn_id,
            admission_id: admission_id.clone(),
            mode,
            policy: turn_policy,
            multi_agent_mode: effective_config
                .multi_agent
                .enabled
                .then_some(effective_config.multi_agent.mode),
            current_time: crate::context::current_time::CurrentTimeSnapshot::now(),
        });
        let admitted_result: Result<RunSummary, AppRunError> = async {
            let mut active_run = self
                .store
                .active_runs()
                .try_start(session_id, request.run_control.clone())?;
            if let Some(GoalSlashCommand::SetObjective(objective)) = slash_goal_command {
                self.set_goal_from_slash(session_id, &objective, renderer)
                    .await?;
            }
            let mut renderer_sink = RendererSink {
                renderer,
                show_reasoning_summary: request.show_reasoning_summary,
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

            if prepared.prompt.trim().is_empty() {
                let context = self.context_manager(session_id).await?;
                if !context.has_user_turn() {
                    return Err(AppRunError::Message(
                        "cannot resume a session without a prompt or prior user turn".to_string(),
                    ));
                }
            } else {
                let user_turn = build_user_turn(
                    protocol_turn_id,
                    &prepared,
                    &image_parts,
                    request.editor_context.clone(),
                );
                self.session_service
                    .store_user_turn_with_protocol_bundle(
                        &session_context,
                        &admission_id,
                        &user_turn,
                        protocol_turn_id,
                        sink.reserve_sequence_no(),
                    )
                    .await?;
                sink.emit_committed(crate::session::RunEvent::UserTurnStored {
                    session_id,
                    turn: Box::new(user_turn),
                })?;
            }

            let context = self.context_manager(session_id).await?;
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
                        turn: turn_context,
                        context,
                        config: effective_config.clone(),
                        run_control: request.run_control.clone(),
                        live_config: request.live_config.clone(),
                        steer_rx: active_run.take_steer_receiver(),
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
                request
                    .run_control
                    .fail(format!("run admission heartbeat task failed: {error}"));
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
            &request.run_control,
            admitted_result,
            heartbeat_result,
        )
        .await;
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
            .evaluate_idle_turn_admission(session_id, false)
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Cancelled,
            finish_reason: Some(crate::session::FinishReason::Cancelled),
            interruption_cause: Some(crate::protocol::TurnInterruptionCause::UserStop),
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            .evaluate_idle_turn_admission(request.session_id, request.pending_trigger_turn)
            .await?;
        renderer.render_session_idle_turn_admission(&admission)?;
        Ok(RunSummary {
            session_id: request.session_id,
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
        renderer.render_session_history_items(&session, &history_items)?;
        Ok(RunSummary {
            session_id: request.session_id,
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Running,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            interruption_cause: None,
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
            turn_id: None,
            final_response_id: None,
            status: SessionStatus::Running,
            finish_reason: None,
            interruption_cause: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        })
    }
}

async fn renew_admitted_run_lease_with_terminal_cancel(
    repo: crate::storage::SqliteSessionRepository,
    protocol_store: crate::protocol::SqliteProtocolEventStore,
    session_id: crate::session::SessionId,
    admission_id: String,
    turn_id: crate::protocol::TurnId,
    run_control: RunControl,
    agent_context: Option<crate::app::AgentRunContext>,
) -> Result<RunAdmissionLeaseRenewalOutcome, crate::error::StorageError> {
    let outcome = repo
        .renew_admitted_run_lease(session_id, &admission_id, turn_id)
        .await?;
    if outcome == RunAdmissionLeaseRenewalOutcome::SupersededOrExpired {
        run_control.supersede();
        return Ok(outcome);
    }
    if outcome != RunAdmissionLeaseRenewalOutcome::GracefulTerminal {
        return Ok(outcome);
    }

    let terminal_status = protocol_store
        .list_runtime_events(session_id, turn_id)?
        .into_iter()
        .rev()
        .find_map(|event| match event.msg {
            crate::protocol::RuntimeEventMsg::TurnTerminal { terminal } => {
                Some(terminal.status.as_session_status())
            }
            _ => None,
        });
    match terminal_status {
        Some(SessionStatus::Completed) => {}
        Some(SessionStatus::Cancelled | SessionStatus::Failed)
        | Some(SessionStatus::Running | SessionStatus::Idle)
        | None => {
            // A terminal session without a corroborating event for this turn belongs to another
            // durable observer, as do terminal statuses other than this turn's own successful
            // commit. The existing durable event remains authoritative; the local worker must not
            // manufacture a second interruption/failure classification.
            run_control.supersede();
            if let Some(agent_context) = agent_context {
                let _ = agent_context.cancel_for_durable_terminal();
            }
        }
    }
    Ok(outcome)
}

fn spawn_run_admission_heartbeat<Renew, RenewFuture>(
    session_id: crate::session::SessionId,
    admission_id: String,
    run_control: RunControl,
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
    let control_on_thread_failure = run_control.clone();
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
            if let Err(error) = &result {
                record_heartbeat_failure(&control_on_thread_failure, error.to_string());
            }
            let _ = result_tx.send(result);
        });

    let thread_spawn_error = thread_spawn.err().map(|error| {
        let error = crate::error::StorageError::Message(format!(
            "failed to start run admission heartbeat thread for session {session_id} admission {admission_id}: {error}"
        ));
        record_heartbeat_failure(&run_control, error.to_string());
        error
    });
    tokio::spawn(async move {
        if let Some(error) = thread_spawn_error {
            return Err(error);
        }
        result_rx.await.map_err(|_| {
            let error = crate::error::StorageError::Message(format!(
                "run admission heartbeat thread stopped without a result for session {session_id} admission {admission_id}"
            ));
            record_heartbeat_failure(&run_control, error.to_string());
            error
        })?
    })
}

fn record_heartbeat_failure(run_control: &RunControl, message: String) {
    // Root controls route Failure through AgentControl synchronously. Child and standalone
    // controls intentionally keep the exact-owner first-writer behavior in RunControl.
    run_control.fail(message);
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
    run_control: &RunControl,
    admitted_result: Result<RunSummary, AppRunError>,
    heartbeat_result: Result<(), crate::error::StorageError>,
) -> Result<RunSummary, AppRunError> {
    let admitted_result = match heartbeat_result {
        Ok(()) => admitted_result,
        Err(heartbeat_error) => match admitted_result {
            Ok(summary) => {
                match durable_run_summary_for_turn(store, session_id, protocol_turn_id).await {
                    Ok(Some(durable)) if durable.status == SessionStatus::Completed => {
                        // The exact turn's durable session/protocol commit is authoritative. The
                        // heartbeat failure remains diagnostic and must not reverse completed work.
                        Ok(summary)
                    }
                    Ok(_) => Err(AppRunError::Storage(heartbeat_error)),
                    Err(authority_error) => Err(AppRunError::Message(format!(
                        "run admission heartbeat failed: {heartbeat_error}; additionally failed to verify durable terminal truth: {authority_error}"
                    ))),
                }
            }
            Err(run_error) => Err(AppRunError::Message(format!(
                "{run_error}; additionally the run admission heartbeat failed: {heartbeat_error}"
            ))),
        },
    };
    if let Err(error) = &admitted_result {
        classify_run_error(run_control, error);
    }
    let settled = settle_admitted_run_result(
        store,
        session_id,
        admission_id,
        protocol_turn_id,
        run_control.cause(),
        admitted_result,
    )
    .await;
    if settled
        .as_ref()
        .is_ok_and(|summary| summary.status == SessionStatus::Completed)
    {
        run_control.seal_success();
    }
    let released = store
        .session_repo()
        .release_stopped_run_admission(session_id, admission_id)
        .await;
    reconcile_admitted_run_release(settled, released, admission_id)
}

pub(crate) fn classify_run_error(run_control: &RunControl, error: &AppRunError) {
    let cause = if matches!(error, AppRunError::Agent(AgentError::RunSuperseded { .. })) {
        RunCancellationCause::Superseded
    } else {
        RunCancellationCause::Failure(error.to_string())
    };
    let _ = run_control.request_cancel(cause);
}

fn reconcile_admitted_run_release(
    settled: Result<RunSummary, AppRunError>,
    released: Result<bool, crate::error::StorageError>,
    admission_id: &str,
) -> Result<RunSummary, AppRunError> {
    match (settled, released) {
        (result, Ok(_)) => result,
        (Ok(summary), Err(release_error)) if summary.status == SessionStatus::Completed => {
            eprintln!(
                "warning: durable run {} completed, but admission {admission_id} could not be released: {release_error}",
                summary.session_id
            );
            Ok(summary)
        }
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
    cancellation_cause: Option<RunCancellationCause>,
    result: Result<RunSummary, AppRunError>,
) -> Result<RunSummary, AppRunError> {
    let Err(error) = result else {
        return result;
    };
    if matches!(cancellation_cause, Some(RunCancellationCause::Superseded)) {
        if let Some(summary) = durable_run_summary_for_turn(store, session_id, protocol_turn_id)
            .await
            .map_err(|authority_error| {
                AppRunError::Message(format!(
                    "{error}; additionally failed to verify durable terminal truth after supersession: {authority_error}"
                ))
            })?
        {
            return Ok(summary);
        }
        return Err(AppRunError::Agent(AgentError::RunSuperseded {
            session_id,
            admission_id: admission_id.to_string(),
        }));
    }
    let terminal = match cancellation_cause {
        Some(RunCancellationCause::Interruption(cause)) => crate::session::DurableTurnTerminal {
            status: crate::protocol::TurnTerminalStatus::Interrupted,
            finish_reason: Some(crate::session::FinishReason::Cancelled),
            interruption_cause: Some(cause),
            final_response_id: None,
            summary: cause.legacy_reason().to_string(),
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        },
        Some(RunCancellationCause::Failure(message)) => crate::session::DurableTurnTerminal {
            status: crate::protocol::TurnTerminalStatus::Failed,
            finish_reason: Some(crate::session::FinishReason::Error),
            interruption_cause: None,
            final_response_id: None,
            summary: message,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        },
        Some(RunCancellationCause::Superseded) => unreachable!("handled above"),
        None => crate::session::DurableTurnTerminal {
            status: crate::protocol::TurnTerminalStatus::Failed,
            finish_reason: Some(crate::session::FinishReason::Error),
            interruption_cause: None,
            final_response_id: None,
            summary: error.to_string(),
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        },
    };
    let event = crate::session::RunEvent::TurnTerminal {
        session_id,
        terminal: Box::new(terminal),
    };
    let repo = store.session_repo();
    let terminalized = repo
        .terminalize_admitted_turn_with_protocol_event(
            session_id,
            admission_id,
            &event,
            protocol_turn_id,
            None,
            None,
            None,
            None,
        )
        .await
        .map_err(|cleanup_error| {
            AppRunError::Message(format!(
                "{error}; additionally failed to settle admitted run {admission_id}: {cleanup_error}"
            ))
        })?;
    if terminalized != crate::storage::session_repo::AdmittedTerminalCommit::Applied
        && let Some(summary) = durable_run_summary_for_turn(store, session_id, protocol_turn_id)
            .await
            .map_err(|authority_error| {
                AppRunError::Message(format!(
                    "{error}; additionally failed to verify durable terminal truth after losing terminalization: {authority_error}"
                ))
            })?
    {
        return Ok(summary);
    }
    Err(error)
}

async fn durable_run_summary_for_turn(
    store: &StoreBundle,
    session_id: crate::session::SessionId,
    protocol_turn_id: crate::protocol::TurnId,
) -> Result<Option<RunSummary>, crate::error::StorageError> {
    let terminal = store
        .protocol_event_store()
        .list_runtime_events(session_id, protocol_turn_id)?
        .into_iter()
        .rev()
        .find_map(|event| match event.msg {
            crate::protocol::RuntimeEventMsg::TurnTerminal { terminal } => Some(*terminal),
            _ => None,
        });
    Ok(terminal.map(|terminal| RunSummary {
        session_id,
        turn_id: Some(protocol_turn_id),
        final_response_id: terminal.final_response_id,
        status: terminal.status.as_session_status(),
        finish_reason: terminal.finish_reason,
        interruption_cause: terminal.interruption_cause,
        tool_call_count: terminal.tool_call_count,
        failed_tool_count: terminal.failed_tool_count,
        change_count: terminal.change_count,
        metrics: terminal.metrics,
    }))
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

async fn resolve_session_collaboration_mode(
    session_service: &crate::session::SessionService,
    session_id: crate::session::SessionId,
) -> Result<CollaborationMode, AppRunError> {
    let kind = session_service.collaboration_mode(session_id).await?;
    Ok(CollaborationMode::resolve(kind))
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

fn build_user_turn(
    turn_id: crate::protocol::TurnId,
    prepared: &PreparedRunTurn,
    images: &[ImagePart],
    editor_context: Option<crate::session::EditorContext>,
) -> UserTurn {
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
    UserTurn {
        turn_id,
        items,
        prompt_dispatch: prepared.prompt_dispatch.clone(),
        editor_context,
    }
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

fn build_review_prompt(raw_prompt: &str, scope: &crate::workspace::ReviewScope) -> String {
    let mode_line = match scope.mode {
        crate::workspace::ReviewScopeMode::Uncommitted => {
            "Review the current uncommitted workspace changes.".to_string()
        }
        crate::workspace::ReviewScopeMode::Branch => format!(
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
    show_reasoning_summary: bool,
}

impl<'a> RunEventSink for RendererSink<'a> {
    fn emit(&mut self, event: crate::session::RunEvent) -> Result<(), RuntimeError> {
        if matches!(
            event,
            crate::session::RunEvent::ReasoningSummaryDelta { .. }
        ) && !self.show_reasoning_summary
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

    use crate::config::model::{ProviderApiMode, ReasoningEffort};
    use crate::config::{ProviderMetadataMode, ResolvedConfig};
    use crate::protocol::{ModeKind, ProtocolEventStore};
    use crate::session::{
        NewSession, ProjectId, ProjectRepository, SessionRepository, ThreadGoalStatus,
    };
    use crate::storage::{SqliteStore, StoragePaths, StoreBundle};

    struct UnreachableLlm;

    #[async_trait::async_trait(?Send)]
    impl crate::llm::LlmClient for UnreachableLlm {
        async fn stream_chat(
            &self,
            _request: crate::llm::ChatRequest,
            _cancel: tokio_util::sync::CancellationToken,
            _sink: &mut dyn crate::llm::LlmEventSink,
        ) -> Result<crate::llm::LlmResponseSummary, crate::error::LlmError> {
            panic!("turn policy failure must happen before model dispatch")
        }
    }

    struct NoPrompt;

    impl crate::cli::ConfirmationPrompt for NoPrompt {
        fn confirm(
            &mut self,
            _request: &crate::tool::PermissionRequest,
        ) -> Result<crate::cli::ReviewDecision, crate::error::CliPromptError> {
            Ok(crate::cli::ReviewDecision::Denied)
        }
    }

    async fn run_service_fixture(
        config: ResolvedConfig,
    ) -> (
        Arc<super::RunService>,
        StoreBundle,
        crate::workspace::Workspace,
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8PathBuf::from_path_buf(temp.keep().join("data")).expect("utf8 data dir");
        std::fs::create_dir_all(data_dir.as_std_path()).expect("data dir");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let sqlite = SqliteStore::open(&paths).expect("store");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let workspace =
            crate::workspace::WorkspaceDiscovery::discover_fixed_root(&data_dir, &config)
                .expect("workspace");
        store
            .project_repo()
            .upsert_project(workspace.project_id, &workspace.root, "test", "none")
            .await
            .expect("project");
        let session_service = crate::session::SessionService::new(store.clone());
        let agent_runtime = Arc::new(crate::app::AgentRuntime::new(
            store.clone(),
            session_service.clone(),
        ));
        let tool_services = crate::tool::context::ToolServices {
            edit_safety: crate::edit::EditSafety::default(),
            formatter: crate::edit::Formatter::new(config.format.clone()),
            change_tracker: crate::edit::ChangeTracker::default(),
            store: store.clone(),
            storage_paths: store.paths().clone(),
            truncator: crate::tool::truncate::ToolTruncator,
            mcp: Arc::new(crate::mcp::McpClient::new(config.mcp.clone())),
            skills: crate::skill::SkillsService::new(),
        };
        let agent_loop = crate::agent::AgentLoop::new(
            Arc::new(UnreachableLlm),
            crate::tool::registry::ToolRegistry::core_agent_for_config(&config),
            store.clone(),
            crate::agent::PromptBuilder,
            tool_services,
        );
        let run_service = Arc::new(super::RunService::new(
            store.clone(),
            config,
            workspace.clone(),
            session_service,
            agent_loop,
            crate::runtime::SessionRuntimeEventHub::new(16),
            agent_runtime.clone(),
        ));
        agent_runtime
            .bind_run_service(Arc::downgrade(&run_service))
            .expect("bind run service");
        (run_service, store, workspace)
    }

    async fn heartbeat_active_turn_fixture(
        title: &str,
    ) -> (
        StoreBundle,
        crate::session::SessionId,
        String,
        crate::protocol::TurnId,
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
        let turn_id = crate::protocol::TurnId::new();
        let admission_id = store
            .session_repo()
            .admit_session_turn(session.id, turn_id)
            .await
            .expect("admission")
            .expect("admitted");
        (store, session.id, admission_id, turn_id)
    }

    #[tokio::test]
    async fn run_mode_resolution_replays_the_latest_history_instruction() {
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
        let session_id = store
            .session_repo()
            .create_session(NewSession {
                project_id,
                title: "persistent collaboration mode".to_string(),
                cwd: data_dir,
                model: "model".to_string(),
                base_url: "http://localhost:1234".to_string(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("session")
            .id;
        let session_service = crate::session::SessionService::new(store);
        session_service
            .set_collaboration_mode(session_id, ModeKind::Plan)
            .await
            .expect("persist plan mode")
            .expect("mode instruction");

        let mode = super::resolve_session_collaboration_mode(&session_service, session_id)
            .await
            .expect("resolve mode for resumed run");
        assert_eq!(mode.kind, ModeKind::Plan);
        assert!(mode.developer_instructions.is_some());
    }

    #[test]
    fn retained_root_owner_supports_more_than_three_successful_continuation_turns() {
        let root_control = crate::runtime::RunControl::new();
        let (tree, first_execution) = crate::runtime::AgentControl::with_root_control(
            crate::session::SessionId::new(),
            1,
            root_control.clone(),
        )
        .expect("agent tree");
        let mut execution = first_execution;

        for completed_turn in 0..6 {
            assert!(root_control.seal_success());
            tree.complete_execution(
                execution,
                crate::runtime::AgentStatus::Completed(None),
                None,
            )
            .expect("complete root turn");
            if completed_turn == 5 {
                break;
            }
            execution = match tree
                .try_acquire_root_continuation(root_control.clone())
                .expect("continuation outcome")
            {
                crate::runtime::AgentRootContinuationOutcome::Admitted(execution) => execution,
                crate::runtime::AgentRootContinuationOutcome::Blocked
                | crate::runtime::AgentRootContinuationOutcome::NotReady
                | crate::runtime::AgentRootContinuationOutcome::Invalid => {
                    panic!("retained root owner rejected continuation turn {completed_turn}")
                }
            };
            assert!(execution.run_control().same_owner(&root_control));
        }

        assert!(root_control.success_is_sealed());
        assert!(!tree.tree_is_cancelled());
        assert!(tree.is_quiescent().expect("tree quiescence"));
    }

    #[tokio::test]
    async fn invalid_turn_policy_fails_before_durable_run_admission() {
        let mut config = ResolvedConfig::default();
        config.model.model = "policy-error-model".to_string();
        config.model.base_url = "http://local".to_string();
        config.model.provider_metadata_mode = ProviderMetadataMode::OpenAiCompatibleOnly;
        config.model.provider_api_mode = ProviderApiMode::ChatCompletions;
        config.model.chat_completions_reasoning_parameters = None;
        config.model.reasoning_effort = Some(ReasoningEffort::Medium);
        config.model.supports_reasoning = true;
        config.multi_agent.enabled = false;

        let (run_service, store, workspace) = run_service_fixture(config.clone()).await;
        let run_control = crate::runtime::RunControl::new();
        let mut renderer = crate::cli::HumanRenderer::new();
        let mut prompt = NoPrompt;

        let error = run_service
            .execute(
                crate::app::AppCommand::Run(crate::app::RunRequest {
                    prompt: "exercise invalid turn policy".to_string(),
                    session_id: None,
                    continue_last: false,
                    title: Some("policy failure".to_string()),
                    cwd: workspace.cwd.clone(),
                    model: String::new(),
                    base_url: String::new(),
                    config_override: None,
                    output_mode: crate::cli::OutputMode::Human,
                    show_reasoning_summary: false,
                    prompt_dispatch: None,
                    editor_context: None,
                    review_request: None,
                    image_paths: Vec::new(),
                    run_control: run_control.clone(),
                    live_config: None,
                    agent_confirmation: None,
                    agent_context: None,
                }),
                &mut renderer,
                &mut prompt,
            )
            .await
            .expect_err("unsupported reasoning policy must reject the run");

        assert!(error.to_string().contains("does not support it"));
        assert!(matches!(
            run_control.cause(),
            Some(crate::runtime::RunCancellationCause::Failure(message))
                if message.contains("does not support it")
        ));
        let sessions = store
            .session_repo()
            .list_sessions(workspace.project_id, 10)
            .await
            .expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].status, crate::session::SessionStatus::Idle);
        assert!(
            !store
                .session_repo()
                .has_fresh_run_admission(sessions[0].id)
                .await
                .expect("admission state")
        );
        assert!(
            store
                .protocol_event_store()
                .list_history_items_for_session(sessions[0].id)
                .expect("history")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn idle_goal_continuation_stops_only_for_semantic_goal_or_cancel_state() {
        let config = ResolvedConfig::default();
        let (run_service, store, workspace) = run_service_fixture(config.clone()).await;
        let session = store
            .session_repo()
            .create_session(NewSession {
                project_id: workspace.project_id,
                title: "semantic continuation stop".to_string(),
                cwd: workspace.cwd,
                model: config.model.model,
                base_url: config.model.base_url,
                access_mode: config.permissions.access_mode,
            })
            .await
            .expect("session");
        store
            .session_repo()
            .replace_thread_goal(
                session.id,
                "continue until the semantic goal is complete",
                ThreadGoalStatus::Active,
                None,
            )
            .await
            .expect("active goal");
        let active = tokio_util::sync::CancellationToken::new();
        assert!(
            run_service
                .should_start_idle_goal_continuation(session.id, &active)
                .await
                .expect("active goal admission")
        );

        store
            .session_repo()
            .update_thread_goal(
                session.id,
                None,
                Some(ThreadGoalStatus::BudgetLimited),
                None,
            )
            .await
            .expect("budget-limited goal");
        assert!(
            !run_service
                .should_start_idle_goal_continuation(session.id, &active)
                .await
                .expect("budget stop")
        );

        store
            .session_repo()
            .update_thread_goal(session.id, None, Some(ThreadGoalStatus::Active), None)
            .await
            .expect("reactivated goal");
        let cancelled = tokio_util::sync::CancellationToken::new();
        cancelled.cancel();
        assert!(
            !run_service
                .should_start_idle_goal_continuation(session.id, &cancelled)
                .await
                .expect("cancel stop")
        );

        store
            .session_repo()
            .update_thread_goal(session.id, None, Some(ThreadGoalStatus::Complete), None)
            .await
            .expect("completed goal");
        assert!(
            !run_service
                .should_start_idle_goal_continuation(session.id, &active)
                .await
                .expect("completed stop")
        );
    }

    fn successful_run_summary(session_id: crate::session::SessionId) -> crate::session::RunSummary {
        crate::session::RunSummary {
            session_id,
            turn_id: None,
            final_response_id: None,
            status: crate::session::SessionStatus::Completed,
            finish_reason: Some(crate::session::FinishReason::Stop),
            interruption_cause: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        }
    }

    #[test]
    fn admission_release_error_cannot_reverse_durable_success() {
        let session_id = crate::session::SessionId::new();
        let summary = successful_run_summary(session_id);
        let reconciled = super::reconcile_admitted_run_release(
            Ok(summary),
            Err(crate::error::StorageError::Message(
                "release write failed".to_string(),
            )),
            "admission",
        )
        .expect("durable success remains observable");

        assert_eq!(reconciled.session_id, session_id);
        assert_eq!(reconciled.status, crate::session::SessionStatus::Completed);
    }

    async fn commit_completed_turn(
        store: &StoreBundle,
        session_id: crate::session::SessionId,
        admission_id: &str,
        turn_id: crate::protocol::TurnId,
    ) -> crate::storage::session_repo::AdmittedTerminalCommit {
        let event = terminal_event(
            session_id,
            crate::protocol::TurnTerminalStatus::Completed,
            "completed",
            None,
        );
        store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                session_id,
                admission_id,
                &event,
                turn_id,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("terminal commit")
    }

    fn terminal_event(
        session_id: crate::session::SessionId,
        status: crate::protocol::TurnTerminalStatus,
        summary: &str,
        interruption_cause: Option<crate::protocol::TurnInterruptionCause>,
    ) -> crate::session::RunEvent {
        let finish_reason = Some(match status {
            crate::protocol::TurnTerminalStatus::Completed => crate::session::FinishReason::Stop,
            crate::protocol::TurnTerminalStatus::Interrupted => {
                crate::session::FinishReason::Cancelled
            }
            crate::protocol::TurnTerminalStatus::Failed => crate::session::FinishReason::Error,
        });
        crate::session::RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(crate::session::DurableTurnTerminal {
                status,
                finish_reason,
                interruption_cause,
                final_response_id: None,
                summary: summary.to_string(),
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        }
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
            let turn_id = crate::protocol::TurnId::new();
            let admission_id = store
                .session_repo()
                .admit_session_turn(session.id, turn_id)
                .await
                .expect("admission")
                .expect("admitted");
            let failure = super::settle_admitted_run_result(
                &store,
                session.id,
                &admission_id,
                turn_id,
                None,
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
                    .admit_session_turn(session.id, crate::protocol::TurnId::new())
                    .await
                    .expect("readmission")
                    .is_some(),
                "{setup_stage} must release durable admission ownership"
            );
        }
    }

    #[test]
    fn admitted_operational_error_classification_preserves_the_first_terminal_owner() {
        let operational_error = crate::error::AppRunError::Message("provider failed".to_string());

        let open = crate::runtime::RunControl::new();
        super::classify_run_error(&open, &operational_error);
        assert_eq!(
            open.cause(),
            Some(crate::runtime::RunCancellationCause::Failure(
                "provider failed".to_string()
            ))
        );

        let interrupted = crate::runtime::RunControl::new();
        assert!(interrupted.interrupt(crate::protocol::TurnInterruptionCause::UserStop));
        super::classify_run_error(&interrupted, &operational_error);
        assert_eq!(
            interrupted.cause(),
            Some(crate::runtime::RunCancellationCause::Interruption(
                crate::protocol::TurnInterruptionCause::UserStop
            ))
        );

        let superseded = crate::runtime::RunControl::new();
        assert!(superseded.supersede());
        super::classify_run_error(&superseded, &operational_error);
        assert_eq!(
            superseded.cause(),
            Some(crate::runtime::RunCancellationCause::Superseded)
        );

        let success = crate::runtime::RunControl::new();
        let reservation = success.begin_success_commit().expect("success reservation");
        super::classify_run_error(&success, &operational_error);
        assert_eq!(success.cause(), None);
        assert!(reservation.seal());
        assert!(success.success_is_sealed());
    }

    #[test]
    fn root_operational_error_closes_sibling_admission_before_durable_terminal_settlement() {
        let root_control = crate::runtime::RunControl::new();
        let (tree, _root_execution) = crate::runtime::AgentControl::with_root_control(
            crate::session::SessionId::new(),
            2,
            root_control.clone(),
        )
        .expect("agent tree");
        let (_, sibling_execution) = tree
            .register_child(
                &crate::runtime::AgentPath::root(),
                "sibling",
                crate::session::SessionId::new(),
                None,
            )
            .expect("sibling");
        let sibling_control = sibling_execution.run_control();
        let failure = crate::runtime::RunCancellationCause::Failure(
            "provider failed before terminal settlement".to_string(),
        );

        // `finish_admitted_run` invokes this synchronous classifier immediately before its
        // awaited durable terminal settlement. The whole tree must already be closed on return.
        super::classify_run_error(
            &root_control,
            &crate::error::AppRunError::Message(
                "provider failed before terminal settlement".to_string(),
            ),
        );

        assert_eq!(root_control.cause(), Some(failure.clone()));
        assert_eq!(sibling_control.cause(), Some(failure));
        assert!(tree.tree_is_cancelled());
        assert!(sibling_control.begin_tool_effect_admission().is_none());
    }

    #[test]
    fn heartbeat_failure_closes_sibling_admission_while_root_effect_is_reserved() {
        let root_control = crate::runtime::RunControl::new();
        let (tree, _root_execution) = crate::runtime::AgentControl::with_root_control(
            crate::session::SessionId::new(),
            2,
            root_control.clone(),
        )
        .expect("agent tree");
        let (_, sibling_execution) = tree
            .register_child(
                &crate::runtime::AgentPath::root(),
                "sibling",
                crate::session::SessionId::new(),
                None,
            )
            .expect("sibling");
        let sibling_control = sibling_execution.run_control();
        let root_effect = root_control
            .begin_tool_effect_admission()
            .expect("root effect reservation");
        let failure = crate::runtime::RunCancellationCause::Failure(
            "heartbeat failed during root effect admission".to_string(),
        );

        super::record_heartbeat_failure(
            &root_control,
            "heartbeat failed during root effect admission".to_string(),
        );

        assert_eq!(root_control.cause(), None);
        assert_eq!(sibling_control.cause(), Some(failure.clone()));
        assert!(tree.tree_is_cancelled());
        assert!(sibling_control.begin_tool_effect_admission().is_none());

        assert_eq!(root_effect.admit(), Err(failure.clone()));
        assert_eq!(root_control.cause(), Some(failure));
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
        let turn_id = crate::protocol::TurnId::new();
        let admission_id = store
            .session_repo()
            .admit_session_turn_at(session.id, turn_id, admitted_at_ms, 3_000)
            .await
            .expect("admission")
            .expect("admitted");

        let run_control = crate::runtime::RunControl::new();
        let heartbeat_stop = tokio_util::sync::CancellationToken::new();
        let heartbeat_repo = store.session_repo();
        let heartbeat_admission_id = admission_id.clone();
        let heartbeat_task = super::spawn_run_admission_heartbeat(
            session.id,
            admission_id,
            run_control.clone(),
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
        assert!(!run_control.is_cancelled());
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
        let run_control = crate::runtime::RunControl::new();
        let heartbeat_stop = tokio_util::sync::CancellationToken::new();
        let heartbeat_task = super::spawn_run_admission_heartbeat(
            crate::session::SessionId::new(),
            "blocked-permission-admission".to_string(),
            run_control.clone(),
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
        assert!(!run_control.is_cancelled());
        heartbeat_stop.cancel();
        heartbeat_task
            .await
            .expect("heartbeat task")
            .expect("heartbeat result");
    }

    #[tokio::test]
    async fn durable_interrupt_cancels_a_foreground_permission_wait() {
        let (store, session_id, admission_id, turn_id) =
            heartbeat_active_turn_fixture("external interrupt heartbeat").await;
        let interrupted = terminal_event(
            session_id,
            crate::protocol::TurnTerminalStatus::Interrupted,
            "external stop",
            Some(crate::protocol::TurnInterruptionCause::UserStop),
        );
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    &admission_id,
                    &interrupted,
                    turn_id,
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .expect("external interrupt"),
            crate::storage::session_repo::AdmittedTerminalCommit::Applied
        );
        let run_control = crate::runtime::RunControl::new();

        assert_eq!(
            super::renew_admitted_run_lease_with_terminal_cancel(
                store.session_repo(),
                store.protocol_event_store(),
                session_id,
                admission_id,
                turn_id,
                run_control.clone(),
                None,
            )
            .await
            .expect("terminal renewal"),
            crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome::GracefulTerminal
        );
        assert!(run_control.is_cancelled());
        assert_eq!(
            run_control.cause(),
            Some(crate::runtime::RunCancellationCause::Superseded)
        );
    }

    #[tokio::test]
    async fn own_completed_turn_does_not_cancel_after_terminal_heartbeat_race() {
        let (store, session_id, admission_id, turn_id) =
            heartbeat_active_turn_fixture("own completed heartbeat").await;
        assert_eq!(
            commit_completed_turn(&store, session_id, &admission_id, turn_id).await,
            crate::storage::session_repo::AdmittedTerminalCommit::Applied
        );
        let run_control = crate::runtime::RunControl::new();

        assert_eq!(
            super::renew_admitted_run_lease_with_terminal_cancel(
                store.session_repo(),
                store.protocol_event_store(),
                session_id,
                admission_id,
                turn_id,
                run_control.clone(),
                None,
            )
            .await
            .expect("terminal renewal"),
            crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome::GracefulTerminal
        );
        assert!(!run_control.is_cancelled());
    }

    #[tokio::test]
    async fn terminal_commit_wins_the_heartbeat_barrier_without_reversing_success() {
        let (store, session_id, admission_id, turn_id) =
            heartbeat_active_turn_fixture("terminal heartbeat barrier").await;
        let renewal_entered = Arc::new(tokio::sync::Notify::new());
        let allow_renewal = Arc::new(tokio::sync::Notify::new());
        let heartbeat_repo = store.session_repo();
        let heartbeat_admission_id = admission_id.clone();
        let heartbeat_renewal_entered = Arc::clone(&renewal_entered);
        let heartbeat_allow_renewal = Arc::clone(&allow_renewal);
        let run_control = crate::runtime::RunControl::new();
        let heartbeat_task = super::spawn_run_admission_heartbeat(
            session_id,
            admission_id.clone(),
            run_control.clone(),
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

        let completed_event = terminal_event(
            session_id,
            crate::protocol::TurnTerminalStatus::Completed,
            "completed",
            None,
        );
        assert_eq!(
            store
                .session_repo()
                .terminalize_admitted_turn_with_protocol_event(
                    session_id,
                    &admission_id,
                    &completed_event,
                    turn_id,
                    None,
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
        assert!(!run_control.is_cancelled());
        let completed = super::finish_admitted_run(
            &store,
            session_id,
            &admission_id,
            turn_id,
            &run_control,
            Ok(successful_run_summary(session_id)),
            heartbeat_result,
        )
        .await
        .expect("graceful heartbeat must not reverse terminal success");
        assert_eq!(completed.status, crate::session::SessionStatus::Completed);
        assert!(run_control.success_is_sealed());
        assert!(!run_control.interrupt(crate::protocol::TurnInterruptionCause::UserStop));
        assert!(!run_control.is_cancelled());
    }

    #[tokio::test]
    async fn completed_commit_after_heartbeat_error_remains_the_durable_authority() {
        let (store, session_id, admission_id, turn_id) =
            heartbeat_active_turn_fixture("heartbeat error before completed commit").await;
        let renewal_entered = Arc::new(tokio::sync::Notify::new());
        let release_error = Arc::new(tokio::sync::Notify::new());
        let heartbeat_renewal_entered = Arc::clone(&renewal_entered);
        let heartbeat_release_error = Arc::clone(&release_error);
        let run_control = crate::runtime::RunControl::new();
        let heartbeat_task = super::spawn_run_admission_heartbeat(
            session_id,
            admission_id.clone(),
            run_control.clone(),
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
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            run_control.token().cancelled_owned(),
        )
        .await
        .expect("heartbeat error did not cancel the run");
        let heartbeat_result = heartbeat_task.await.expect("heartbeat task");

        assert_eq!(
            commit_completed_turn(&store, session_id, &admission_id, turn_id).await,
            crate::storage::session_repo::AdmittedTerminalCommit::Applied
        );
        assert_eq!(
            store
                .session_repo()
                .durable_terminal_for_turn(session_id, turn_id)
                .await
                .expect("durable terminal truth")
                .map(|terminal| terminal.status.as_session_status()),
            Some(crate::session::SessionStatus::Completed)
        );
        let completed = super::finish_admitted_run(
            &store,
            session_id,
            &admission_id,
            turn_id,
            &run_control,
            Ok(successful_run_summary(session_id)),
            heartbeat_result,
        )
        .await
        .expect("durable completion must override the heartbeat diagnostic");
        assert_eq!(completed.status, crate::session::SessionStatus::Completed);
    }

    #[tokio::test]
    async fn completed_commit_before_heartbeat_panic_remains_the_durable_authority() {
        let (store, session_id, admission_id, turn_id) =
            heartbeat_active_turn_fixture("completed commit before heartbeat panic").await;
        let renewal_entered = Arc::new(tokio::sync::Notify::new());
        let release_panic = Arc::new(tokio::sync::Notify::new());
        let heartbeat_renewal_entered = Arc::clone(&renewal_entered);
        let heartbeat_release_panic = Arc::clone(&release_panic);
        let run_control = crate::runtime::RunControl::new();
        let heartbeat_task = super::spawn_run_admission_heartbeat(
            session_id,
            admission_id.clone(),
            run_control.clone(),
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
            commit_completed_turn(&store, session_id, &admission_id, turn_id).await,
            crate::storage::session_repo::AdmittedTerminalCommit::Applied
        );
        release_panic.notify_one();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            run_control.token().cancelled_owned(),
        )
        .await
        .expect("heartbeat panic did not cancel the run");
        let heartbeat_result = heartbeat_task.await.expect("heartbeat task");

        let completed = super::finish_admitted_run(
            &store,
            session_id,
            &admission_id,
            turn_id,
            &run_control,
            Ok(successful_run_summary(session_id)),
            heartbeat_result,
        )
        .await
        .expect("durable completion must override the heartbeat panic diagnostic");
        assert_eq!(completed.status, crate::session::SessionStatus::Completed);
    }

    #[tokio::test]
    async fn heartbeat_error_finished_before_terminal_commit_settles_as_failure() {
        let (store, session_id, admission_id, turn_id) =
            heartbeat_active_turn_fixture("heartbeat storage error").await;
        let run_control = crate::runtime::RunControl::new();
        let heartbeat_task = super::spawn_run_admission_heartbeat(
            session_id,
            admission_id.clone(),
            run_control.clone(),
            tokio_util::sync::CancellationToken::new(),
            std::time::Duration::from_millis(1),
            || {
                std::future::ready(Err(crate::error::StorageError::Message(
                    "injected heartbeat storage error".to_string(),
                )))
            },
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            run_control.token().cancelled_owned(),
        )
        .await
        .expect("storage failure did not cancel the run");
        let heartbeat_result = heartbeat_task.await.expect("heartbeat task");
        let failure = super::finish_admitted_run(
            &store,
            session_id,
            &admission_id,
            turn_id,
            &run_control,
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
        assert!(matches!(
            run_control.cause(),
            Some(crate::runtime::RunCancellationCause::Failure(message))
                if message.contains("injected heartbeat storage error")
        ));
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
                .durable_terminal_for_turn(session_id, turn_id)
                .await
                .expect("failed terminal truth")
                .map(|terminal| terminal.status.as_session_status()),
            Some(crate::session::SessionStatus::Failed)
        );
        assert_eq!(
            commit_completed_turn(&store, session_id, &admission_id, turn_id).await,
            crate::storage::session_repo::AdmittedTerminalCommit::NotOwned
        );
        assert!(
            store
                .session_repo()
                .admit_session_turn(session_id, crate::protocol::TurnId::new())
                .await
                .expect("readmission")
                .is_some()
        );
    }

    #[tokio::test]
    async fn heartbeat_panic_cancels_and_still_settles_and_releases() {
        let (store, session_id, admission_id, turn_id) =
            heartbeat_active_turn_fixture("heartbeat panic").await;
        let run_control = crate::runtime::RunControl::new();
        let heartbeat_task = super::spawn_run_admission_heartbeat(
            session_id,
            admission_id.clone(),
            run_control.clone(),
            tokio_util::sync::CancellationToken::new(),
            std::time::Duration::from_millis(1),
            panicking_heartbeat_renewal,
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            run_control.token().cancelled_owned(),
        )
        .await
        .expect("heartbeat panic did not cancel the run");
        let heartbeat_result = heartbeat_task.await.expect("heartbeat task");
        let failure = super::finish_admitted_run(
            &store,
            session_id,
            &admission_id,
            turn_id,
            &run_control,
            Ok(successful_run_summary(session_id)),
            heartbeat_result,
        )
        .await
        .expect_err("heartbeat panic must fail the run");

        assert!(failure.to_string().contains("injected heartbeat panic"));
        assert!(matches!(
            run_control.cause(),
            Some(crate::runtime::RunCancellationCause::Failure(message))
                if message.contains("injected heartbeat panic")
        ));
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
                .admit_session_turn(session_id, crate::protocol::TurnId::new())
                .await
                .expect("readmission")
                .is_some()
        );
    }
}
